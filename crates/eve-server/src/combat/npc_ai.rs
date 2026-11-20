//! `npc_ai` — Belt/gate NPC spawns from SDE spawn groups, NPC AI state machine (Idle -> Chase -> Orbit -> Attack -> Reps), and dynamic bounty payout (`C-4`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_sde::{AttributeId, EffectId, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// AI state machine for spawned NPC combat ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpcAiState {
    Idle,
    Chase,
    Orbit,
    Attack,
}

/// Computes faction-specific damage profile (`feat:npc.npc-damage-types`).
pub fn faction_damage_vector(faction_id: u32, total_dps: f64) -> DamageVector {
    match faction_id {
        500010 | 500001 => {
            // Guristas / Caldari: 70% Kinetic, 30% Thermal
            DamageVector::new(0.0, total_dps * 0.30, total_dps * 0.70, 0.0)
        }
        500012 | 500019 | 500003 => {
            // Blood Raiders / Sansha's Nation / Amarr: 60% EM, 40% Thermal
            DamageVector::new(total_dps * 0.60, total_dps * 0.40, 0.0, 0.0)
        }
        500020 | 500004 => {
            // Serpentis / Gallente: 60% Thermal, 40% Kinetic
            DamageVector::new(0.0, total_dps * 0.60, total_dps * 0.40, 0.0)
        }
        500011 | 500002 => {
            // Angel Cartel / Minmatar: 60% Explosive, 30% Kinetic, 10% Thermal
            DamageVector::new(0.0, total_dps * 0.10, total_dps * 0.30, total_dps * 0.60)
        }
        _ => {
            let q = total_dps * 0.25;
            DamageVector::new(q, q, q, q)
        }
    }
}

/// Resolves the regional pirate faction ID for a solar system (`feat:npc.belt-rats`, `feat:npc.clone-soldier-spawns`).
// DERIVED: Region-to-pirate-faction table mapped from EVE regional pirate rat distribution
// (Guristas, Blood Raiders, Sansha's Nation, Serpentis, Angel Cartel); client FSD static data
// does not ship server-side regional pirate spawn tables.
pub fn regional_pirate_faction_id(ctx: &ServerContext, solar_system_id: u32) -> u32 {
    let Some(sys) = ctx.sde.solar_systems.get(&solar_system_id) else {
        return 500_010;
    };
    let sde_faction = ctx
        .sde
        .constellations
        .get(&sys.constellation_id)
        .and_then(|c| c.faction_id)
        .or_else(|| ctx.sde.regions.get(&sys.region_id).and_then(|r| r.faction_id));
    if let Some(fid) = sde_faction {
        match fid {
            500_010 | 500_001 => return 500_010, // Guristas / Caldari State
            500_011 | 500_002 | 500_015 => return 500_011, // Angel Cartel / Minmatar / Thukker
            500_012 | 500_003 | 500_008 => return 500_012, // Blood Raiders / Amarr / Khanid
            500_019 | 500_007 => return 500_019, // Sansha's Nation / Ammatar Mandate
            500_020 | 500_004 | 500_009 | 500_014 => return 500_020, // Serpentis / Gallente / Syndicate / ORE
            _ => {}
        }
    }
    match sys.region_id {
        10_000_002 | 10_000_003 | 10_000_010 | 10_000_015 | 10_000_016
        | 10_000_023 | 10_000_033 | 10_000_051 | 10_000_069 => 500_010, // Guristas
        10_000_020 | 10_000_036 | 10_000_038 | 10_000_043 | 10_000_052 | 10_000_054
        | 10_000_065 | 10_000_067 => 500_012, // Blood Raiders
        10_000_001 | 10_000_031 | 10_000_037 | 10_000_039 | 10_000_047 | 10_000_050
        | 10_000_056 | 10_000_063 => 500_019, // Sansha's Nation
        10_000_027 | 10_000_032 | 10_000_035 | 10_000_041 | 10_000_044 | 10_000_046
        | 10_000_048 | 10_000_055 | 10_000_057 | 10_000_058 | 10_000_059 | 10_000_060
        | 10_000_064 | 10_000_068 => 500_020, // Serpentis
        10_000_005 | 10_000_006 | 10_000_007 | 10_000_008 | 10_000_009 | 10_000_011
        | 10_000_012 | 10_000_014 | 10_000_025 | 10_000_028 | 10_000_030 | 10_000_042 => 500_011, // Angel Cartel
        _ => 500_010,
    }
}

/// Returns the Clone Soldier tier name for a low-sec system security status (`0.05..0.45`), or `None` outside low-sec (`feat:npc.clone-soldier-spawns`).
pub fn clone_soldier_tier_for_security(sys_sec: f64) -> Option<&'static str> {
    if !(0.05..0.45).contains(&sys_sec) {
        return None;
    }
    if sys_sec >= 0.35 {
        Some("Trainer")
    } else if sys_sec >= 0.25 {
        Some("Recruiter")
    } else if sys_sec >= 0.15 {
        Some("Transporter")
    } else {
        Some("Negotiator")
    }
}

/// Returns the Clone Soldier NPC `type_id` for a low-sec system security status (`0.05..0.45`), or `None` outside low-sec (`feat:npc.clone-soldier-spawns`).
pub fn clone_soldier_type_for_security(sys_sec: f64) -> Option<u32> {
    match clone_soldier_tier_for_security(sys_sec)? {
        "Trainer" => Some(33132),     // Clone Soldier Trainer (0.4) -> Tag 33138
        "Recruiter" => Some(33142),   // Clone Soldier Recruiter (0.3) -> Tag 33139
        "Transporter" => Some(33163), // Clone Soldier Transporter (0.2) -> Tag 33140
        _ => Some(33175),             // Clone Soldier Negotiator (0.1) -> Tag 33141
    }
}

/// Resolves the regional pirate faction's Clone Soldier NPC `type_id` from `sde.types`
/// matching the system's low-sec security band (`0.1`..=`0.4`) and pirate faction (`feat:npc.clone-soldier-spawns`).
pub fn clone_soldier_type_for_security_and_faction(
    sde: &eve_sde::SdeRegistry,
    sys_sec: f64,
    faction_id: u32,
) -> Option<u32> {
    let tier = clone_soldier_tier_for_security(sys_sec)?;
    let target_group_id: u32 = match faction_id {
        500_011 => 551, // Asteroid Angel Cartel BattleCruiser / Clone Soldiers
        500_012 => 552, // Asteroid Blood Raiders BattleCruiser / Clone Soldiers
        500_010 => 556, // Asteroid Guristas BattleCruiser / Clone Soldiers
        500_019 => 566, // Asteroid Sansha's Nation BattleCruiser / Clone Soldiers
        500_020 => 574, // Asteroid Serpentis BattleCruiser / Clone Soldiers
        _ => 556,
    };
    if sde.types.len() > 1_000 {
        static CLONE_SOLDIER_CATALOG_BY_LEN: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<(usize, usize), Vec<(u32, u32, String)>>>,
        > = std::sync::OnceLock::new();
        let key = (
            sde.types.len(),
            (33_130u32..=33_250)
                .filter(|id| sde.types.contains_key(id))
                .count(),
        );
        let map = CLONE_SOLDIER_CATALOG_BY_LEN
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        let catalog = guard.entry(key).or_insert_with(|| {
            let mut rows: Vec<(u32, u32, String)> = sde
                .types
                .values()
                .filter(|t| {
                    matches!(t.group_id, 551 | 552 | 556 | 566 | 574)
                        && t.name.contains("Clone Soldier")
                })
                .map(|t| (t.type_id, t.group_id, t.name.clone()))
                .collect();
            rows.sort_unstable_by_key(|r| r.0);
            rows
        });
        if let Some(&(tid, _, _)) = catalog
            .iter()
            .find(|&&(_, gid, ref name)| gid == target_group_id && name.contains(tier))
        {
            return Some(tid);
        }
        return clone_soldier_type_for_security(sys_sec);
    }
    let mut matches: Vec<u32> = sde
        .types
        .values()
        .filter(|t| {
            t.group_id == target_group_id
                && t.name.contains("Clone Soldier")
                && t.name.contains(tier)
        })
        .map(|t| t.type_id)
        .collect();
    matches.sort_unstable();
    matches
        .into_iter()
        .next()
        .or_else(|| clone_soldier_type_for_security(sys_sec))
}

/// Computes character security status gain from destroying an NPC pirate (`feat:crimewatch.sec-status-gain`).
pub fn compute_sec_status_gain(current_sec: f64, bounty: f64) -> f64 {
    if current_sec >= 5.0 || bounty <= 0.0 {
        return 0.0;
    }
    let headroom = (5.0 - current_sec).max(0.0);
    let scale = (bounty / 500_000.0).clamp(0.01, 10.0);
    (headroom * 0.01 * scale).clamp(0.001, 0.5).min(headroom)
}

/// In-memory runtime tracking for an active NPC combat entity in a solar system.
#[derive(Debug, Clone)]
pub struct NpcCombatEntity {
    pub ball_id: i64,
    pub type_id: u32,
    pub faction_id: u32,
    pub state: NpcAiState,
    pub spawn_group_id: u32,
    pub spawn_pos: DVec3,
    pub target_id: Option<i64>,
    pub bounty_isk: f64,
    pub optimal_range: f64,
    pub orbit_range: f64,
    pub base_dps: f64,
    pub damage_vector: DamageVector,
    pub last_attack_tick: u64,
    pub has_shield_rep: bool,
    pub has_armor_rep: bool,
}

/// System-level NPC spawner and AI director.
#[derive(Debug, Default)]
pub struct NpcSpawnManager {
    pub entities: HashMap<i64, NpcCombatEntity>,
    /// Tracks cleared spawn groups and their respawn cooldown timestamp: `(system_id, spawn_group_id) -> respawn_after_unix`.
    pub respawn_timers: HashMap<(u32, u32), u64>,
    /// Tracks Clone Soldier respawn cooldown per low-sec solar system: `system_id -> respawn_after_unix`.
    pub clone_soldier_respawn_timers: HashMap<u32, u64>,
    /// Per-entity SDE combat profile & sub-tick weapon cooldown state (`#1396`).
    runtime: HashMap<i64, NpcRuntimeCombatState>,
}

const ATTR_GFX_TURRET_ID: AttributeId = 245;
const ATTR_ENTITY_FLY_RANGE: AttributeId = 416;
const ATTR_MISSILE_DAMAGE_MULT: AttributeId = 212;
const ATTR_MISSILE_EXP_DELAY_MS: AttributeId = 281;
const ATTR_MISSILE_LAUNCH_DURATION_MS: AttributeId = 506;
const ATTR_ENTITY_MISSILE_TYPE_ID: AttributeId = 507;
const ATTR_ENTITY_CRUISE_SPEED: AttributeId = 508;
const ATTR_OPTIMAL_SIG_RADIUS: AttributeId = 620;
const ATTR_MISSILE_VELOCITY_MULT: AttributeId = 645;
const ATTR_MISSILE_FLIGHT_TIME_MULT: AttributeId = 646;
const ATTR_MISSILE_AOE_VELOCITY: AttributeId = 653;
const ATTR_MISSILE_AOE_CLOUD_SIZE: AttributeId = 654;
const ATTR_MISSILE_EXP_RADIUS_MULT: AttributeId = 858;
const ATTR_MISSILE_EXP_VELOCITY_MULT: AttributeId = 859;
const ATTR_MISSILE_NEVER_DOES_DAMAGE: AttributeId = 1075;
const ATTR_MISSILE_AOE_DRF: AttributeId = 1353;
const ATTR_NPC_MAX_COMBAT_ORBIT_RANGE: AttributeId = 2786;

const EFF_TARGET_ATTACK: EffectId = 10;
const EFF_MISSILE_LAUNCHING_FOR_ENTITY: EffectId = 569;
const EFF_TARGET_DISINTEGRATOR_ATTACK: EffectId = 6995;
const EFF_CHAIN_LIGHTNING: EffectId = 8037;
const EFF_ENTITY_CHAIN_LIGHTNING: EffectId = 8088;

static LOGGED_FALLBACK_NPC_TYPES: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn fallback_log_registry() -> &'static Mutex<HashSet<u32>> {
    LOGGED_FALLBACK_NPC_TYPES.get_or_init(|| Mutex::new(HashSet::new()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NpcWeaponKind {
    Laser,
    Hybrid,
    Projectile,
    Missile,
    TriglavianBeam,
    Vorton,
}

impl NpcWeaponKind {
    pub(crate) fn fx_guid(self) -> &'static str {
        match self {
            Self::Laser => "effects.Laser",
            Self::Hybrid => "effects.HybridFired",
            Self::Projectile => "effects.ProjectileFired",
            Self::Missile => "effects.MissileDeployment",
            Self::TriglavianBeam => "effects.TriglavianBeam",
            Self::Vorton => "effects.AttackMode",
        }
    }
}

/// Resolves an NPC weapon's `OnSpecialFX` GUID using SDE effect metadata (`sde.effects.guid`)
/// when available, while preserving turret graphic overrides (`Hybrid`/`Projectile`) where
/// SDE category-11 types tag generic `targetAttack(10)` alongside a Hybrid/Projectile turret gfx.
fn resolve_npc_weapon_fx_guid(
    sde: &SdeRegistry,
    npc_type: &eve_sde::InvType,
    kind: NpcWeaponKind,
    gfx_group_id: Option<u32>,
    is_missile: bool,
) -> String {
    let effect_guid = |eid: EffectId| -> Option<String> {
        let eff = sde.effects.get(&eid)?;
        let raw_guid = eff.guid.as_deref()?;
        raw_guid
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| s.starts_with("effects."))
            .map(str::to_string)
    };

    if is_missile {
        if npc_type.effect_ids.contains(&EFF_MISSILE_LAUNCHING_FOR_ENTITY) {
            if let Some(g) = effect_guid(EFF_MISSILE_LAUNCHING_FOR_ENTITY) {
                return g;
            }
        }
        return NpcWeaponKind::Missile.fx_guid().to_string();
    }

    match kind {
        NpcWeaponKind::Hybrid => {
            if gfx_group_id != Some(74) {
                if let Some(g) = npc_type
                    .default_effect_id
                    .and_then(effect_guid)
                    .filter(|g| g != "effects.Laser")
                {
                    return g;
                }
            }
            return NpcWeaponKind::Hybrid.fx_guid().to_string();
        }
        NpcWeaponKind::Projectile => {
            if gfx_group_id != Some(55) {
                if let Some(g) = npc_type
                    .default_effect_id
                    .and_then(effect_guid)
                    .filter(|g| g != "effects.Laser")
                {
                    return g;
                }
            }
            return NpcWeaponKind::Projectile.fx_guid().to_string();
        }
        NpcWeaponKind::TriglavianBeam => {
            if npc_type.effect_ids.contains(&EFF_TARGET_DISINTEGRATOR_ATTACK) {
                if let Some(g) = effect_guid(EFF_TARGET_DISINTEGRATOR_ATTACK) {
                    return g;
                }
            }
        }
        NpcWeaponKind::Vorton => {
            for &eid in &[EFF_ENTITY_CHAIN_LIGHTNING, EFF_CHAIN_LIGHTNING] {
                if npc_type.effect_ids.contains(&eid) {
                    if let Some(g) = effect_guid(eid) {
                        return g;
                    }
                }
            }
        }
        NpcWeaponKind::Laser => {
            if npc_type.effect_ids.contains(&EFF_TARGET_ATTACK) {
                if let Some(g) = effect_guid(EFF_TARGET_ATTACK) {
                    return g;
                }
            }
            if let Some(g) = npc_type.default_effect_id.and_then(effect_guid) {
                return g;
            }
        }
        NpcWeaponKind::Missile => {}
    }

    kind.fx_guid().to_string()
}

#[derive(Debug, Clone)]
pub(crate) struct NpcTurretProfile {
    pub kind: NpcWeaponKind,
    pub fx_guid: String,
    pub gfx_type_id: Option<u32>,
    pub shot_damage: DamageVector,
    pub cycle_duration_s: f64,
    pub optimal_range: f64,
    pub falloff_range: f64,
    pub tracking_speed: f64,
    pub signature_resolution: f64,
    pub vorton_explosion_radius: f64,
    pub vorton_explosion_velocity: f64,
    pub vorton_drf: f64,
}

impl NpcTurretProfile {
    pub(crate) fn max_effective_range(&self) -> f64 {
        if self.kind == NpcWeaponKind::Vorton {
            // Vorton Projectors apply only within optimal range (`#1247`, `combat_weapons.rs:1539`).
            self.optimal_range
        } else if self.falloff_range > 0.0 {
            self.optimal_range + 3.0 * self.falloff_range
        } else {
            self.optimal_range
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NpcMissileProfile {
    pub missile_type_id: u32,
    pub fx_guid: String,
    pub volley_damage: DamageVector,
    pub cycle_duration_s: f64,
    pub max_flight_range: f64,
    pub explosion_radius: f64,
    pub explosion_velocity: f64,
    pub damage_reduction_factor: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct NpcCombatProfile {
    pub bounty_isk: f64,
    pub max_velocity: f64,
    pub cruise_velocity: f64,
    pub orbit_range: f64,
    pub engagement_optimal_range: f64,
    pub total_nominal_dps: f64,
    pub nominal_damage_vector: DamageVector,
    pub turret: Option<NpcTurretProfile>,
    pub missile: Option<NpcMissileProfile>,
    pub is_synthetic_fallback: bool,
}

#[derive(Debug, Clone)]
struct NpcRuntimeCombatState {
    profile: NpcCombatProfile,
    uses_synthetic_legacy: bool,
    baseline_damage_vector: DamageVector,
    baseline_optimal_range: f64,
    turret_cooldown_s: f64,
    missile_cooldown_s: f64,
}

fn damage_vector_approx_eq(a: DamageVector, b: DamageVector) -> bool {
    (a.em - b.em).abs() < 1e-6
        && (a.thermal - b.thermal).abs() < 1e-6
        && (a.kinetic - b.kinetic).abs() < 1e-6
        && (a.explosive - b.explosive).abs() < 1e-6
}

fn sum_damage_vectors(a: DamageVector, b: DamageVector) -> DamageVector {
    DamageVector::new(
        a.em + b.em,
        a.thermal + b.thermal,
        a.kinetic + b.kinetic,
        a.explosive + b.explosive,
    )
}

fn infer_turret_kind_from_damage(dmg: DamageVector) -> NpcWeaponKind {
    if dmg.explosive > 0.0 && dmg.em <= 0.0 && (dmg.kinetic > 0.0 || dmg.thermal <= 0.0) {
        NpcWeaponKind::Projectile
    } else if dmg.em > 0.0 && dmg.kinetic <= 0.0 && dmg.explosive <= 0.0 {
        NpcWeaponKind::Laser
    } else if dmg.kinetic > 0.0 && dmg.thermal > 0.0 && dmg.em <= 0.0 && dmg.explosive <= 0.0 {
        NpcWeaponKind::Hybrid
    } else {
        NpcWeaponKind::Laser
    }
}

fn resolve_turret_kind_and_gfx(
    sde: &SdeRegistry,
    npc_type: &eve_sde::InvType,
    shot_damage: DamageVector,
    fallbacks: &mut Vec<&'static str>,
) -> (NpcWeaponKind, Option<u32>, Option<u32>) {
    let gfx_type_id = npc_type
        .default_attributes
        .get(&ATTR_GFX_TURRET_ID)
        .copied()
        .filter(|&v| v > 0.0)
        .map(|v| v as u32);
    let gfx_group_id = gfx_type_id.and_then(|id| sde.types.get(&id).map(|g| g.group_id));

    if npc_type.effect_ids.contains(&EFF_TARGET_DISINTEGRATOR_ATTACK) {
        return (NpcWeaponKind::TriglavianBeam, gfx_type_id, gfx_group_id);
    }
    if npc_type.effect_ids.contains(&EFF_ENTITY_CHAIN_LIGHTNING)
        || npc_type.effect_ids.contains(&EFF_CHAIN_LIGHTNING)
    {
        return (NpcWeaponKind::Vorton, gfx_type_id, gfx_group_id);
    }
    if let Some(group_id) = gfx_group_id {
        match group_id {
            1986 => return (NpcWeaponKind::TriglavianBeam, gfx_type_id, gfx_group_id),
            4060 => return (NpcWeaponKind::Vorton, gfx_type_id, gfx_group_id),
            53 | 54 | 464 => return (NpcWeaponKind::Laser, gfx_type_id, gfx_group_id),
            74 => return (NpcWeaponKind::Hybrid, gfx_type_id, gfx_group_id),
            55 => return (NpcWeaponKind::Projectile, gfx_type_id, gfx_group_id),
            _ => {}
        }
    }
    fallbacks.push("gfxTurretID(245)");
    (
        infer_turret_kind_from_damage(shot_damage),
        gfx_type_id,
        gfx_group_id,
    )
}

fn build_npc_turret_profile(
    sde: &SdeRegistry,
    npc_type: &eve_sde::InvType,
    fallbacks: &mut Vec<&'static str>,
) -> Option<NpcTurretProfile> {
    let a = &npc_type.default_attributes;
    let em = a.get(&eve_sde::attr_ids::EM_DAMAGE).copied().unwrap_or(0.0);
    let th = a.get(&eve_sde::attr_ids::THERMAL_DAMAGE).copied().unwrap_or(0.0);
    let kin = a.get(&eve_sde::attr_ids::KINETIC_DAMAGE).copied().unwrap_or(0.0);
    let exp = a.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE).copied().unwrap_or(0.0);
    if em + th + kin + exp <= 0.0 {
        return None;
    }
    let dmg_mult = match a.get(&eve_sde::attr_ids::DAMAGE_MULTIPLIER).copied() {
        Some(m) if m <= 0.0 => return None,
        Some(m) => m,
        None => {
            fallbacks.push("damageMultiplier(64)");
            1.0
        }
    };
    let shot_damage = DamageVector::new(em * dmg_mult, th * dmg_mult, kin * dmg_mult, exp * dmg_mult);
    let cycle_ms = if let Some(ms) = a.get(&eve_sde::attr_ids::SPEED).copied().filter(|&v| v > 0.0) {
        ms
    } else if let Some(ms) = a.get(&eve_sde::attr_ids::DURATION).copied().filter(|&v| v > 0.0) {
        ms
    } else {
        fallbacks.push("turretRateOfFire(51/73)");
        3_000.0
    };
    let cycle_duration_s = (cycle_ms / 1000.0).max(0.5);

    let opt_attr = a.get(&eve_sde::attr_ids::MAX_RANGE).copied();
    let fall_attr = a.get(&eve_sde::attr_ids::FALLOFF).copied();
    let falloff_range = if let Some(f) = fall_attr.filter(|&v| v >= 0.0) {
        f
    } else {
        fallbacks.push("falloff(158)");
        1_500.0
    };
    let optimal_range = if let Some(opt) = opt_attr.filter(|&v| v > 0.0) {
        opt
    } else if opt_attr == Some(0.0) && falloff_range > 0.0 {
        0.0
    } else {
        fallbacks.push("maxRange(54)");
        5_000.0
    };

    let (kind, gfx_type_id, gfx_group_id) =
        resolve_turret_kind_and_gfx(sde, npc_type, shot_damage, fallbacks);
    let raw_trk = a.get(&eve_sde::attr_ids::TRACKING_SPEED).copied().filter(|&v| v > 0.0);
    let (
        signature_resolution,
        tracking_speed,
        vorton_explosion_radius,
        vorton_explosion_velocity,
        vorton_drf,
    ) = if kind == NpcWeaponKind::Vorton {
        let er = if let Some(v) = a.get(&ATTR_MISSILE_AOE_CLOUD_SIZE).copied().filter(|&v| v > 0.0) {
            v
        } else {
            fallbacks.push("aoeCloudSize(654)");
            43.0
        };
        let ev = if let Some(v) = a.get(&ATTR_MISSILE_AOE_VELOCITY).copied().filter(|&v| v > 0.0) {
            v
        } else {
            fallbacks.push("aoeVelocity(653)");
            190.0
        };
        let drf = if let Some(v) = a.get(&ATTR_MISSILE_AOE_DRF).copied().filter(|&v| v > 0.0) {
            v
        } else {
            fallbacks.push("aoeDamageReductionFactor(1353)");
            0.40
        };
        let sr = a
            .get(&ATTR_OPTIMAL_SIG_RADIUS)
            .copied()
            .filter(|&v| v > 0.0)
            .unwrap_or(40_000.0);
        (sr, raw_trk.unwrap_or(1.0), er, ev, drf)
    } else {
        let signature_resolution = if let Some(sr) = a
            .get(&ATTR_OPTIMAL_SIG_RADIUS)
            .copied()
            .filter(|&v| v > 0.0)
        {
            sr
        } else {
            fallbacks.push("optimalSigRadius(620)");
            if raw_trk.is_some_and(|trk| trk > 2.0) {
                40_000.0
            } else {
                40.0
            }
        };
        let tracking_speed = if let Some(trk) = raw_trk {
            trk
        } else {
            fallbacks.push("trackingSpeed(160)");
            if signature_resolution >= 10_000.0 {
                1.0
            } else {
                0.05
            }
        };
        (signature_resolution, tracking_speed, 0.0, 0.0, 0.0)
    };
    let fx_guid = resolve_npc_weapon_fx_guid(sde, npc_type, kind, gfx_group_id, false);
    Some(NpcTurretProfile {
        kind,
        fx_guid,
        gfx_type_id,
        shot_damage,
        cycle_duration_s,
        optimal_range,
        falloff_range,
        tracking_speed,
        signature_resolution,
        vorton_explosion_radius,
        vorton_explosion_velocity,
        vorton_drf,
    })
}

fn build_npc_missile_profile(
    sde: &SdeRegistry,
    npc_type: &eve_sde::InvType,
    fallbacks: &mut Vec<&'static str>,
) -> Option<NpcMissileProfile> {
    let a = &npc_type.default_attributes;
    let missile_type_id = a
        .get(&ATTR_ENTITY_MISSILE_TYPE_ID)
        .copied()
        .filter(|&v| v > 0.0)
        .map(|v| v as u32)?;
    let launch_dur_opt = a.get(&ATTR_MISSILE_LAUNCH_DURATION_MS).copied();
    let has_missile_effect = npc_type.effect_ids.contains(&EFF_MISSILE_LAUNCHING_FOR_ENTITY);
    if !has_missile_effect && launch_dur_opt.unwrap_or(0.0) <= 100.0 {
        return None;
    }
    let Some(m_type) = sde.types.get(&missile_type_id) else {
        fallbacks.push("entityMissileTypeID(507)");
        return None;
    };
    let ma = &m_type.default_attributes;
    if ma.get(&ATTR_MISSILE_NEVER_DOES_DAMAGE).is_some_and(|&v| v > 0.0) {
        return None;
    }
    let dmg_mult = match a.get(&ATTR_MISSILE_DAMAGE_MULT).copied() {
        Some(m) if m <= 0.0 => return None,
        Some(m) => m,
        None => {
            fallbacks.push("missileDamageMultiplier(212)");
            1.0
        }
    };
    let m_em = ma.get(&eve_sde::attr_ids::EM_DAMAGE).copied().unwrap_or(0.0);
    let m_th = ma.get(&eve_sde::attr_ids::THERMAL_DAMAGE).copied().unwrap_or(0.0);
    let m_kin = ma.get(&eve_sde::attr_ids::KINETIC_DAMAGE).copied().unwrap_or(0.0);
    let m_exp = ma.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE).copied().unwrap_or(0.0);
    if m_em + m_th + m_kin + m_exp <= 0.0 {
        return None;
    }
    let volley_damage = DamageVector::new(
        m_em * dmg_mult,
        m_th * dmg_mult,
        m_kin * dmg_mult,
        m_exp * dmg_mult,
    );
    let cycle_ms = if let Some(ms) = launch_dur_opt.filter(|&v| v > 100.0) {
        ms
    } else {
        fallbacks.push("missileLaunchDuration(506)");
        20_000.0
    };
    let cycle_duration_s = (cycle_ms / 1000.0).max(0.5);

    let m_vel = if let Some(v) = ma.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().filter(|&v| v > 0.0) {
        v
    } else {
        fallbacks.push("missileMaxVelocity(37)");
        3_000.0
    };
    let vel_mult = a.get(&ATTR_MISSILE_VELOCITY_MULT).copied().filter(|&v| v > 0.0).unwrap_or(1.0);
    let m_flight_ms = if let Some(ms) = ma.get(&ATTR_MISSILE_EXP_DELAY_MS).copied().filter(|&v| v > 0.0) {
        ms
    } else {
        fallbacks.push("missileFlightTime(281)");
        5_000.0
    };
    let flight_mult = a.get(&ATTR_MISSILE_FLIGHT_TIME_MULT).copied().filter(|&v| v > 0.0).unwrap_or(1.0);
    let max_flight_range = ((m_vel * vel_mult) * ((m_flight_ms / 1000.0) * flight_mult)).max(0.0);

    let m_er = if let Some(er) = ma.get(&ATTR_MISSILE_AOE_CLOUD_SIZE).copied().filter(|&v| v > 0.0) {
        er
    } else {
        fallbacks.push("aoeCloudSize(654)");
        100.0
    };
    let er_mult = a.get(&ATTR_MISSILE_EXP_RADIUS_MULT).copied().filter(|&v| v > 0.0).unwrap_or(1.0);
    let m_ev = if let Some(ev) = ma.get(&ATTR_MISSILE_AOE_VELOCITY).copied().filter(|&v| v > 0.0) {
        ev
    } else {
        fallbacks.push("aoeVelocity(653)");
        100.0
    };
    let ev_mult = a.get(&ATTR_MISSILE_EXP_VELOCITY_MULT).copied().filter(|&v| v > 0.0).unwrap_or(1.0);
    let damage_reduction_factor = if let Some(drf) = ma.get(&ATTR_MISSILE_AOE_DRF).copied().filter(|&v| v > 0.0) {
        drf
    } else {
        fallbacks.push("aoeDamageReductionFactor(1353)");
        0.6
    };

    let fx_guid = resolve_npc_weapon_fx_guid(sde, npc_type, NpcWeaponKind::Missile, None, true);
    Some(NpcMissileProfile {
        missile_type_id,
        fx_guid,
        volley_damage,
        cycle_duration_s,
        max_flight_range,
        explosion_radius: (m_er * er_mult).max(1.0),
        explosion_velocity: (m_ev * ev_mult).max(1.0),
        damage_reduction_factor,
    })
}

/// Pure, side-effect-free SDE profile inspection for an NPC type (`#1396`).
/// Returns the computed combat/movement profile alongside any missing attribute fallback labels.
pub(crate) fn inspect_npc_sde_profile(
    sde: &SdeRegistry,
    type_id: u32,
    bounty_isk_override: Option<f64>,
    faction_id: u32,
) -> (NpcCombatProfile, Vec<&'static str>) {
    let mut fallbacks = Vec::new();
    let Some(t) = sde.types.get(&type_id) else {
        fallbacks.push("type_missing");
        let bounty = bounty_isk_override.unwrap_or(25_000.0);
        let dps = 40.0;
        return (
            NpcCombatProfile {
                bounty_isk: bounty,
                max_velocity: 320.0,
                cruise_velocity: 320.0,
                orbit_range: 5_000.0,
                engagement_optimal_range: 8_000.0,
                total_nominal_dps: dps,
                nominal_damage_vector: faction_damage_vector(faction_id, dps),
                turret: None,
                missile: None,
                is_synthetic_fallback: true,
            },
            fallbacks,
        );
    };
    let a = &t.default_attributes;
    let bounty = bounty_isk_override.unwrap_or_else(|| {
        a.get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
            .copied()
            .unwrap_or(25_000.0)
    });

    if t.category_id != 11 {
        let max_vel = a.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().unwrap_or(320.0);
        let cruise_vel = a.get(&ATTR_ENTITY_CRUISE_SPEED).copied().filter(|&v| v > 0.0).unwrap_or(max_vel);
        let opt = 8_000.0_f64;
        let orb = (opt * 0.75).clamp(2_500.0, 15_000.0);
        let dps = (bounty / 500.0).clamp(20.0, 250.0);
        return (
            NpcCombatProfile {
                bounty_isk: bounty,
                max_velocity: max_vel,
                cruise_velocity: cruise_vel,
                orbit_range: orb,
                engagement_optimal_range: opt,
                total_nominal_dps: dps,
                nominal_damage_vector: faction_damage_vector(faction_id, dps),
                turret: None,
                missile: None,
                is_synthetic_fallback: true,
            },
            fallbacks,
        );
    }

    let turret = build_npc_turret_profile(sde, t, &mut fallbacks);
    let missile = build_npc_missile_profile(sde, t, &mut fallbacks);
    let is_armed = turret.is_some() || missile.is_some();

    let turret_dps_vec = turret
        .as_ref()
        .map(|w| w.shot_damage.scaled(1.0 / w.cycle_duration_s))
        .unwrap_or_default();
    let missile_dps_vec = missile
        .as_ref()
        .map(|m| m.volley_damage.scaled(1.0 / m.cycle_duration_s))
        .unwrap_or_default();
    let nominal_damage_vector = sum_damage_vectors(turret_dps_vec, missile_dps_vec);
    let total_nominal_dps = nominal_damage_vector.total();

    let fly_range_opt = a.get(&ATTR_ENTITY_FLY_RANGE).copied();
    let npc_orbit_opt = a.get(&ATTR_NPC_MAX_COMBAT_ORBIT_RANGE).copied();
    let cruise_opt = a.get(&ATTR_ENTITY_CRUISE_SPEED).copied().filter(|&v| v > 0.0);
    let is_stationary_group = matches!(t.group_id, 99 | 319 | 335 | 383 | 495 | 4576);

    let max_velocity = if let Some(v) = a.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().filter(|&v| v >= 0.0) {
        v
    } else if !is_armed || fly_range_opt == Some(0.0) || is_stationary_group {
        0.0
    } else if let Some(cv) = cruise_opt {
        fallbacks.push("maxVelocity(37)");
        cv
    } else {
        fallbacks.push("maxVelocity(37)");
        320.0
    };

    let cruise_velocity = if max_velocity <= 0.0 {
        0.0
    } else if let Some(cv) = cruise_opt {
        cv.min(max_velocity)
    } else {
        if is_armed {
            fallbacks.push("entityCruiseSpeed(508)");
        }
        max_velocity
    };

    let engagement_optimal_range = turret
        .as_ref()
        .map(|w| {
            if w.optimal_range > 0.0 {
                w.optimal_range
            } else {
                w.max_effective_range()
            }
        })
        .or_else(|| missile.as_ref().map(|m| m.max_flight_range))
        .unwrap_or(0.0);

    let orbit_range = if max_velocity <= 0.0 {
        0.0
    } else if let Some(fly) = fly_range_opt.filter(|&v| v > 0.0) {
        fly
    } else if let Some(orb) = npc_orbit_opt.filter(|&v| v > 0.0) {
        orb
    } else if is_armed {
        fallbacks.push("entityFlyRange(416)");
        (engagement_optimal_range * 0.75).clamp(2_500.0, 15_000.0)
    } else {
        0.0
    };

    (
        NpcCombatProfile {
            bounty_isk: bounty,
            max_velocity,
            cruise_velocity,
            orbit_range,
            engagement_optimal_range,
            total_nominal_dps,
            nominal_damage_vector,
            turret,
            missile,
            is_synthetic_fallback: false,
        },
        fallbacks,
    )
}

fn log_npc_fallbacks_once(type_id: u32, type_name: &str, fallbacks: &[&'static str]) {
    if fallbacks.is_empty() {
        return;
    }
    let set = LOGGED_FALLBACK_NPC_TYPES.get_or_init(|| Mutex::new(HashSet::new()));
    let newly_inserted = set
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(type_id);
    if newly_inserted {
        tracing::warn!(
            target: "npc_ai",
            type_id,
            npc_name = type_name,
            missing_attrs = ?fallbacks,
            "NPC type missing SDE combat/movement attribute(s); using fallback(s)"
        );
    }
}

pub(crate) fn resolve_npc_combat_profile(
    sde: &SdeRegistry,
    type_id: u32,
    bounty_isk_override: Option<f64>,
    faction_id: u32,
) -> NpcCombatProfile {
    let (profile, fallbacks) = inspect_npc_sde_profile(sde, type_id, bounty_isk_override, faction_id);
    let type_name = sde
        .types
        .get(&type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("<unknown>");
    log_npc_fallbacks_once(type_id, type_name, &fallbacks);
    profile
}

impl NpcSpawnManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns an SDE NPC spawn group (e.g. Guristas / Blood Raider / Serpentis pirate patrol)
    /// at the given belt or gate position.
    pub async fn spawn_group(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        spawn_group_id: u32,
        spawn_pos: DVec3,
    ) -> Result<Vec<i64>, String> {
        let spawn_group = ctx
            .sde
            .npc_spawn_groups
            .get(&spawn_group_id)
            .cloned()
            .ok_or_else(|| format!("SDE NPC spawn group {spawn_group_id} not found"))?;
        let count = spawn_group.npc_type_ids.len() as i64;
        let start_ball_id = ctx.db.with_mut_snapshot(|snap| {
            let start = snap.next_entity_id + 1;
            snap.next_entity_id += count;
            start
        });
        Ok(self
            .spawn_group_with_start_id(ctx, sys_id, spawn_group_id, &spawn_group, spawn_pos, start_ball_id)
            .await)
    }

    async fn spawn_group_detached_with_start_id(
        ctx: &ServerContext,
        sys_id: u32,
        spawn_group_id: u32,
        spawn_group: &eve_sde::NpcSpawnGroupInfo,
        spawn_pos: DVec3,
        start_ball_id: i64,
    ) -> (Vec<i64>, Vec<(i64, NpcCombatEntity)>) {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut spawned_ball_ids = Vec::with_capacity(spawn_group.npc_type_ids.len());
        let mut spawned_entities = Vec::with_capacity(spawn_group.npc_type_ids.len());

        for (idx, &npc_type_id) in spawn_group.npc_type_ids.iter().enumerate() {
            let ball_id = start_ball_id + idx as i64;
            let faction_id = spawn_group.faction_id;
            let profile = resolve_npc_combat_profile(&ctx.sde, npc_type_id, None, faction_id);

            // Disperse ships slightly around spawn center
            let offset = DVec3::new(
                ((idx as f64) * 500.0).sin() * 1_200.0,
                0.0,
                ((idx as f64) * 500.0).cos() * 1_200.0,
            );
            let entity_pos = spawn_pos + offset;

            let sde_t = ctx.sde.types.get(&npc_type_id);
            let ball_inertia = sde_t
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied())
                .filter(|&a| a > 0.0)
                .unwrap_or(2.5);
            let ball_mass = sde_t
                .map(|t| t.mass)
                .filter(|&m| m > 0.0)
                .unwrap_or(1_200_000.0);
            let ball_radius = sde_t
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(45.0);

            let ball = Ball {
                id: ball_id,
                type_id: npc_type_id,
                position: entity_pos,
                velocity: DVec3::ZERO,
                max_velocity: profile.max_velocity,
                inertia: ball_inertia,
                mass: ball_mass,
                radius: ball_radius,
                speed_fraction: if profile.max_velocity > 0.0 { 1.0 } else { 0.0 },
                mode: MovementMode::Stop,
            };

            let combat_state = ctx.build_type_combat_state(npc_type_id);
            let fac_owner = if faction_id > 0 {
                faction_id as i64
            } else {
                500_001
            };
            let npc_meta = eve_destiny::SlimItemMeta {
                group_id: sde_t.map(|t| t.group_id).unwrap_or(546),
                category_id: sde_t.map(|t| t.category_id).unwrap_or(11),
                owner_id: fac_owner,
                char_id: None,
                corp_id: crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_faction(ctx, fac_owner),
                alliance_id: -1,
                security_status: -1.0,
                is_cloaked: false,
                name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
                ..Default::default()
            };
            let _ = sys_handle
                .spawn_ball_with_meta(ball, combat_state, npc_meta)
                .await;

            let entity = NpcCombatEntity {
                ball_id,
                type_id: npc_type_id,
                faction_id,
                state: NpcAiState::Idle,
                spawn_group_id,
                spawn_pos: entity_pos,
                target_id: None,
                bounty_isk: profile.bounty_isk,
                optimal_range: profile.engagement_optimal_range,
                orbit_range: profile.orbit_range,
                base_dps: profile.total_nominal_dps,
                damage_vector: profile.nominal_damage_vector,
                last_attack_tick: 0,
                has_shield_rep: true,
                has_armor_rep: true,
            };

            spawned_entities.push((ball_id, entity));
            spawned_ball_ids.push(ball_id);
        }

        (spawned_ball_ids, spawned_entities)
    }

    async fn spawn_group_with_start_id(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        spawn_group_id: u32,
        spawn_group: &eve_sde::NpcSpawnGroupInfo,
        spawn_pos: DVec3,
        start_ball_id: i64,
    ) -> Vec<i64> {
        let (spawned_ball_ids, spawned_entities) = Self::spawn_group_detached_with_start_id(
            ctx,
            sys_id,
            spawn_group_id,
            spawn_group,
            spawn_pos,
            start_ball_id,
        )
        .await;
        for (ball_id, entity) in spawned_entities {
            self.insert_spawned_entity(&ctx.sde, ball_id, entity);
        }
        spawned_ball_ids
    }

    fn insert_spawned_entity(&mut self, sde: &SdeRegistry, ball_id: i64, entity: NpcCombatEntity) {
        let profile = resolve_npc_combat_profile(
            sde,
            entity.type_id,
            Some(entity.bounty_isk),
            entity.faction_id,
        );
        self.runtime.insert(
            ball_id,
            NpcRuntimeCombatState {
                uses_synthetic_legacy: profile.is_synthetic_fallback,
                baseline_damage_vector: profile.nominal_damage_vector,
                baseline_optimal_range: profile.engagement_optimal_range,
                profile,
                turret_cooldown_s: 0.0,
                missile_cooldown_s: 0.0,
            },
        );
        self.entities.insert(ball_id, entity);
    }

    pub(crate) fn plan_belt_rats_for_system(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        drifter_alive: bool,
        rt_until_opt: Option<u64>,
        cs_alive: bool,
        mordu_alive: bool,
        officer_alive: bool,
    ) -> (Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>, Vec<(u32, &'static str, u32, DVec3)>) {
        let now_unix = ctx.now_unix();
        // Wormhole & Drifter Hive systems (31_000_000..32_000_000): spawn Drifter Cruiser + Battleship (`feat:npc.drifters`)
        if (31_000_000..32_000_000).contains(&sys_id) {
            let on_cd = self
                .respawn_timers
                .get(&(sys_id, 500_024))
                .is_some_and(|&until| now_unix < until);
            if !drifter_alive && !on_cd {
                self.respawn_timers
                    .insert((sys_id, 500_024), now_unix.saturating_add(600));
                let anchor_pos = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .first()
                    .map(|c| c.position + DVec3::new(15_000.0, 0.0, 0.0))
                    .unwrap_or(DVec3::new(15_000.0, 0.0, 0.0));
                let cruiser_tid = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.group_id == 1956)
                    .map(|t| t.type_id)
                    .min()
                    .unwrap_or(47153);
                let bs_group = if (31_000_001..=31_000_006).contains(&sys_id) {
                    1530
                } else {
                    1310
                };
                let bs_tid = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.group_id == bs_group)
                    .map(|t| t.type_id)
                    .min()
                    .unwrap_or(34495);
                return (
                    Vec::new(),
                    vec![
                        (cruiser_tid, "Drifter", 500_024, anchor_pos),
                        (
                            bs_tid,
                            "Drifter",
                            500_024,
                            anchor_pos + DVec3::new(2_500.0, 0.0, 0.0),
                        ),
                    ],
                );
            }
            return (Vec::new(), Vec::new());
        }

        let belt_positions: Vec<DVec3> = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .filter(|c| c.group_id == 9) // Asteroid Belt
            .map(|c| c.position)
            .collect();

        if belt_positions.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let sys_sec = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status as f64)
            .unwrap_or(0.5);
        let reg_faction = regional_pirate_faction_id(ctx, sys_id);

        let max_ship_class: u32 = if sys_sec >= 0.45 {
            2 // High-sec: Frigate / Destroyer
        } else if sys_sec >= 0.05 {
            4 // Low-sec: up to Cruiser / Battlecruiser
        } else {
            6 // Null-sec / W-space: up to Battleship
        };
        let hull_rank = |g: &eve_sde::NpcSpawnGroupInfo| -> u8 {
            if g.group_name.contains("Frigate") {
                0
            } else if g.group_name.contains("Destroyer") {
                1
            } else if g.group_name.contains("BattleCruiser") {
                3
            } else if g.group_name.contains("Cruiser") {
                2
            } else if g.group_name.contains("Battleship") {
                4
            } else {
                5
            }
        };
        let is_non_standard_belt_group = |g: &eve_sde::NpcSpawnGroupInfo| -> bool {
            g.group_name.contains("Officer")
                || g.group_name.contains("Commander")
                || g.group_name.contains("Hauler")
                || g.group_name.contains("Dreadnought")
                || g.group_name.contains("Titan")
                || g.group_name.contains("Deadspace")
                || g.group_name.contains("Ghost")
                || g.group_name.contains("Mission")
                || g.group_name.contains("Incursion")
        };
        let max_hull_rank: u8 = if sys_sec >= 0.45 {
            1 // High-sec: Frigate / Destroyer (plus untagged custom groups == 5)
        } else if sys_sec >= 0.05 {
            3 // Low-sec: up to BattleCruiser
        } else {
            4 // Null-sec / W-space: up to Battleship
        };
        let fits_sec_tier = |g: &eve_sde::NpcSpawnGroupInfo| -> bool {
            let hr = hull_rank(g);
            g.ship_class <= max_ship_class
                && !is_non_standard_belt_group(g)
                && (hr <= max_hull_rank || hr == 5)
        };
        let is_eligible_belt_group = |g: &eve_sde::NpcSpawnGroupInfo| -> bool {
            !g.npc_type_ids.is_empty()
                && !crate::services::combat_crimewatch::combat_legacy_ess::is_non_combat_npc_prop_group(g.group_id)
                && !crate::services::combat_crimewatch::combat_legacy_ess::is_invulnerable_npc_authority_group(g.group_id)
                && !crate::services::combat_crimewatch::combat_legacy_ess::is_sleeper_or_drifter_npc_group(g.group_id)
                && !matches!(
                    g.group_id,
                    182 | 279 | 286 | 297 | 298 | 301 | 383 | 495 | 520 | 522 | 523 | 527 | 533 | 534
                )
        };
        let regional_keyword = match reg_faction {
            500_011 => "Angel",
            500_012 => "Blood",
            500_019 => "Sansha",
            500_020 => "Serpentis",
            _ => "Guristas",
        };
        let mut candidate_groups: Vec<u32> = ctx
            .sde
            .npc_spawn_groups
            .iter()
            .filter(|(_, g)| {
                is_eligible_belt_group(g)
                    && fits_sec_tier(g)
                    && (g.faction_id == reg_faction
                        || (g.faction_id == 0 && g.group_name.contains(regional_keyword)))
            })
            .map(|(&gid, _)| gid)
            .collect();
        if candidate_groups.is_empty() {
            candidate_groups = ctx
                .sde
                .npc_spawn_groups
                .iter()
                .filter(|(_, g)| {
                    is_eligible_belt_group(g)
                        && (g.faction_id == reg_faction || g.faction_id == 0)
                        && fits_sec_tier(g)
                })
                .map(|(&gid, _)| gid)
                .collect();
        }
        if candidate_groups.is_empty() {
            candidate_groups = ctx
                .sde
                .npc_spawn_groups
                .iter()
                .filter(|(_, g)| {
                    is_eligible_belt_group(g) && (g.faction_id == reg_faction || g.faction_id == 0)
                })
                .map(|(&gid, _)| gid)
                .collect();
        }
        if candidate_groups.is_empty() {
            candidate_groups = ctx.sde.npc_spawn_groups.keys().copied().collect();
        }
        candidate_groups.sort_by_key(|&gid| {
            ctx.sde.npc_spawn_groups.get(&gid).map_or(
                (true, true, u32::MAX, u8::MAX, gid),
                |g| {
                    (
                        is_non_standard_belt_group(g),
                        !g.group_name.starts_with("Asteroid "),
                        g.ship_class,
                        hull_rank(g),
                        gid,
                    )
                },
            )
        });

        let mut planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)> = Vec::new();
        if !candidate_groups.is_empty() {
            for (idx, &belt_pos) in belt_positions.iter().take(3).enumerate() {
                let group_id = candidate_groups[idx % candidate_groups.len()];
                let belt_alive = self
                    .entities
                    .values()
                    .any(|e| e.spawn_pos.distance(belt_pos) < 25_000.0);
                if belt_alive
                    || self
                        .respawn_timers
                        .get(&(sys_id, group_id))
                        .is_some_and(|&until| now_unix < until)
                {
                    continue;
                }
                if let Some(sg) = ctx.sde.npc_spawn_groups.get(&group_id) {
                    planned_groups.push((group_id, sg.clone(), belt_pos));
                }
            }
            if sys_sec < 0.45 {
                let gate_positions: Vec<DVec3> = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .into_iter()
                    .filter(|c| c.group_id == 10) // Stargate
                    .map(|c| c.position)
                    .collect();
                if let Some(&gate_pos) = gate_positions.first() {
                    let gate_group_id = candidate_groups[belt_positions.len() % candidate_groups.len()];
                    let gate_alive = self
                        .entities
                        .values()
                        .any(|e| e.spawn_pos.distance(gate_pos) < 25_000.0);
                    if !gate_alive
                        && !self
                            .respawn_timers
                            .get(&(sys_id, gate_group_id))
                            .is_some_and(|&until| now_unix < until)
                    {
                        if let Some(sg) = ctx.sde.npc_spawn_groups.get(&gate_group_id) {
                            planned_groups.push((gate_group_id, sg.clone(), gate_pos));
                        }
                    }
                }
            }
        }

        let need_cs = clone_soldier_tier_for_security(sys_sec).is_some();
        let need_officer = sys_sec < 0.0 && sys_id < 31_000_000;
        let mut special_specs: Vec<(u32, &'static str, u32, DVec3)> = Vec::new();

        // In low-sec (0.1..=0.4), spawn a Clone Soldier at the first asteroid belt if not on respawn cooldown (`feat:npc.clone-soldier-spawns`).
        if need_cs {
            if let Some(rt_until) = rt_until_opt {
                let entry = self.clone_soldier_respawn_timers.entry(sys_id).or_insert(rt_until);
                if rt_until > *entry {
                    *entry = rt_until;
                }
            }
            let on_cooldown = self
                .clone_soldier_respawn_timers
                .get(&sys_id)
                .is_some_and(|&until| now_unix < until)
                || cs_alive;
            if !on_cooldown {
                if let Some(cs_type_id) =
                    clone_soldier_type_for_security_and_faction(&ctx.sde, sys_sec, reg_faction)
                {
                    special_specs.push((
                        cs_type_id,
                        "CloneSoldier",
                        reg_faction,
                        belt_positions[0],
                    ));
                }
            }

            // Low-sec Mordu's Legion Special Warfare Unit belt spawn (SDE groups 1285..=1287, faction 500018)
            let mordu_cd = self
                .respawn_timers
                .get(&(sys_id, 500_018))
                .is_some_and(|&until| now_unix < until)
                || mordu_alive;
            if !mordu_cd {
                self.respawn_timers
                    .insert((sys_id, 500_018), now_unix.saturating_add(300));
                if ctx.mechanic_rng.roll(0.25) {
                    let mordu_grp: u32 = if sys_sec >= 0.30 {
                        1285 // Frigate Operative (Garmur)
                    } else if sys_sec >= 0.15 {
                        1286 // Cruiser Specialist (Orthrus)
                    } else {
                        1287 // Battleship Commander (Barghest)
                    };
                    if let Some(mordu_tid) = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.group_id == mordu_grp)
                        .map(|t| t.type_id)
                        .min()
                    {
                        let m_pos = belt_positions
                            .get(1)
                            .copied()
                            .unwrap_or(belt_positions[0])
                            + DVec3::new(2_500.0, 0.0, 0.0);
                        special_specs.push((mordu_tid, "MorduCommander", 500_018, m_pos));
                    }
                }
            }
        }

        // Rare Faction Commander spawn at asteroid belts (`feat:npc.belt-rats`)
        let cmd_cd = self
            .respawn_timers
            .get(&(sys_id, 900_050))
            .is_some_and(|&until| now_unix < until);
        if !cmd_cd {
            self.respawn_timers
                .insert((sys_id, 900_050), now_unix.saturating_add(300));
            let cmd_prob = if sys_sec >= 0.45 {
                0.08
            } else if sys_sec >= 0.05 {
                0.15
            } else {
                0.20
            };
            if ctx.mechanic_rng.roll(cmd_prob) {
                let cmd_grp: u32 = match (reg_faction, sys_sec >= 0.45, sys_sec >= 0.05) {
                    (500_011, true, _) => 789,
                    (500_011, false, true) => 790,
                    (500_011, false, false) => 848,
                    (500_012, true, _) => 792,
                    (500_012, false, true) => 791,
                    (500_012, false, false) => 849,
                    (500_019, true, _) => 810,
                    (500_019, false, true) => 808,
                    (500_019, false, false) => 851,
                    (500_020, true, _) => 814,
                    (500_020, false, true) => 812,
                    (500_020, false, false) => 852,
                    (_, true, _) => 800,
                    (_, false, true) => 798,
                    (_, false, false) => 850,
                };
                let mut cmd_tids: Vec<u32> = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.group_id == cmd_grp)
                    .map(|t| t.type_id)
                    .collect();
                cmd_tids.sort_unstable();
                if !cmd_tids.is_empty() {
                    let pick = (ctx.mechanic_rng.try_next_u64().unwrap_or(0) as usize) % cmd_tids.len();
                    special_specs.push((
                        cmd_tids[pick],
                        "Officer",
                        reg_faction,
                        belt_positions[0] + DVec3::new(1_800.0, 0.0, -1_200.0),
                    ));
                }
            }
        }

        // Rare Named Officer spawn in true null-sec asteroid belts (`sys_sec < 0.0`, `feat:npc.officer-npc`)
        if need_officer {
            let off_cd = self
                .respawn_timers
                .get(&(sys_id, 900_099))
                .is_some_and(|&until| now_unix < until)
                || officer_alive;
            if !off_cd {
                self.respawn_timers
                    .insert((sys_id, 900_099), now_unix.saturating_add(600));
                if ctx.mechanic_rng.roll(0.10) {
                    let off_grp: u32 = match reg_faction {
                        500_011 => 553, // Angel Officer
                        500_012 => 559, // Blood Raiders Officer
                        500_019 => 569, // Sansha Officer
                        500_020 => 574, // Serpentis Officer
                        _ => 564,       // Guristas Officer
                    };
                    let mut off_tids: Vec<u32> = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.group_id == off_grp)
                        .map(|t| t.type_id)
                        .collect();
                    off_tids.sort_unstable();
                    if !off_tids.is_empty() {
                        let pick =
                            (ctx.mechanic_rng.try_next_u64().unwrap_or(0) as usize) % off_tids.len();
                        special_specs.push((
                            off_tids[pick],
                            "Officer",
                            reg_faction,
                            belt_positions[0] + DVec3::new(-2_500.0, 0.0, 1_500.0),
                        ));
                    }
                }
            }
        }

        (planned_groups, special_specs)
    }

    pub(crate) async fn execute_belt_rat_plan_detached_with_start_id(
        ctx: &ServerContext,
        sys_id: u32,
        planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>,
        special_specs: Vec<(u32, &'static str, u32, DVec3)>,
        mut next_start_id: i64,
        out_entities: &mut Vec<(i64, crate::services::combat_crimewatch::ExtendedNpcEntity)>,
    ) -> (Vec<i64>, Vec<(i64, NpcCombatEntity)>) {
        let mut all_spawned = Vec::new();
        let mut all_entities = Vec::new();
        for (group_id, sg, spawn_pos) in planned_groups {
            let count = sg.npc_type_ids.len() as i64;
            let (spawned, entities) = Self::spawn_group_detached_with_start_id(
                ctx,
                sys_id,
                group_id,
                &sg,
                spawn_pos,
                next_start_id,
            )
            .await;
            all_spawned.extend(spawned);
            all_entities.extend(entities);
            next_start_id += count;
        }
        if !special_specs.is_empty() {
            let special_ids = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch_with_start_id(
                ctx,
                sys_id,
                &special_specs,
                next_start_id,
                |_| {},
                out_entities,
            )
            .await;
            all_spawned.extend(special_ids);
        }
        (all_spawned, all_entities)
    }

    pub(crate) async fn execute_belt_rat_plan_with_start_id(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>,
        special_specs: Vec<(u32, &'static str, u32, DVec3)>,
        next_start_id: i64,
        out_entities: &mut Vec<(i64, crate::services::combat_crimewatch::ExtendedNpcEntity)>,
    ) -> Vec<i64> {
        let (all_spawned, all_entities) = Self::execute_belt_rat_plan_detached_with_start_id(
            ctx,
            sys_id,
            planned_groups,
            special_specs,
            next_start_id,
            out_entities,
        )
        .await;
        for (ball_id, entity) in all_entities {
            self.insert_spawned_entity(&ctx.sde, ball_id, entity);
        }
        all_spawned
    }

    /// Spawns belt rats at asteroid belts in a solar system based on system security and region (`feat:npc.belt-rats`, `feat:npc.clone-soldier-spawns`, `feat:npc.officer-npc`, `feat:npc.drifters`).
    pub async fn spawn_belt_rats_for_system(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
    ) -> Result<Vec<i64>, String> {
        let (drifter_alive, rt_until_opt, cs_alive, mordu_alive, officer_alive) =
            if (31_000_000..32_000_000).contains(&sys_id) {
                let da = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.npc_entities
                        .values()
                        .any(|n| n.solar_system_id == sys_id && n.archetype == "Drifter")
                });
                (da, None, false, false, false)
            } else {
                let sys_sec = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status as f64)
                    .unwrap_or(0.5);
                let need_cs = clone_soldier_tier_for_security(sys_sec).is_some();
                let need_officer = sys_sec < 0.0 && sys_id < 31_000_000;
                if need_cs || need_officer {
                    let (rt_until_opt, cs_alive, mordu_alive, officer_alive) =
                        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                            let mut cs_alive = false;
                            let mut mordu_alive = false;
                            let mut officer_alive = false;
                            for n in rt.npc_entities.values() {
                                if n.solar_system_id != sys_id {
                                    continue;
                                }
                                if n.archetype == "CloneSoldier" {
                                    cs_alive = true;
                                } else if n.archetype == "MorduCommander" {
                                    mordu_alive = true;
                                } else if need_officer
                                    && ctx
                                        .sde
                                        .types
                                        .get(&n.type_id)
                                        .is_some_and(|t| matches!(t.group_id, 553 | 559 | 564 | 569 | 574))
                                {
                                    officer_alive = true;
                                }
                            }
                            (
                                rt.clone_soldier_cooldown_until.get(&sys_id).copied(),
                                cs_alive,
                                mordu_alive,
                                officer_alive,
                            )
                        });
                    (false, rt_until_opt, cs_alive, mordu_alive, officer_alive)
                } else {
                    (false, None, false, false, false)
                }
            };

        let (planned_groups, special_specs) = self.plan_belt_rats_for_system(
            ctx,
            sys_id,
            drifter_alive,
            rt_until_opt,
            cs_alive,
            mordu_alive,
            officer_alive,
        );
        let group_npcs: i64 = planned_groups
            .iter()
            .map(|(_, sg, _)| sg.npc_type_ids.len() as i64)
            .sum();
        let total_npcs = group_npcs + special_specs.len() as i64;
        if total_npcs == 0 {
            return Ok(Vec::new());
        }
        let start_id = ctx.db.with_mut_snapshot(|snap| {
            let start = snap.next_entity_id + 1;
            snap.next_entity_id += total_npcs;
            start
        });
        let mut built_special = Vec::with_capacity(special_specs.len());
        let all_spawned = self
            .execute_belt_rat_plan_with_start_id(
                ctx,
                sys_id,
                planned_groups,
                special_specs,
                start_id,
                &mut built_special,
            )
            .await;
        if !built_special.is_empty() {
            crate::services::combat_crimewatch::with_mut_combat_ephemeral(ctx, |rt| {
                for (ball_id, entity) in built_special {
                    rt.npc_entities.insert(ball_id, entity);
                }
            });
        }
        Ok(all_spawned)
    }

    /// Evaluates the NPC AI state machine for all active NPC entities in the solar system:
    /// - `Idle`: No hostile within 50 km -> stand down / stop.
    /// - `Chase`: Hostile detected within 50 km -> pursue using `MovementMode::Follow`.
    /// - `Orbit`: Distance <= `orbit_range * 1.3` -> switch to `MovementMode::Orbit`.
    /// - `Attack`: Hostile within weapon range -> apply cycle damage, trigger shield/armor reps if HP low.
    pub fn step_ai_sync(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        current_tick: u64,
        dt: f64,
    ) {
        if self.entities.is_empty() {
            return;
        }
        let npc_ids: HashSet<i64> = self.entities.keys().copied().collect();
        let (ball_kinematics, player_targets) = collect_ai_step_inputs(ctx, sys_id, &npc_ids);
        let plan = self.plan_ai_step_for_ids(
            None,
            &ctx.sde,
            &ball_kinematics,
            &player_targets,
            current_tick,
            dt,
        );
        let destroyed_balls = apply_ai_step_world_effects(
            ctx,
            sys_id,
            plan.mode_updates,
            plan.dmg_actions,
            &ball_kinematics,
        );
        let now_unix = ctx.now_unix();
        let actions =
            self.apply_ai_step_cleanup(ctx, sys_id, plan.to_remove, destroyed_balls, now_unix);
        finalize_ai_step_cleanup(ctx, sys_id, actions, now_unix);
    }

    fn plan_ai_step_for_ids(
        &mut self,
        tracked_ids: Option<&HashSet<i64>>,
        sde: &SdeRegistry,
        ball_kinematics: &HashMap<i64, AiBallKinematics>,
        player_targets: &[(i64, DVec3, f64)],
        current_tick: u64,
        dt: f64,
    ) -> AiStepPlan {
        let step_dt = if dt.is_finite() && dt > 0.0 { dt } else { 1.0 };
        self.runtime.retain(|id, _| self.entities.contains_key(id));

        let mut to_remove = Vec::new();
        let mut mode_updates: Vec<ModeUpdate> = Vec::new();
        let mut dmg_actions: Vec<AiDamageAction> = Vec::new();

        for (&ball_id, entity) in self.entities.iter_mut() {
            if let Some(ids) = tracked_ids {
                if !ids.contains(&ball_id) {
                    continue;
                }
            }
            let Some(&npc_kin) = ball_kinematics.get(&ball_id) else {
                to_remove.push(ball_id);
                continue;
            };
            let rt = self.runtime.entry(ball_id).or_insert_with(|| {
                let profile = resolve_npc_combat_profile(
                    sde,
                    entity.type_id,
                    Some(entity.bounty_isk),
                    entity.faction_id,
                );
                let overridden = !damage_vector_approx_eq(entity.damage_vector, profile.nominal_damage_vector)
                    || (entity.optimal_range - profile.engagement_optimal_range).abs() > 1e-3;
                NpcRuntimeCombatState {
                    uses_synthetic_legacy: profile.is_synthetic_fallback || overridden,
                    baseline_damage_vector: profile.nominal_damage_vector,
                    baseline_optimal_range: profile.engagement_optimal_range,
                    profile,
                    turret_cooldown_s: 0.0,
                    missile_cooldown_s: 0.0,
                }
            });
            if !rt.uses_synthetic_legacy
                && (!damage_vector_approx_eq(entity.damage_vector, rt.baseline_damage_vector)
                    || (entity.optimal_range - rt.baseline_optimal_range).abs() > 1e-3)
            {
                rt.uses_synthetic_legacy = true;
            }
            let uses_synthetic_legacy = rt.uses_synthetic_legacy;
            let max_velocity = rt.profile.max_velocity;
            let orbit_speed_fraction = if max_velocity > 0.0 {
                (rt.profile.cruise_velocity / max_velocity).clamp(0.05, 1.0)
            } else {
                0.0
            };
            let chase_speed_fraction = if max_velocity > 0.0 { 1.0 } else { 0.0 };

            // Pick highest-threat candidate within 50 km (breaking ties by closest distance)
            let best_in_range = player_targets
                .iter()
                .map(|&(pid, pos, weight)| (pid, pos, (pos - npc_kin.position).length(), weight))
                .filter(|&(_, _, dist, _)| dist <= 50_000.0)
                .max_by(|a, b| {
                    a.3.partial_cmp(&b.3)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
                });

            match best_in_range {
                Some((target_id, _target_pos, dist, _)) => {
                    let target_changed = entity.target_id != Some(target_id);
                    entity.target_id = Some(target_id);
                    let target_radius = ball_kinematics
                        .get(&target_id)
                        .map(|tk| tk.radius)
                        .unwrap_or(0.0);
                    let surface_dist = (dist - npc_kin.radius.max(0.0) - target_radius.max(0.0)).max(0.0);
                    let is_stationary = max_velocity <= 0.0;
                    let in_orbit_band = !is_stationary && dist <= entity.orbit_range * 1.4;
                    let prev_state = entity.state;

                    if !is_stationary {
                        if !in_orbit_band {
                            // 1. Chase target at maxVelocity
                            if prev_state != NpcAiState::Chase || target_changed {
                                entity.state = NpcAiState::Chase;
                                mode_updates.push(ModeUpdate {
                                    ball_id,
                                    mode: MovementMode::Follow {
                                        target_id,
                                        range: entity.orbit_range,
                                    },
                                    speed_fraction: chase_speed_fraction,
                                });
                            }
                        } else {
                            // 2. Orbit target at entityCruiseSpeed
                            if !matches!(prev_state, NpcAiState::Orbit | NpcAiState::Attack)
                                || target_changed
                            {
                                entity.state = NpcAiState::Orbit;
                                mode_updates.push(ModeUpdate {
                                    ball_id,
                                    mode: MovementMode::Orbit {
                                        target_id,
                                        range: entity.orbit_range,
                                    },
                                    speed_fraction: orbit_speed_fraction,
                                });
                            }
                        }
                    }

                    // 3. Weapon engagement & ROF accumulator
                    if uses_synthetic_legacy {
                        rt.turret_cooldown_s = (rt.turret_cooldown_s - step_dt).max(0.0);
                        rt.missile_cooldown_s = (rt.missile_cooldown_s - step_dt).max(0.0);
                        if (is_stationary || in_orbit_band) && dist <= entity.optimal_range * 1.25 {
                            entity.state = NpcAiState::Attack;
                            if current_tick >= entity.last_attack_tick.saturating_add(2) {
                                entity.last_attack_tick = current_tick;
                                dmg_actions.push(AiDamageAction::Synthetic {
                                    attacker_id: ball_id,
                                    npc_type_id: entity.type_id,
                                    target_id,
                                    dmg: entity.damage_vector.scaled(0.5),
                                });
                            }
                        } else if is_stationary {
                            entity.state = NpcAiState::Idle;
                        }
                    } else {
                        let mut fired_or_in_range = false;

                        if let Some(turret) = &rt.profile.turret {
                            let max_r = turret.max_effective_range();
                            if surface_dist <= max_r && turret.shot_damage.total() > 0.0 {
                                fired_or_in_range = true;
                                let mut shots = 0_u32;
                                while rt.turret_cooldown_s <= 1e-9 && shots < 16 {
                                    shots += 1;
                                    rt.turret_cooldown_s += turret.cycle_duration_s;
                                    entity.last_attack_tick = current_tick;
                                    if turret.kind == NpcWeaponKind::Vorton {
                                        dmg_actions.push(AiDamageAction::Vorton {
                                            attacker_id: ball_id,
                                            npc_type_id: entity.type_id,
                                            target_id,
                                            module_type_id: turret.gfx_type_id,
                                            fx_guid: turret.fx_guid.clone(),
                                            cycle_duration_s: turret.cycle_duration_s,
                                            shot_damage: turret.shot_damage,
                                            optimal_range: turret.optimal_range,
                                            explosion_radius: turret.vorton_explosion_radius,
                                            explosion_velocity: turret.vorton_explosion_velocity,
                                            drf: turret.vorton_drf,
                                        });
                                    } else {
                                        dmg_actions.push(AiDamageAction::Turret {
                                            attacker_id: ball_id,
                                            npc_type_id: entity.type_id,
                                            target_id,
                                            module_type_id: turret.gfx_type_id,
                                            fx_guid: turret.fx_guid.clone(),
                                            cycle_duration_s: turret.cycle_duration_s,
                                            shot_damage: turret.shot_damage,
                                            optimal_range: turret.optimal_range,
                                            falloff_range: turret.falloff_range,
                                            tracking_speed: turret.tracking_speed,
                                            sig_resolution: turret.signature_resolution,
                                            max_effective_range: max_r,
                                        });
                                    }
                                }
                                rt.turret_cooldown_s -= step_dt;
                            } else {
                                rt.turret_cooldown_s = (rt.turret_cooldown_s - step_dt).max(0.0);
                            }
                        }

                        if let Some(missile) = &rt.profile.missile {
                            if surface_dist <= missile.max_flight_range
                                && missile.volley_damage.total() > 0.0
                            {
                                fired_or_in_range = true;
                                let mut volleys = 0_u32;
                                while rt.missile_cooldown_s <= 1e-9 && volleys < 16 {
                                    volleys += 1;
                                    rt.missile_cooldown_s += missile.cycle_duration_s;
                                    entity.last_attack_tick = current_tick;
                                    dmg_actions.push(AiDamageAction::Missile {
                                        attacker_id: ball_id,
                                        npc_type_id: entity.type_id,
                                        target_id,
                                        missile_type_id: missile.missile_type_id,
                                        fx_guid: missile.fx_guid.clone(),
                                        cycle_duration_s: missile.cycle_duration_s,
                                        volley_damage: missile.volley_damage,
                                        max_flight_range: missile.max_flight_range,
                                        explosion_radius: missile.explosion_radius,
                                        explosion_velocity: missile.explosion_velocity,
                                        drf: missile.damage_reduction_factor,
                                    });
                                }
                                rt.missile_cooldown_s -= step_dt;
                            } else {
                                rt.missile_cooldown_s = (rt.missile_cooldown_s - step_dt).max(0.0);
                            }
                        }

                        if is_stationary {
                            entity.state = if fired_or_in_range {
                                NpcAiState::Attack
                            } else {
                                NpcAiState::Idle
                            };
                        } else if in_orbit_band {
                            entity.state = if fired_or_in_range {
                                NpcAiState::Attack
                            } else {
                                NpcAiState::Orbit
                            };
                        }
                    }
                }
                _ => {
                    // No target within 50km: transition to Idle
                    rt.turret_cooldown_s = (rt.turret_cooldown_s - step_dt).max(0.0);
                    rt.missile_cooldown_s = (rt.missile_cooldown_s - step_dt).max(0.0);
                    if entity.state != NpcAiState::Idle {
                        entity.state = NpcAiState::Idle;
                        entity.target_id = None;
                        if max_velocity > 0.0 {
                            mode_updates.push(ModeUpdate {
                                ball_id,
                                mode: MovementMode::Stop,
                                speed_fraction: 0.0,
                            });
                        }
                    } else if entity.target_id.is_some() {
                        entity.target_id = None;
                    }
                }
            }
        }

        AiStepPlan {
            to_remove,
            mode_updates,
            dmg_actions,
        }
    }

    fn remove_npc_and_record_cooldown(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        npc_ball_id: i64,
        now_unix: u64,
    ) -> Option<(NpcCombatEntity, Option<u64>)> {
        self.runtime.remove(&npc_ball_id);
        let entity = self.entities.remove(&npc_ball_id)?;
        let until = now_unix.saturating_add(1_200);
        let clone_soldier_until = if crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
            ctx,
            entity.type_id,
        ) == "CloneSoldier"
        {
            self.clone_soldier_respawn_timers.insert(sys_id, until);
            Some(until)
        } else {
            self.respawn_timers
                .insert((sys_id, entity.spawn_group_id), until);
            None
        };
        Some((entity, clone_soldier_until))
    }

    fn apply_ai_step_cleanup(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        to_remove: Vec<i64>,
        destroyed_balls: Vec<(i64, i64, Ball)>,
        now_unix: u64,
    ) -> AiCleanupActions {
        let mut clone_soldier_cooldowns = Vec::new();
        for dead_id in to_remove {
            if let Some((_dead_ent, cs_until)) =
                self.remove_npc_and_record_cooldown(ctx, sys_id, dead_id, now_unix)
            {
                if let Some(until) = cs_until {
                    clone_soldier_cooldowns.push(until);
                }
            }
        }

        let mut destroyed_tracked_npcs = Vec::new();
        let mut remaining_destructions = Vec::with_capacity(destroyed_balls.len());
        for (attacker_npc_id, target_id, dead_ball) in destroyed_balls {
            if let Some((entity, cs_until)) =
                self.remove_npc_and_record_cooldown(ctx, sys_id, target_id, now_unix)
            {
                if let Some(until) = cs_until {
                    clone_soldier_cooldowns.push(until);
                }
                destroyed_tracked_npcs.push((attacker_npc_id, entity));
            } else {
                remaining_destructions.push((attacker_npc_id, target_id, dead_ball));
            }
        }

        AiCleanupActions {
            clone_soldier_cooldowns,
            destroyed_tracked_npcs,
            remaining_destructions,
        }
    }

    pub async fn step_ai(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        current_tick: u64,
        dt: f64,
    ) {
        self.step_ai_sync(ctx, sys_id, current_tick, dt);
    }

    /// Handles death of an NPC: credits dynamic bounty based on `ENTITY_KILL_BOUNTY`,
    /// spawns a wreck with loot drops, raises killer's security status, records a killmail with the exact bounty value, and updates respawn timers.
    pub fn handle_npc_destroyed(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        npc_ball_id: i64,
        killer_char_id: i64,
        now_unix: u64,
    ) -> Result<f64, String> {
        let Some((entity, cs_until)) =
            self.remove_npc_and_record_cooldown(ctx, sys_id, npc_ball_id, now_unix)
        else {
            return Err(format!(
                "NPC ball {npc_ball_id} not tracked by spawn manager"
            ));
        };
        if let Some(until) = cs_until {
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.clone_soldier_cooldown_until.insert(sys_id, until);
            });
        }
        finalize_destroyed_npc_side_effects(ctx, sys_id, &entity, killer_char_id, now_unix)
    }
}

#[derive(Debug, Clone, Copy)]
struct AiBallKinematics {
    position: DVec3,
    radius: f64,
    target_sig_radius: f64,
}

struct ModeUpdate {
    ball_id: i64,
    mode: MovementMode,
    speed_fraction: f64,
}

enum AiDamageAction {
    Turret {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        module_type_id: Option<u32>,
        fx_guid: String,
        cycle_duration_s: f64,
        shot_damage: DamageVector,
        optimal_range: f64,
        falloff_range: f64,
        tracking_speed: f64,
        sig_resolution: f64,
        max_effective_range: f64,
    },
    Vorton {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        module_type_id: Option<u32>,
        fx_guid: String,
        cycle_duration_s: f64,
        shot_damage: DamageVector,
        optimal_range: f64,
        explosion_radius: f64,
        explosion_velocity: f64,
        drf: f64,
    },
    Missile {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        missile_type_id: u32,
        fx_guid: String,
        cycle_duration_s: f64,
        volley_damage: DamageVector,
        max_flight_range: f64,
        explosion_radius: f64,
        explosion_velocity: f64,
        drf: f64,
    },
    Synthetic {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        dmg: DamageVector,
    },
}

struct AiStepPlan {
    to_remove: Vec<i64>,
    mode_updates: Vec<ModeUpdate>,
    dmg_actions: Vec<AiDamageAction>,
}

struct AiCleanupActions {
    clone_soldier_cooldowns: Vec<u64>,
    destroyed_tracked_npcs: Vec<(i64, NpcCombatEntity)>,
    remaining_destructions: Vec<(i64, i64, Ball)>,
}

/// Steps the system's `ServerContext::npc_spawns` AI without holding `RANK_NPC_SPAWNS` across
/// `ballpark`, `db`, `combat_ephemeral`, or `death_pipeline` (`cmd_sender` / `try_send_fit_projection`).
pub(crate) fn step_system_npc_spawns_ai_sync(
    ctx: &ServerContext,
    sys_id: u32,
    current_tick: u64,
    dt: f64,
) {
    let npc_ids: HashSet<i64> = {
        let spawns = ctx.npc_spawns.read().unwrap_or_else(|e| e.into_inner());
        let Some(mgr) = spawns.get(&sys_id) else {
            return;
        };
        if mgr.entities.is_empty() {
            return;
        }
        mgr.entities.keys().copied().collect()
    };
    let (ball_kinematics, player_targets) = collect_ai_step_inputs(ctx, sys_id, &npc_ids);
    let plan = {
        let mut spawns = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
        let Some(mgr) = spawns.get_mut(&sys_id) else {
            return;
        };
        mgr.plan_ai_step_for_ids(
            Some(&npc_ids),
            &ctx.sde,
            &ball_kinematics,
            &player_targets,
            current_tick,
            dt,
        )
    };
    let destroyed_balls = apply_ai_step_world_effects(
        ctx,
        sys_id,
        plan.mode_updates,
        plan.dmg_actions,
        &ball_kinematics,
    );
    let now_unix = ctx.now_unix();
    let actions = {
        let mut spawns = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
        if let Some(mgr) = spawns.get_mut(&sys_id) {
            mgr.apply_ai_step_cleanup(ctx, sys_id, plan.to_remove, destroyed_balls, now_unix)
        } else {
            AiCleanupActions {
                clone_soldier_cooldowns: Vec::new(),
                destroyed_tracked_npcs: Vec::new(),
                remaining_destructions: destroyed_balls,
            }
        }
    };
    finalize_ai_step_cleanup(ctx, sys_id, actions, now_unix);
}

fn collect_ai_step_inputs(
    ctx: &ServerContext,
    sys_id: u32,
    npc_ids: &HashSet<i64>,
) -> (HashMap<i64, AiBallKinematics>, Vec<(i64, DVec3, f64)>) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (mut ball_kinematics, mut cand_targets): (
        HashMap<i64, AiBallKinematics>,
        Vec<(i64, u32, DVec3, f64, Option<f64>, Option<f64>)>,
    ) = {
        let bp = sys_handle
            .ballpark
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let mut kin_map = HashMap::with_capacity(npc_ids.len());
        for &nid in npc_ids {
            if let Some(b) = bp.balls.get(&nid) {
                kin_map.insert(
                    nid,
                    AiBallKinematics {
                        position: b.position,
                        radius: b.radius,
                        target_sig_radius: 0.0,
                    },
                );
            }
        }
        let mut cands = Vec::new();
        for (&id, b) in &bp.balls {
            if npc_ids.contains(&id)
                || bp.static_ball_ids.contains(&id)
                || bp.non_free_ball_ids.contains(&id)
                || b.type_id == 670
                || b.max_velocity <= 0.0
                || matches!(b.mode, MovementMode::Warp { .. } | MovementMode::Rigid)
                || bp
                    .combat_states
                    .get(&id)
                    .is_some_and(|cs| cs.is_cloaked || cs.structure_hp <= 0.0)
            {
                continue;
            }
            let cat_id = bp
                .ball_slim_meta
                .get(&id)
                .map(|m| m.category_id)
                .or_else(|| ctx.sde.types.get(&b.type_id).map(|t| t.category_id))
                .unwrap_or(6);
            if matches!(cat_id, 6 | 18 | 65) {
                let cs_opt = bp.combat_states.get(&id);
                let cs_sig = cs_opt.map(|cs| cs.signature_radius).filter(|&s| s > 0.0);
                let cs_base_sig = cs_opt.and_then(|cs| cs.base_signature_radius).filter(|&s| s > 0.0);
                cands.push((
                    id,
                    b.type_id,
                    b.position,
                    b.radius,
                    cs_sig,
                    cs_base_sig,
                ));
            }
        }
        (kin_map, cands)
    };

    if !cand_targets.is_empty() {
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            cand_targets.retain(|(id, _, _, _, _, _)| !rt.npc_entities.contains_key(id));
        });
    }

    // Resolve effective target signature radius (with fit / MWD / TP bloom) outside ballpark & npc_spawns locks.
    let mut target_sigs: HashMap<i64, f64> = HashMap::with_capacity(cand_targets.len());
    for &(bid, type_id, pos, radius, cs_sig_opt, cs_base_sig_opt) in &cand_targets {
        let raw_sde_sig = ctx
            .sde
            .types
            .get(&type_id)
            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).copied())
            .filter(|&v| v > 0.0)
            .unwrap_or_else(|| radius.max(25.0));
        let eff_sig = if let Some(cs_sig) = cs_sig_opt.filter(|_| cs_base_sig_opt.is_some()) {
            cs_sig
        } else {
            let db_sig = ctx.effective_target_signature_radius(
                bid,
                cs_sig_opt.unwrap_or(raw_sde_sig),
            );
            if let Some(cs_sig) = cs_sig_opt {
                if (cs_sig - raw_sde_sig).abs() > 1e-3 && (db_sig - raw_sde_sig).abs() <= 1e-3 {
                    cs_sig
                } else {
                    db_sig
                }
            } else {
                db_sig
            }
        };
        target_sigs.insert(bid, eff_sig.max(1.0));
        ball_kinematics.entry(bid).or_insert(AiBallKinematics {
            position: pos,
            radius,
            target_sig_radius: eff_sig.max(1.0),
        });
    }

    // Find potential player targets (ships/drones/structures that are not NPCs, not asteroids/celestials, not capsules, not cloaked, not warping, and alive)
    let player_targets: Vec<(i64, DVec3, f64)> = if cand_targets.is_empty() {
        Vec::new()
    } else {
        ctx.db.with_snapshot(|snap| {
            let mut ewar_set: HashSet<i64> = HashSet::new();
            let mut logi_set: HashSet<i64> = HashSet::new();
            if !snap.module_active_states.is_empty() {
                for &aid in &snap.module_active_states {
                    if let Some(it) = snap.items.get(&aid) {
                        if let Some(mt) = ctx.sde.types.get(&it.type_id) {
                            if matches!(mt.group_id, 52 | 65 | 201 | 208 | 291 | 357) {
                                ewar_set.insert(it.location_id);
                            }
                            if matches!(mt.group_id, 41 | 325 | 585) {
                                logi_set.insert(it.location_id);
                            }
                        }
                    }
                }
            }
            cand_targets
                .into_iter()
                .map(|(bid, _type_id, pos, _, _, _)| {
                    let sig = target_sigs.get(&bid).copied().unwrap_or(40.0);
                    let ewar = ewar_set.contains(&bid);
                    let logi = logi_set.contains(&bid);
                    // DERIVED: EWAR 1.5x, logistics 1.3x, and signature radius threat weight.
                    let mult = if ewar { 1.5 } else if logi { 1.3 } else { 1.0 };
                    let weight = (100.0 + (sig / 40.0).clamp(0.5, 5.0) * 10.0) * mult;
                    (bid, pos, weight)
                })
                .collect()
        })
    };

    (ball_kinematics, player_targets)
}

fn combat_state_ratios(bp: &eve_destiny::Ballpark, tid: i64) -> (f64, f64, f64) {
    bp.combat_states
        .get(&tid)
        .map(|cs| {
            (
                (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
            )
        })
        .unwrap_or((0.0, 0.0, 0.0))
}

fn resolve_target_sig_radius(
    ball_kinematics: &HashMap<i64, AiBallKinematics>,
    target_id: i64,
    tgt_ball: &Ball,
) -> f64 {
    ball_kinematics
        .get(&target_id)
        .map(|k| k.target_sig_radius)
        .filter(|&s| s > 0.0)
        .unwrap_or_else(|| tgt_ball.radius.max(25.0))
}

fn apply_ai_step_world_effects(
    ctx: &ServerContext,
    sys_id: u32,
    mode_updates: Vec<ModeUpdate>,
    dmg_actions: Vec<AiDamageAction>,
    ball_kinematics: &HashMap<i64, AiBallKinematics>,
) -> Vec<(i64, i64, Ball)> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut damaged_target_ids: Vec<i64> = Vec::with_capacity(dmg_actions.len());
    let mut destroyed_balls: Vec<(i64, i64, Ball)> = Vec::new();
    let mut applied_hits: Vec<(i64, u32, i64, f64, bool, f64, f64, f64)> = Vec::new();
    if !mode_updates.is_empty() || !dmg_actions.is_empty() {
        let mut bp = sys_handle
            .ballpark
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut st_guard = sys_handle
            .actor_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let st = &mut *st_guard;
        for upd in mode_updates {
            let _ = bp.set_mode(upd.ball_id, upd.mode);
            let needs_speed_update = bp
                .balls
                .get(&upd.ball_id)
                .is_some_and(|b| (b.speed_fraction - upd.speed_fraction).abs() >= 1e-6);
            if needs_speed_update {
                let _ = bp.set_speed_fraction(upd.ball_id, upd.speed_fraction);
            }
        }
        for action in dmg_actions {
            let (
                attacker_id,
                npc_type_id,
                tid,
                module_type_id,
                other_type_id,
                fx_guid,
                cycle_duration_s,
                resolved_dmg,
                is_wrecking,
            ) = match action {
                AiDamageAction::Synthetic {
                    attacker_id,
                    npc_type_id,
                    target_id,
                    dmg,
                } => {
                    if !bp.balls.contains_key(&attacker_id) || !bp.balls.contains_key(&target_id) {
                        continue;
                    }
                    (
                        attacker_id,
                        npc_type_id,
                        target_id,
                        None,
                        None,
                        NpcWeaponKind::Laser.fx_guid().to_string(),
                        2.0,
                        dmg,
                        false,
                    )
                }
                AiDamageAction::Turret {
                    attacker_id,
                    npc_type_id,
                    target_id,
                    module_type_id,
                    fx_guid,
                    cycle_duration_s,
                    shot_damage,
                    optimal_range,
                    falloff_range,
                    tracking_speed,
                    sig_resolution,
                    max_effective_range,
                } => {
                    let (Some(atk_ball), Some(tgt_ball)) =
                        (bp.balls.get(&attacker_id), bp.balls.get(&target_id))
                    else {
                        continue;
                    };
                    let surface_dist = atk_ball.surface_distance(tgt_ball);
                    if surface_dist > max_effective_range {
                        continue;
                    }
                    let omega = crate::formulas::turret_angular_velocity(
                        atk_ball.position,
                        atk_ball.velocity,
                        tgt_ball.position,
                        tgt_ball.velocity,
                    );
                    let tracking_mult = bp
                        .combat_states
                        .get(&attacker_id)
                        .map(|cs| cs.tracking_speed_multiplier)
                        .filter(|&m| m > 0.0)
                        .unwrap_or(1.0);
                    let target_sig = resolve_target_sig_radius(ball_kinematics, target_id, tgt_ball);
                    let p_hit = crate::formulas::turret_chance_to_hit_with_res(
                        omega,
                        (tracking_speed * tracking_mult).max(1e-6),
                        sig_resolution,
                        target_sig,
                        surface_dist,
                        optimal_range,
                        falloff_range,
                    );
                    let roll_u = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5);
                    let (dmg, wreck) =
                        crate::formulas::turret_shot_damage(shot_damage, p_hit, roll_u);
                    (
                        attacker_id,
                        npc_type_id,
                        target_id,
                        module_type_id,
                        None,
                        fx_guid,
                        cycle_duration_s,
                        dmg,
                        wreck,
                    )
                }
                AiDamageAction::Vorton {
                    attacker_id,
                    npc_type_id,
                    target_id,
                    module_type_id,
                    fx_guid,
                    cycle_duration_s,
                    shot_damage,
                    optimal_range,
                    explosion_radius,
                    explosion_velocity,
                    drf,
                } => {
                    let (Some(atk_ball), Some(tgt_ball)) =
                        (bp.balls.get(&attacker_id), bp.balls.get(&target_id))
                    else {
                        continue;
                    };
                    if atk_ball.surface_distance(tgt_ball) > optimal_range {
                        continue;
                    }
                    let target_sig = resolve_target_sig_radius(ball_kinematics, target_id, tgt_ball);
                    let factor = crate::formulas::missile_damage_factor(
                        target_sig,
                        tgt_ball.velocity.length(),
                        explosion_radius,
                        explosion_velocity,
                        drf,
                    );
                    (
                        attacker_id,
                        npc_type_id,
                        target_id,
                        module_type_id,
                        None,
                        fx_guid,
                        cycle_duration_s,
                        shot_damage.scaled(factor),
                        false,
                    )
                }
                AiDamageAction::Missile {
                    attacker_id,
                    npc_type_id,
                    target_id,
                    missile_type_id,
                    fx_guid,
                    cycle_duration_s,
                    volley_damage,
                    max_flight_range,
                    explosion_radius,
                    explosion_velocity,
                    drf,
                } => {
                    let (Some(atk_ball), Some(tgt_ball)) =
                        (bp.balls.get(&attacker_id), bp.balls.get(&target_id))
                    else {
                        continue;
                    };
                    if atk_ball.surface_distance(tgt_ball) > max_flight_range {
                        continue;
                    }
                    let target_sig = resolve_target_sig_radius(ball_kinematics, target_id, tgt_ball);
                    let factor = crate::formulas::missile_damage_factor(
                        target_sig,
                        tgt_ball.velocity.length(),
                        explosion_radius,
                        explosion_velocity,
                        drf,
                    );
                    (
                        attacker_id,
                        npc_type_id,
                        target_id,
                        None,
                        Some(missile_type_id),
                        fx_guid,
                        cycle_duration_s,
                        volley_damage.scaled(factor),
                        false,
                    )
                }
            };

            damaged_target_ids.push(tid);
            bp.special_fx_meta.insert(
                (attacker_id, None, fx_guid.clone()),
                eve_destiny::SpecialFxWireMeta {
                    module_type_id,
                    target_id: Some(tid),
                    other_type_id,
                    is_offensive: true,
                    duration_ms: Some(cycle_duration_s * 1000.0),
                    repeat: Some(1),
                },
            );
            bp.emit_event(
                Some(attacker_id),
                eve_destiny::DestinyEvent::OnSpecialFX {
                    ball_id: attacker_id,
                    module_id: None,
                    guid: fx_guid,
                    is_active: true,
                },
            );
            if resolved_dmg.total() > 0.0 {
                if let Some((applied, destroyed, dead_ball_opt)) = bp.apply_damage_to_ball(tid, resolved_dmg) {
                    let (s_r, a_r, h_r) = combat_state_ratios(&bp, tid);
                    applied_hits.push((
                        attacker_id,
                        npc_type_id,
                        tid,
                        applied,
                        is_wrecking,
                        s_r,
                        a_r,
                        h_r,
                    ));
                    if destroyed {
                        if let Some(dead_ball) = dead_ball_opt {
                            destroyed_balls.push((attacker_id, tid, dead_ball));
                        }
                    }
                }
            } else {
                let (s_r, a_r, h_r) = combat_state_ratios(&bp, tid);
                applied_hits.push((attacker_id, npc_type_id, tid, 0.0, false, s_r, a_r, h_r));
            }
        }
        let deact_guard = sys_handle
            .module_deactivation_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        crate::system_actor::reconcile_tackle_and_bubbles(
            &mut bp,
            &mut st.active_module_cycles,
            &st.base_scramble_strengths,
            &st.base_core_strengths,
            &st.db_opt,
            deact_guard.as_ref(),
        );
        crate::system_actor::sync_ball_positions(
            &bp,
            &sys_handle.ball_positions,
            &sys_handle.scrambled_balls,
        );
    }

    if !applied_hits.is_empty() {
        for (attacker_id, npc_type_id, tid, applied, is_wrecking, s_r, a_r, h_r) in applied_hits {
            let victim_char_opt = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&tid)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.characters
                            .values()
                            .find(|c| c.active_ship_id == tid)
                            .map(|c| c.character_id)
                    })
                    .or_else(|| {
                        snap.items
                            .get(&tid)
                            .map(|i| i.owner_id)
                            .filter(|id| snap.characters.contains_key(id))
                    })
            });
            crate::services::combat_crimewatch::emit_combat_log_and_damage_state(
                ctx,
                attacker_id,
                victim_char_opt,
                attacker_id,
                tid,
                npc_type_id,
                applied,
                is_wrecking,
                s_r,
                a_r,
                h_r,
            );
        }
    }

    let now_unix = ctx.now_unix();
    if !damaged_target_ids.is_empty() {
        let hit_chars: Vec<i64> = ctx.db.with_snapshot(|snap| {
            damaged_target_ids
                .iter()
                .filter_map(|&tid| {
                    snap.characters
                        .get(&tid)
                        .map(|c| c.character_id)
                        .or_else(|| {
                            snap.characters
                                .values()
                                .find(|c| c.active_ship_id == tid)
                                .map(|c| c.character_id)
                        })
                })
                .collect()
        });
        for cid in hit_chars {
            crate::sp6_lifecycle::record_npc_aggression_timer(ctx, cid, now_unix);
        }
    }

    destroyed_balls
}

fn finalize_ai_step_cleanup(
    ctx: &ServerContext,
    sys_id: u32,
    actions: AiCleanupActions,
    now_unix: u64,
) {
    if !actions.clone_soldier_cooldowns.is_empty() {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            for until in actions.clone_soldier_cooldowns {
                rt.clone_soldier_cooldown_until.insert(sys_id, until);
            }
        });
    }
    for (attacker_npc_id, entity) in actions.destroyed_tracked_npcs {
        let _ = finalize_destroyed_npc_side_effects(
            ctx,
            sys_id,
            &entity,
            attacker_npc_id,
            now_unix,
        );
    }
    if !actions.remaining_destructions.is_empty() {
        handle_npc_ai_ball_destructions(ctx, sys_id, actions.remaining_destructions);
    }
}

fn finalize_destroyed_npc_side_effects(
    ctx: &ServerContext,
    sys_id: u32,
    entity: &NpcCombatEntity,
    killer_char_id: i64,
    now_unix: u64,
) -> Result<f64, String> {
    let supp_mult = crate::services::security_mgr::insurgency_suppression_bounty_and_sec_multiplier(ctx, sys_id);
    let bounty = entity.bounty_isk * supp_mult;
    let npc_ball_id = entity.ball_id;

    let res = ctx.db.try_with_mut_snapshot(|snap| {
            crate::services::pve_ess_reserve_banks::credit_ballpark_npc_bounty_with_ess_split(
                &ctx.sde,
                snap,
                sys_id,
                killer_char_id,
                bounty,
                entity.bounty_isk,
                supp_mult,
                entity.type_id,
                now_unix,
            );

            // Persist Killmail with dynamic bounty value (NOT flat 100k)
            let kid = snap.alloc_entity_id();
            snap.record_killmail(
                KillmailRecord {
                    kill_id: kid,
                    victim_character_id: npc_ball_id,
                    attacker_character_id: killer_char_id,
                    solar_system_id: sys_id,
                    ship_type_id: entity.type_id,
                    total_value_isk: bounty,
                    ..Default::default()
                },
                now_unix,
            );

            if !(32_000_000..=32_999_999).contains(&sys_id) {
                // Spawn physical NPC wreck item (26557)
                let wreck_id = snap.alloc_entity_id();
                crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                    snap, wreck_id, now_unix, true,
                );
                snap.items.insert(
                    wreck_id,
                    InventoryItemRecord {
                        item_id: wreck_id,
                        type_id: 26557,
                        owner_id: killer_char_id,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                snap.item_positions.insert(
                    wreck_id,
                    [entity.spawn_pos.x, entity.spawn_pos.y, entity.spawn_pos.z],
                );

                // Populate NPC loot table drops inside the wreck item (feat:npc.npc-loot)
                let ammo_type_id = match entity.faction_id {
                    500010 | 500001 => 204, // Scourge Heavy Missile
                    500012 | 500019 | 500003 => 178, // Radio S
                    500020 | 500004 => 230, // Antimatter Charge S
                    500011 | 500002 => 213, // Phased Plasma S
                    _ => 212, // Iron Charge S
                };
                let loot_drops = [(15331, 10), (ammo_type_id, 100)];
                for &(lid_type, lid_qty) in &loot_drops {
                    let lid = snap.alloc_entity_id();
                    snap.items.insert(
                        lid,
                        InventoryItemRecord {
                            item_id: lid,
                            type_id: lid_type,
                            owner_id: killer_char_id,
                            location_id: wreck_id,
                            flag: InventoryFlag::Cargo,
                            quantity: lid_qty,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                }
            }

            Ok::<_, String>(bounty)
        });
    if res.is_ok() && killer_char_id > 0 {
        crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
            ctx,
            killer_char_id,
            &["DamageNPCShips", "DestroyNPCShips"],
            1,
            Some(sys_id as i64),
        );
        crate::services::agents_missions::on_mission_npc_destroyed_core(
            &ctx.db,
            sys_id,
            npc_ball_id,
            Some(killer_char_id),
        );
        crate::services::agents_missions::agents_standings_arcs::apply_npc_kill_faction_standing_loss(
            ctx,
            sys_id,
            killer_char_id,
            entity.type_id,
            (entity.faction_id > 0).then_some(entity.faction_id),
            None,
            now_unix,
        );
        crate::quasar_gateway::isolate_side_effect("combat.npc_ai.npc_destroyed_progression", || {
            ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(
                killer_char_id as u32,
                1,
                1,
                &ctx.quasar_gateway,
            );
            let ship_group_id = crate::services::quasar_career_goals::character_active_ship_group_id(
                &ctx.db,
                &ctx.sde,
                killer_char_id,
            );
            let mut combat_ev = crate::services::quasar_career_goals::solar_system_career_event(
                &ctx.sde,
                "combat",
                sys_id,
            );
            combat_ev.ship_group_id = ship_group_id;
            ctx.quasar_gateway.progression.career_goals.on_career_gameplay_event(
                killer_char_id as u32,
                combat_ev,
                1,
                &ctx.quasar_gateway,
            );
            if crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
                ctx,
                entity.type_id,
            ) == "Triglavian"
            {
                crate::services::quasar_career_goals::record_career_sub_kind(
                    ctx,
                    killer_char_id,
                    "abyssal",
                    Some("kill_triglavians"),
                    1,
                );
            }
            ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(
                killer_char_id as u32,
                &["destroyHostileNPC", "entityGroupDestroyed"],
                1,
                ctx.now_unix(),
                &ctx.quasar_gateway,
            );
            let npc_gid = ctx
                .sde
                .types
                .get(&entity.type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            crate::services::operations_manager::record_server_operation_event(
                ctx,
                killer_char_id,
                "somethingExplode",
                &[
                    ("typeID", entity.type_id.to_string()),
                    ("groupID", npc_gid.to_string()),
                ],
                1,
            );
        });
    }
    res
}

/// Processes ball destructions returned by `bp.apply_damage_to_ball` during NPC AI ticks:
/// routes all destroyed balls through the unified [`crate::combat::death_pipeline::on_ball_destroyed_sync`]
/// pipeline (`#918`, `#835`, `#858`).
pub(crate) fn handle_npc_ai_ball_destructions(
    ctx: &ServerContext,
    sys_id: u32,
    destructions: Vec<(i64, i64, Ball)>,
) {
    for (attacker_npc_id, target_id, dead_ball) in destructions {
        let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
            ctx,
            sys_id,
            target_id,
            Some(attacker_npc_id),
            dead_ball.position,
        );
    }
}

pub(crate) fn compute_sleeper_sentry_interval_s(ctx: &ServerContext, npc_type_id: u32) -> f64 {
    ctx.sde
        .types
        .get(&npc_type_id)
        .and_then(|t| t.default_attributes.get(&51).copied())
        .unwrap_or(15_000.0)
        / 1000.0
}

#[cfg(test)] #[path = "npc_ai_unit_tests.rs"]
mod npc_ai_unit_tests;

#[cfg(test)]
#[path = "npc_ai_edges_tests.rs"]
mod npc_ai_edges_tests;

