//! Shared SDE NPC weapon profile factory, combat hit/FX evaluators,
//! extended NPC weapon runtime, and atomic size-scaled wreck & ammo loot builders.

#![forbid(unsafe_code)]

use crate::combat::npc_ai::{faction_damage_vector, regional_pirate_faction_id_for_sde};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_ephemeral, ExtendedNpcEntity};
use crate::services::wormhole_pve_seeding::is_any_capital_group;
use eve_destiny::{
    Ball, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, SpecialFxWireMeta,
};
use eve_sde::{AttributeId, EffectId, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

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

const MAX_LOGGED_FALLBACK_NPC_TYPES: usize = 16_384;

static LOGGED_FALLBACK_NPC_TYPES: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

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
        if npc_type
            .effect_ids
            .contains(&EFF_MISSILE_LAUNCHING_FOR_ENTITY)
        {
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
            if npc_type
                .effect_ids
                .contains(&EFF_TARGET_DISINTEGRATOR_ATTACK)
            {
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
            // Vorton Projectors apply only within optimal range.
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
pub(crate) struct ExtendedNpcWeaponRuntime {
    pub npc_type_id: u32,
    pub profile: NpcCombatProfile,
    pub uses_synthetic_legacy: bool,
    pub baseline_damage_vector: DamageVector,
    pub baseline_optimal_range: f64,
    pub turret_cooldown_s: f64,
    pub missile_cooldown_s: f64,
}

impl ExtendedNpcWeaponRuntime {
    pub(crate) fn decay_cooldowns(&mut self, elapsed_s: f64) {
        self.turret_cooldown_s = (self.turret_cooldown_s - elapsed_s).max(0.0);
        self.missile_cooldown_s = (self.missile_cooldown_s - elapsed_s).max(0.0);
    }
}

pub(crate) type ExtendedNpcWeaponRuntimeMap = HashMap<(u32, i64), ExtendedNpcWeaponRuntime>;
#[derive(Clone, Debug, Default)]
pub(crate) struct SharedExtendedNpcWeaponRuntime {
    pub(crate) map: std::sync::Arc<
        eve_db::lock_rank::RankedRwLock<
            ExtendedNpcWeaponRuntimeMap,
            { eve_db::lock_rank::RANK_LEAF },
        >,
    >,
    pub(crate) epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
pub(crate) type WeaponRuntimePruneSnapshot =
    (SharedExtendedNpcWeaponRuntime, HashMap<i64, (u32, u32)>, u64);

pub(crate) fn is_edencom_arc_combatant_type(sde: &SdeRegistry, type_id: u32) -> bool {
    if type_id == 26_468 {
        return false;
    }
    sde.types.get(&type_id).is_none_or(|t| {
        !matches!(t.group_id, 12 | 186 | 340 | 448 | 649)
            && matches!(t.category_id, 6 | 11 | 18 | 65 | 87)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NpcSizeClass {
    Frigate,
    Cruiser,
    Battleship,
    Capital,
}

impl NpcSizeClass {
    pub(crate) fn from_sde(sde: &SdeRegistry, type_id: Option<u32>) -> Self {
        let Some(t) = type_id.and_then(|tid| sde.types.get(&tid)) else {
            return Self::Frigate;
        };
        let sig = t
            .default_attributes
            .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
            .copied()
            .unwrap_or(0.0);
        if is_any_capital_group(t.group_id) || t.mass >= 250_000_000.0 {
            Self::Capital
        } else if t.name.contains("Battleship") {
            Self::Battleship
        } else if t.name.contains("Cruiser")
            || t.name.contains("Battlecruiser")
            || t.name.contains("Awakened")
        {
            Self::Cruiser
        } else if t.name.contains("Frigate")
            || t.name.contains("Destroyer")
            || t.name.contains("Emergent")
        {
            Self::Frigate
        } else if sig >= 2_500.0 {
            Self::Capital
        } else if sig >= 240.0 || t.mass >= 18_000_000.0 {
            Self::Battleship
        } else if sig >= 80.0 || t.mass >= 4_000_000.0 {
            Self::Cruiser
        } else {
            Self::Frigate
        }
    }

    pub(crate) fn scrap_quantity(self) -> i32 {
        // DERIVED: Size-scaled Metal Scraps salvage count by hull class.
        match self {
            Self::Frigate => 2,
            Self::Cruiser => 5,
            Self::Battleship => 10,
            Self::Capital => 20,
        }
    }

    pub(crate) fn ammo_quantity(self, kind: NpcWeaponKind) -> i32 {
        // DERIVED: Size-scaled charge/crystal loot count matched to NPC hull class and weapon kind.
        if kind == NpcWeaponKind::Laser {
            match self {
                Self::Frigate | Self::Cruiser => 1,
                Self::Battleship => 2,
                Self::Capital => 3,
            }
        } else {
            match self {
                Self::Frigate => 25,
                Self::Cruiser => 60,
                Self::Battleship => 120,
                Self::Capital => 250,
            }
        }
    }
}

pub(crate) fn damage_vector_approx_eq(a: DamageVector, b: DamageVector) -> bool {
    (a.em - b.em).abs() < 1e-6
        && (a.thermal - b.thermal).abs() < 1e-6
        && (a.kinetic - b.kinetic).abs() < 1e-6
        && (a.explosive - b.explosive).abs() < 1e-6
}

pub(crate) fn sum_damage_vectors(a: DamageVector, b: DamageVector) -> DamageVector {
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

    if npc_type
        .effect_ids
        .contains(&EFF_TARGET_DISINTEGRATOR_ATTACK)
    {
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
    let th = a
        .get(&eve_sde::attr_ids::THERMAL_DAMAGE)
        .copied()
        .unwrap_or(0.0);
    let kin = a
        .get(&eve_sde::attr_ids::KINETIC_DAMAGE)
        .copied()
        .unwrap_or(0.0);
    let exp = a
        .get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE)
        .copied()
        .unwrap_or(0.0);
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
    let shot_damage = DamageVector::new(
        em * dmg_mult,
        th * dmg_mult,
        kin * dmg_mult,
        exp * dmg_mult,
    );
    let cycle_ms = if let Some(ms) = a
        .get(&eve_sde::attr_ids::SPEED)
        .copied()
        .filter(|&v| v > 0.0)
    {
        ms
    } else if let Some(ms) = a
        .get(&eve_sde::attr_ids::DURATION)
        .copied()
        .filter(|&v| v > 0.0)
    {
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
    let raw_trk = a
        .get(&eve_sde::attr_ids::TRACKING_SPEED)
        .copied()
        .filter(|&v| v > 0.0);
    let (
        signature_resolution,
        tracking_speed,
        vorton_explosion_radius,
        vorton_explosion_velocity,
        vorton_drf,
    ) = if kind == NpcWeaponKind::Vorton {
        let er = if let Some(v) = a
            .get(&ATTR_MISSILE_AOE_CLOUD_SIZE)
            .copied()
            .filter(|&v| v > 0.0)
        {
            v
        } else {
            fallbacks.push("aoeCloudSize(654)");
            43.0
        };
        let ev = if let Some(v) = a
            .get(&ATTR_MISSILE_AOE_VELOCITY)
            .copied()
            .filter(|&v| v > 0.0)
        {
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
    let has_missile_effect = npc_type
        .effect_ids
        .contains(&EFF_MISSILE_LAUNCHING_FOR_ENTITY);
    if !has_missile_effect && launch_dur_opt.unwrap_or(0.0) <= 100.0 {
        return None;
    }
    let Some(m_type) = sde.types.get(&missile_type_id) else {
        fallbacks.push("entityMissileTypeID(507)");
        return None;
    };
    let ma = &m_type.default_attributes;
    if ma
        .get(&ATTR_MISSILE_NEVER_DOES_DAMAGE)
        .is_some_and(|&v| v > 0.0)
    {
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
    let m_th = ma
        .get(&eve_sde::attr_ids::THERMAL_DAMAGE)
        .copied()
        .unwrap_or(0.0);
    let m_kin = ma
        .get(&eve_sde::attr_ids::KINETIC_DAMAGE)
        .copied()
        .unwrap_or(0.0);
    let m_exp = ma
        .get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE)
        .copied()
        .unwrap_or(0.0);
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

    let m_vel = if let Some(v) = ma
        .get(&eve_sde::attr_ids::MAX_VELOCITY)
        .copied()
        .filter(|&v| v > 0.0)
    {
        v
    } else {
        fallbacks.push("missileMaxVelocity(37)");
        3_000.0
    };
    let vel_mult = a
        .get(&ATTR_MISSILE_VELOCITY_MULT)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(1.0);
    let m_flight_ms = if let Some(ms) = ma
        .get(&ATTR_MISSILE_EXP_DELAY_MS)
        .copied()
        .filter(|&v| v > 0.0)
    {
        ms
    } else {
        fallbacks.push("missileFlightTime(281)");
        5_000.0
    };
    let flight_mult = a
        .get(&ATTR_MISSILE_FLIGHT_TIME_MULT)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(1.0);
    let max_flight_range = ((m_vel * vel_mult) * ((m_flight_ms / 1000.0) * flight_mult)).max(0.0);

    let m_er = if let Some(er) = ma
        .get(&ATTR_MISSILE_AOE_CLOUD_SIZE)
        .copied()
        .filter(|&v| v > 0.0)
    {
        er
    } else {
        fallbacks.push("aoeCloudSize(654)");
        100.0
    };
    let er_mult = a
        .get(&ATTR_MISSILE_EXP_RADIUS_MULT)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(1.0);
    let m_ev = if let Some(ev) = ma
        .get(&ATTR_MISSILE_AOE_VELOCITY)
        .copied()
        .filter(|&v| v > 0.0)
    {
        ev
    } else {
        fallbacks.push("aoeVelocity(653)");
        100.0
    };
    let ev_mult = a
        .get(&ATTR_MISSILE_EXP_VELOCITY_MULT)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(1.0);
    let damage_reduction_factor = if let Some(drf) = ma
        .get(&ATTR_MISSILE_AOE_DRF)
        .copied()
        .filter(|&v| v > 0.0)
    {
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

/// Pure, side-effect-free SDE profile inspection for an NPC type.
/// Returns the computed combat/movement profile alongside any missing attribute fallback labels.
fn is_synthetic_unfitted_test_stub(t: &eve_sde::InvType) -> bool {
    const REAL_SDE_ENTITY_ATTRS: &[u16] = &[
        9,   // structureHP
        263, // shieldCapacity
        265, // armorHP
        37,  // maxVelocity
        552, // signatureRadius
        854, // disallowAssistance
        114, 116, 117, 118, // turret damage
        507, // entityMissileTypeID
    ];
    t.published
        && t.category_id == 11
        && !REAL_SDE_ENTITY_ATTRS
            .iter()
            .any(|aid| t.default_attributes.contains_key(aid))
}

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

    let is_stub = is_synthetic_unfitted_test_stub(t);
    if t.category_id != 11 || is_stub {
        if is_stub {
            fallbacks.push("synthetic_stub_no_dogma");
        }
        let max_vel = a
            .get(&eve_sde::attr_ids::MAX_VELOCITY)
            .copied()
            .unwrap_or(320.0);
        let cruise_vel = a
            .get(&ATTR_ENTITY_CRUISE_SPEED)
            .copied()
            .filter(|&v| v > 0.0)
            .unwrap_or(max_vel);
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
    let cruise_opt = a
        .get(&ATTR_ENTITY_CRUISE_SPEED)
        .copied()
        .filter(|&v| v > 0.0);
    let is_stationary_group = matches!(t.group_id, 99 | 180 | 319 | 335 | 383 | 495 | 4576);

    let max_velocity = if let Some(v) = a
        .get(&eve_sde::attr_ids::MAX_VELOCITY)
        .copied()
        .filter(|&v| v >= 0.0)
    {
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
    let set = fallback_log_registry();
    let newly_inserted = {
        let mut guard = set.lock().unwrap_or_else(|e| e.into_inner());
        if guard.contains(&type_id) {
            false
        } else {
            if guard.len() >= MAX_LOGGED_FALLBACK_NPC_TYPES {
                if let Some(&evict) = guard.iter().next() {
                    guard.remove(&evict);
                }
            }
            guard.insert(type_id)
        }
    };
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

pub(crate) fn build_npc_ball_from_sde(
    sde: &SdeRegistry,
    ball_id: i64,
    type_id: u32,
    position: DVec3,
    max_velocity: f64,
    speed_fraction: f64,
) -> Ball {
    let sde_t = sde.types.get(&type_id);
    let inertia = sde_t
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied())
        .filter(|&a| a > 0.0)
        .unwrap_or(2.5);
    let mass = sde_t
        .map(|t| t.mass)
        .filter(|&m| m > 0.0)
        .unwrap_or(1_500_000.0);
    let radius = sde_t
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(50.0);
    Ball {
        id: ball_id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity,
        inertia,
        mass,
        radius,
        speed_fraction,
        mode: MovementMode::Stop,
    }
}

fn expected_specialized_optimal_range(
    sde: &SdeRegistry,
    npc_type_id: u32,
    archetype: &str,
    profile: &NpcCombatProfile,
) -> f64 {
    if archetype == "TransmissionRelay" {
        return 0.0;
    }
    let sde_type = sde.types.get(&npc_type_id);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    if !profile.is_synthetic_fallback {
        match archetype {
            "SleeperSentry" | "EmpireSentry" => {
                attr(54).filter(|&v| v > 0.0).unwrap_or(profile.engagement_optimal_range)
            }
            _ => profile.engagement_optimal_range,
        }
    } else {
        match archetype {
            "SleeperSentry" => attr(54).unwrap_or(250_000.0),
            "EmpireSentry" => attr(54).unwrap_or(500_000.0),
            _ => attr(54).unwrap_or(15_000.0).max(5_000.0),
        }
    }
}

fn specialized_profile_shot_damage(profile: &NpcCombatProfile) -> DamageVector {
    let turret_shot = profile
        .turret
        .as_ref()
        .map(|w| w.shot_damage)
        .unwrap_or_default();
    let missile_shot = profile
        .missile
        .as_ref()
        .map(|m| m.volley_damage)
        .unwrap_or_default();
    sum_damage_vectors(turret_shot, missile_shot)
}

fn is_empire_sentry_faction(faction_id: u32) -> bool {
    matches!(faction_id, 500_001..=500_004 | 500_006..=500_008)
}

/// Resolves an empire faction ID from an NPC type name or Navy archetype when SDE `faction_id` is absent.
fn resolve_empire_faction_from_name(type_name: &str, archetype: &str) -> Option<u32> {
    if type_name.contains("Republic Fleet")
        || type_name.contains("Minmatar")
        || archetype == "MinmatarNavy"
    {
        Some(500_002)
    } else if type_name.contains("Imperial Navy")
        || type_name.contains("Amarr")
        || archetype == "AmarrNavy"
    {
        Some(500_003)
    } else if type_name.contains("Federal Navy")
        || type_name.contains("Gallente")
        || archetype == "GallenteNavy"
    {
        Some(500_004)
    } else if type_name.contains("Caldari")
        || type_name.contains("State Navy")
        || type_name.contains("State Police")
        || type_name.contains("State Peacekeeper")
        || archetype == "CaldariNavy"
    {
        Some(500_001)
    } else if type_name.contains("CONCORD") {
        Some(500_006)
    } else if type_name.contains("Ammatar") {
        Some(500_007)
    } else if type_name.contains("Khanid") {
        Some(500_008)
    } else {
        None
    }
}

/// Resolves a system's governing empire faction from station, constellation, or region SDE ownership.
fn resolve_system_empire_faction_sde(sde: &SdeRegistry, solar_system_id: u32) -> u32 {
    sde.solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            sys.stations
                .iter()
                .find_map(|st| {
                    st.owner_faction_id
                        .filter(|&f| f > 0)
                        .map(|f| f as u32)
                        .filter(|&f| is_empire_sentry_faction(f))
                })
                .or_else(|| {
                    sde.constellations
                        .get(&sys.constellation_id)
                        .and_then(|c| c.faction_id.filter(|&f| is_empire_sentry_faction(f)))
                })
                .or_else(|| {
                    sde.regions
                        .get(&sys.region_id)
                        .and_then(|r| r.faction_id.filter(|&f| is_empire_sentry_faction(f)))
                })
        })
        .unwrap_or(500_001)
}

/// Builds `(Ball, CombatState, ExtendedNpcEntity)` directly from [`SdeRegistry`] for activation-time spawning.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_specialized_npc_components_for_sde(
    sde: &SdeRegistry,
    solar_system_id: u32,
    ball_id: i64,
    npc_type_id: u32,
    archetype: &str,
    spawn_group_id: u32,
    pos: DVec3,
    rng_seed: u64,
) -> (Ball, CombatState, ExtendedNpcEntity) {
    let type_name = sde
        .types
        .get(&npc_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("");
    let empire_faction_override = match archetype {
        "Navy"
        | "FactionPolice"
        | "Police"
        | "CaldariNavy"
        | "MinmatarNavy"
        | "AmarrNavy"
        | "GallenteNavy" => Some(
            resolve_empire_faction_from_name(type_name, archetype).unwrap_or(500_001),
        ),
        // Gate/station sentry types in EVE SDE omit `faction_id`; resolve empire sovereignty
        // rather than falling back to regional pirate factions.
        "EmpireSentry" => Some(
            resolve_empire_faction_from_name(type_name, archetype).unwrap_or_else(|| {
                if is_empire_sentry_faction(spawn_group_id) {
                    spawn_group_id
                } else {
                    resolve_system_empire_faction_sde(sde, solar_system_id)
                }
            }),
        ),
        _ => None,
    };
    build_specialized_npc_components_for_sde_with_empire_hint(
        sde,
        solar_system_id,
        ball_id,
        npc_type_id,
        archetype,
        spawn_group_id,
        pos,
        empire_faction_override,
        rng_seed,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_specialized_npc_components_for_sde_with_empire_hint(
    sde: &SdeRegistry,
    solar_system_id: u32,
    ball_id: i64,
    npc_type_id: u32,
    archetype: &str,
    spawn_group_id: u32,
    pos: DVec3,
    empire_faction_override: Option<u32>,
    rng_seed: u64,
) -> (Ball, CombatState, ExtendedNpcEntity) {
    let sde_type = sde.types.get(&npc_type_id);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    let is_static_structure =
        matches!(archetype, "SleeperSentry" | "EmpireSentry" | "TransmissionRelay");
    let is_navy_or_police = matches!(
        archetype,
        "Navy"
            | "FactionPolice"
            | "Police"
            | "CaldariNavy"
            | "MinmatarNavy"
            | "AmarrNavy"
            | "GallenteNavy"
    );
    let resolved_faction_id =
        if is_navy_or_police && (500_001..=500_030).contains(&spawn_group_id) {
            spawn_group_id
        } else {
            sde_type
                .and_then(|t| t.faction_id)
                .filter(|&f| !(is_navy_or_police && f == 500_027))
                .unwrap_or_else(|| match archetype {
                    "Navy"
                    | "FactionPolice"
                    | "Police"
                    | "CaldariNavy"
                    | "MinmatarNavy"
                    | "AmarrNavy"
                    | "GallenteNavy"
                    | "EmpireSentry" => empire_faction_override.unwrap_or(500_001),
                    "Sleeper" | "PochvenHypnosianSleeper" => 500_021,
                    "RogueDrone" => 500_025,
                    "EDENCOM" => 500_027,
                    "Drifter" => 500_024,
                    "Triglavian" => 500_026,
                    _ => {
                        if (500_001..=500_030).contains(&spawn_group_id) {
                            spawn_group_id
                        } else {
                            regional_pirate_faction_id_for_sde(sde, solar_system_id)
                        }
                    }
                })
        };

    let bounty = if matches!(
        archetype,
        "Sleeper" | "SleeperSentry" | "EmpireSentry" | "TransmissionRelay"
    ) {
        0.0
    } else {
        attr(481).unwrap_or(0.0)
    };
    let profile = resolve_npc_combat_profile(sde, npc_type_id, Some(bounty), resolved_faction_id);

    let max_vel = if is_static_structure {
        0.0
    } else if !profile.is_synthetic_fallback {
        profile.max_velocity
    } else {
        attr(37).unwrap_or(350.0)
    };

    let (opt_range, orb_range, damage_vector) = if archetype == "TransmissionRelay" {
        (0.0, 0.0, DamageVector::default())
    } else if !profile.is_synthetic_fallback {
        let opt = expected_specialized_optimal_range(sde, npc_type_id, archetype, &profile);
        let orb = if is_static_structure {
            0.0
        } else {
            profile.orbit_range
        };
        let dmg = specialized_profile_shot_damage(&profile);
        (opt, orb, dmg)
    } else {
        let (raw_em, raw_exp, raw_kin, raw_thm) = (
            attr(114).unwrap_or(0.0),
            attr(116).unwrap_or(0.0),
            attr(117).unwrap_or(0.0),
            attr(118).unwrap_or(0.0),
        );
        let dmg_mult = attr(64).filter(|&m| m > 0.0).unwrap_or(1.0);
        // Legacy fixture 56680 (`abs2_t1_enviroment_asset`, category 2) is reused as
        // `PochvenHypnosianSleeper` in Pochven encounters and tests.
        let is_non_npc_asset = sde_type.is_some_and(|t| t.category_id == 2)
            && !(npc_type_id == 56_680 && archetype == "PochvenHypnosianSleeper");
        let opt = match archetype {
            "SleeperSentry" => attr(54).unwrap_or(250_000.0),
            "EmpireSentry" => attr(54).unwrap_or(500_000.0),
            _ if is_non_npc_asset && raw_em + raw_exp + raw_kin + raw_thm <= 0.0 => 0.0,
            _ => attr(54).unwrap_or(15_000.0).max(5_000.0),
        };
        let orb = if is_static_structure || opt <= 0.0 {
            0.0
        } else {
            (opt * 0.6).clamp(2_500.0, 20_000.0)
        };
        let dmg = if raw_em + raw_exp + raw_kin + raw_thm > 0.0 {
            DamageVector {
                em: raw_em * dmg_mult,
                explosive: raw_exp * dmg_mult,
                kinetic: raw_kin * dmg_mult,
                thermal: raw_thm * dmg_mult,
            }
        } else if !is_non_npc_asset && profile.is_synthetic_fallback {
            // Synthetic archetype baseline only for synthetic IDs or dogma-less test fixtures.
            match archetype {
                "Triglavian" => DamageVector::new(0.0, 75.0, 0.0, 75.0),
                "EDENCOM" => DamageVector::new(60.0, 0.0, 60.0, 0.0),
                "Drifter" | "Sleeper" | "IncursionSansha" => {
                    DamageVector::new(50.0, 50.0, 50.0, 50.0)
                }
                "CloneSoldier" | "Pirate" | "BeltRat" | "Officer" => {
                    faction_damage_vector(resolved_faction_id, 100.0)
                }
                _ => DamageVector::new(25.0, 25.0, 25.0, 25.0),
            }
        } else {
            DamageVector::default()
        };
        (opt, orb, dmg)
    };

    let tag_drop = if archetype == "CloneSoldier" {
        let name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
        let tier_str = if name.contains("Trainer") {
            "Trainer"
        } else if name.contains("Recruiter") {
            "Recruiter"
        } else if name.contains("Transporter") {
            "Transporter"
        } else {
            "Negotiator"
        };
        sde.types
            .values()
            .find(|t| {
                t.group_id == 1206
                    && t.name.contains("Clone Soldier")
                    && t.name.contains(tier_str)
            })
            .map(|t| t.type_id)
    } else {
        None
    };

    // Resolve EWAR from SDE dogma attributes (`20`=speedFactor, `514`=modifyTargetSpeedRange,
    // `105`=warpScrambleStrength, `103`=warpScrambleRange). Only Sleeper archetypes and named
    // Clone Soldier tiers fall back when SDE dogma is omitted; regular NPCs without EWAR attrs
    // default strictly to 0.0.
    let resolve_web = |default_factor: f64| -> (f64, f64) {
        let factor = attr(20).filter(|&v| v < 0.0).unwrap_or(default_factor);
        if factor < 0.0 {
            let range = attr(514).or_else(|| attr(580)).unwrap_or(10_000.0);
            if range > 0.0 {
                (factor, range)
            } else {
                (0.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        }
    };
    let resolve_scram = |default_strength: f64| -> (f64, f64) {
        let strength = attr(105).filter(|&v| v > 0.0).unwrap_or(default_strength);
        if strength > 0.0 {
            let range = attr(103).or_else(|| attr(582)).unwrap_or(20_000.0);
            if range > 0.0 {
                (strength, range)
            } else {
                (0.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        }
    };
    let (web_factor, web_range, scram_str, scram_range) =
        if is_static_structure || npc_type_id == 79_507 {
            (0.0, 0.0, 0.0, 0.0)
        } else if archetype == "CloneSoldier" {
            let name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
            if name.contains("Transporter") {
                let (wf, wr) = resolve_web(-60.0);
                (wf, wr, 0.0, 0.0)
            } else if name.contains("Negotiator") {
                let ss = attr(105).filter(|&v| v > 0.0).unwrap_or(2.0).max(2.0);
                let sr = attr(103).filter(|&v| v > 0.0).unwrap_or(24_000.0);
                (0.0, 0.0, ss, sr)
            } else {
                let (wf, wr) = resolve_web(0.0);
                (wf, wr, 0.0, 0.0)
            }
        } else if archetype == "Sleeper" {
            let (wf, wr) = resolve_web(-50.0);
            let (ss, sr) = resolve_scram(2.0);
            (wf, wr, ss, sr)
        } else {
            let (wf, wr) = resolve_web(0.0);
            let (ss, sr) = resolve_scram(0.0);
            (wf, wr, ss, sr)
        };

    let mut cs = ServerContext::build_type_combat_state_from_sde_seeded(sde, npc_type_id, rng_seed);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 2_000.0;
        cs.shield_hp = 2_000.0;
        cs.max_armor_hp = 2_000.0;
        cs.armor_hp = 2_000.0;
        cs.max_structure_hp = 2_000.0;
        cs.structure_hp = 2_000.0;
    }
    if archetype == "SleeperSentry" {
        cs.signature_radius = attr(552).unwrap_or(50.0);
    }
    if archetype == "PochvenHypnosianSleeper" {
        let name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
        if name.contains("Aroused") || npc_type_id == 56680 || (56319..=56327).contains(&npc_type_id)
        {
            cs.max_shield_hp = 5_000.0;
            cs.shield_hp = 5_000.0;
        }
    }

    let ball = build_npc_ball_from_sde(sde, ball_id, npc_type_id, pos, max_vel, 0.0);
    let wh_class = sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|s| {
            sde.constellations
                .get(&s.constellation_id)
                .and_then(|c| c.wormhole_class_id)
                .or_else(|| {
                    sde.regions
                        .get(&s.region_id)
                        .and_then(|r| r.wormhole_class_id)
                })
        })
        // Known-space systems carry no wormhole class: never default Sleepers to C3 behaviour.
        .unwrap_or(0) as u8;

    let remote_shield_rep = if is_static_structure {
        0.0
    } else {
        attr(1460).or_else(|| attr(68)).unwrap_or(0.0)
    };
    let remote_armor_rep = if is_static_structure {
        0.0
    } else {
        attr(1455).or_else(|| attr(84)).unwrap_or(0.0)
    };
    let remote_rep_range = if remote_shield_rep <= 0.0 && remote_armor_rep <= 0.0 {
        0.0
    } else {
        attr(1464).unwrap_or(25_000.0)
    };

    let entity = ExtendedNpcEntity {
        ball_id,
        type_id: npc_type_id,
        faction_id: resolved_faction_id,
        archetype: archetype.to_string(),
        solar_system_id,
        spawn_group_id,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: bounty,
        optimal_range: opt_range,
        orbit_range: orb_range,
        damage_vector,
        entropic_cycles: 0,
        drifter_overshield_hp: if archetype == "Drifter"
            && sde_type.is_none_or(|t| t.group_id != 1956)
        {
            4_000.0
        } else {
            0.0
        },
        drifter_doomsday_fired: false,
        web_speed_factor: web_factor,
        web_range,
        scram_strength: scram_str,
        scram_range,
        neut_amount: if is_static_structure || npc_type_id == 79_507 {
            0.0
        } else if archetype == "Sleeper" {
            attr(97).filter(|&v| v >= 0.0).unwrap_or(120.0)
        } else {
            attr(97).filter(|&v| v > 0.0).unwrap_or(0.0)
        },
        ecm_jammed: false,
        remote_shield_rep,
        remote_armor_rep,
        remote_rep_range,
        tag_drop_type_id: tag_drop,
        spawn_position: pos,
        sleeper_wh_class: wh_class,
        custom_cargo_loot: None,
    };
    (ball, cs, entity)
}

pub(crate) fn build_specialized_npc_components_with_empire_hint(
    ctx: &ServerContext,
    solar_system_id: u32,
    ball_id: i64,
    npc_type_id: u32,
    archetype: &str,
    spawn_group_id: u32,
    pos: DVec3,
    empire_faction_override: Option<u32>,
) -> (Ball, CombatState, ExtendedNpcEntity) {
    let seq = ctx
        .combat_rng_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    build_specialized_npc_components_for_sde_with_empire_hint(
        &ctx.sde,
        solar_system_id,
        ball_id,
        npc_type_id,
        archetype,
        spawn_group_id,
        pos,
        empire_faction_override,
        seq ^ (u64::from(npc_type_id) << 32),
    )
}

pub(crate) fn emit_npc_special_fx(
    bp: &mut Ballpark,
    attacker_id: i64,
    target_id: i64,
    module_type_id: Option<u32>,
    other_type_id: Option<u32>,
    fx_guid: String,
    cycle_duration_s: f64,
) {
    bp.insert_special_fx_meta(
        (attacker_id, None, fx_guid.clone()),
        SpecialFxWireMeta {
            module_type_id,
            target_id: Some(target_id),
            other_type_id,
            is_offensive: true,
            duration_ms: Some(cycle_duration_s * 1000.0),
            repeat: Some(1),
        },
    );
    bp.emit_event(
        Some(attacker_id),
        DestinyEvent::OnSpecialFX {
            ball_id: attacker_id,
            module_id: None,
            guid: fx_guid,
            is_active: true,
        },
    );
}

pub(crate) fn resolve_live_target_sig_radius(
    bp: &Ballpark,
    target_sig_radii: &HashMap<i64, f64>,
    target_id: i64,
    tgt_ball: &Ball,
) -> f64 {
    if let Some(cs) = bp.combat_states.get(&target_id) {
        if (cs.base_signature_radius.is_some() || cs.tp_sig_multiplier.is_some())
            && cs.signature_radius > 0.0
        {
            return cs.signature_radius.max(1.0);
        }
    }
    target_sig_radii
        .get(&target_id)
        .copied()
        .filter(|&s| s > 0.0)
        .or_else(|| {
            bp.combat_states
                .get(&target_id)
                .map(|cs| cs.signature_radius)
                .filter(|&s| s > 0.0)
        })
        .unwrap_or_else(|| tgt_ball.radius.max(25.0))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_npc_turret_shot(
    ctx: &ServerContext,
    bp: &Ballpark,
    target_sig_radii: &HashMap<i64, f64>,
    attacker_id: i64,
    target_id: i64,
    shot_damage: DamageVector,
    optimal_range: f64,
    falloff_range: f64,
    tracking_speed: f64,
    sig_resolution: f64,
    max_effective_range: f64,
) -> Option<(DamageVector, bool)> {
    let (atk_ball, tgt_ball) = (bp.balls.get(&attacker_id)?, bp.balls.get(&target_id)?);
    let surface_dist = atk_ball.surface_distance(tgt_ball);
    if surface_dist > max_effective_range {
        return None;
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
    let target_sig = resolve_live_target_sig_radius(bp, target_sig_radii, target_id, tgt_ball);
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
    Some(crate::formulas::turret_shot_damage(
        shot_damage,
        p_hit,
        roll_u,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_npc_missile_or_vorton_hit(
    bp: &Ballpark,
    target_sig_radii: &HashMap<i64, f64>,
    attacker_id: i64,
    target_id: i64,
    base_damage: DamageVector,
    max_range: f64,
    explosion_radius: f64,
    explosion_velocity: f64,
    drf: f64,
) -> Option<DamageVector> {
    let (atk_ball, tgt_ball) = (bp.balls.get(&attacker_id)?, bp.balls.get(&target_id)?);
    if atk_ball.surface_distance(tgt_ball) > max_range {
        return None;
    }
    let target_sig = resolve_live_target_sig_radius(bp, target_sig_radii, target_id, tgt_ball);
    let factor = crate::formulas::missile_damage_factor(
        target_sig,
        tgt_ball.velocity.length(),
        explosion_radius,
        explosion_velocity,
        drf,
    );
    Some(base_damage.scaled(factor))
}

fn clone_extended_npc_weapon_runtime_arc(ctx: &ServerContext) -> SharedExtendedNpcWeaponRuntime {
    let guard = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
    guard.extended_npc_weapon_runtime.clone()
}

pub(crate) fn with_extended_npc_weapon_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&ExtendedNpcWeaponRuntimeMap) -> R,
) -> R {
    let shared = clone_extended_npc_weapon_runtime_arc(ctx);
    let guard = shared.map.read().unwrap_or_else(|e| e.into_inner());
    f(&guard)
}

pub(crate) fn with_mut_extended_npc_weapon_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut ExtendedNpcWeaponRuntimeMap) -> R,
) -> R {
    let shared = clone_extended_npc_weapon_runtime_arc(ctx);
    let mut guard = shared.map.write().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub(crate) fn capture_wrt_prune(
    shared: &SharedExtendedNpcWeaponRuntime,
    npc_entities: &HashMap<i64, ExtendedNpcEntity>,
) -> WeaponRuntimePruneSnapshot {
    let seq = shared.epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let live = npc_entities
        .iter()
        .map(|(&id, n)| (id, (n.solar_system_id, n.type_id)))
        .collect();
    (shared.clone(), live, seq)
}

pub(crate) fn capture_wrt_prune_if_changed(
    was_unsynced: bool,
    old_npcs: &HashMap<i64, ExtendedNpcEntity>,
    next_npcs: &HashMap<i64, ExtendedNpcEntity>,
    next_shared: &SharedExtendedNpcWeaponRuntime,
) -> Option<WeaponRuntimePruneSnapshot> {
    let changed = (was_unsynced && next_npcs.is_empty())
        || old_npcs.len() != next_npcs.len()
        || old_npcs.iter().any(|(bid, old)| {
            next_npcs
                .get(bid)
                .is_none_or(|n| (n.solar_system_id, n.type_id) != (old.solar_system_id, old.type_id))
        });
    changed.then(|| capture_wrt_prune(next_shared, next_npcs))
}

pub(crate) fn prune_wrt_snap((shared, live, seq): WeaponRuntimePruneSnapshot) {
    let mut wrt = shared.map.write().unwrap_or_else(|e| e.into_inner());
    if shared.epoch.load(std::sync::atomic::Ordering::Acquire) == seq {
        wrt.retain(|&(sid, bid), entry| {
            live.get(&bid)
                .is_some_and(|&(nsid, ntid)| nsid == sid && ntid == entry.npc_type_id)
        });
    }
}

pub(crate) fn finalize_system_weapon_runtime_step(
    ctx: &ServerContext,
    solar_system_id: u32,
    cleared_ids: &[i64],
    destroyed_in_call: &HashSet<i64>,
    attacked_npc_ids: &HashSet<i64>,
) {
    let (shared, seq, live_in_sys): (_, u64, HashMap<i64, u32>) = {
        let guard = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        let seq = guard
            .extended_npc_weapon_runtime
            .epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let live = guard
            .npc_entities
            .values()
            .filter(|n| n.solar_system_id == solar_system_id)
            .map(|n| (n.ball_id, n.type_id))
            .collect();
        (guard.extended_npc_weapon_runtime.clone(), seq, live)
    };
    let mut wrt = shared.map.write().unwrap_or_else(|e| e.into_inner());
    for bid in cleared_ids {
        wrt.remove(&(solar_system_id, *bid));
    }
    if shared.epoch.load(std::sync::atomic::Ordering::Acquire) == seq {
        wrt.retain(|&(sid, bid), entry| {
            sid != solar_system_id
                || (live_in_sys.get(&bid) == Some(&entry.npc_type_id)
                    && !destroyed_in_call.contains(&bid))
        });
    }
    for (&(sid, bid), entry) in wrt.iter_mut() {
        if sid == solar_system_id && !attacked_npc_ids.contains(&bid) {
            entry.decay_cooldowns(1.0);
        }
    }
}

pub(crate) fn clear_extended_npc_weapon_runtime_with_rt(
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    solar_system_id: u32,
    ball_ids: &[i64],
) {
    if ball_ids.is_empty() {
        return;
    }
    let shared = {
        let guard = combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        guard.extended_npc_weapon_runtime.clone()
    };
    let mut wrt = shared.map.write().unwrap_or_else(|e| e.into_inner());
    for &bid in ball_ids {
        wrt.remove(&(solar_system_id, bid));
    }
}

pub(crate) fn clear_extended_npc_weapon_runtime(
    ctx: &ServerContext,
    solar_system_id: u32,
    ball_ids: &[i64],
) {
    clear_extended_npc_weapon_runtime_with_rt(ctx.combat_ephemeral.as_ref(), solar_system_id, ball_ids);
}

pub(crate) fn take_or_init_extended_npc_weapon_runtime(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc: &ExtendedNpcEntity,
) -> ExtendedNpcWeaponRuntime {
    let existing = with_mut_extended_npc_weapon_runtime(ctx, |wrt| {
        wrt.remove(&(solar_system_id, npc.ball_id))
    });
    let mut rt = match existing {
        Some(cached) if cached.npc_type_id == npc.type_id => cached,
        _ => {
            let profile =
                resolve_npc_combat_profile(&ctx.sde, npc.type_id, Some(npc.bounty_isk), npc.faction_id);
            let baseline_damage_vector = specialized_profile_shot_damage(&profile);
            let baseline_optimal_range = expected_specialized_optimal_range(
                &ctx.sde,
                npc.type_id,
                &npc.archetype,
                &profile,
            );
            let overridden = !damage_vector_approx_eq(npc.damage_vector, baseline_damage_vector)
                || (npc.optimal_range - baseline_optimal_range).abs() > 1e-3;
            ExtendedNpcWeaponRuntime {
                npc_type_id: npc.type_id,
                uses_synthetic_legacy: profile.is_synthetic_fallback || overridden,
                baseline_damage_vector,
                baseline_optimal_range,
                profile,
                turret_cooldown_s: 0.0,
                missile_cooldown_s: 0.0,
            }
        }
    };
    if !rt.uses_synthetic_legacy
        && (!damage_vector_approx_eq(npc.damage_vector, rt.baseline_damage_vector)
            || (npc.optimal_range - rt.baseline_optimal_range).abs() > 1e-3)
    {
        rt.uses_synthetic_legacy = true;
    }
    rt
}

pub(crate) fn store_extended_npc_weapon_runtime(
    ctx: &ServerContext,
    solar_system_id: u32,
    ball_id: i64,
    runtime: ExtendedNpcWeaponRuntime,
) {
    with_mut_extended_npc_weapon_runtime(ctx, |wrt| {
        wrt.insert((solar_system_id, ball_id), runtime);
    });
}

/// Pre-collected hostility lookup metadata for EDENCOM Vorton chaining so `Ballpark` write locks
/// never inspect `ctx.db` or `combat_ephemeral`.
pub(crate) struct EdencomHostilityMeta {
    ship_owners: HashMap<i64, (Option<i64>, HashMap<i64, f64>)>,
    drone_owners: HashMap<i64, (i64, Option<i64>, HashMap<i64, f64>)>,
    hostile_npc_ids: HashSet<i64>,
    location_invulnerable_balls: HashSet<i64>,
}

impl EdencomHostilityMeta {
    pub(crate) fn collect(ctx: &ServerContext, solar_system_id: u32, ball_ids: &[i64]) -> Self {
        let mut drone_owner_cids: HashMap<i64, i64> = HashMap::new();
        let mut hostile_npc_ids: HashSet<i64> = HashSet::new();
        with_combat_ephemeral(ctx, |rt| {
            for &id in ball_ids {
                if let Some(d) = rt.active_drones_in_space.get(&id) {
                    drone_owner_cids.insert(id, d.owner_char_id);
                }
                if let Some(other_npc) = rt.npc_entities.get(&id) {
                    let hostile_arch = matches!(
                        other_npc.archetype.as_str(),
                        "Triglavian"
                            | "Sleeper"
                            | "PochvenHypnosianSleeper"
                            | "Drifter"
                            | "RogueDrone"
                            | "BeltRat"
                            | "Officer"
                            | "Pirate"
                    );
                    let hostile_fac = matches!(
                        other_npc.faction_id,
                        500_026 | 500_024 | 500_021 | 500_025 | 500_010..=500_020
                    );
                    if hostile_arch || hostile_fac {
                        hostile_npc_ids.insert(id);
                    }
                }
            }
        });

        let ball_id_set: HashSet<i64> = ball_ids.iter().copied().collect();
        let (ship_owners, drone_owners, active_ship_chars) = ctx.db.with_snapshot(|snap| {
            let mut ships: HashMap<i64, (Option<i64>, HashMap<i64, f64>)> = HashMap::new();
            let mut ship_chars: Vec<(i64, i64)> = Vec::new();
            for ch in snap.characters.values() {
                if ball_id_set.contains(&ch.active_ship_id) {
                    ship_chars.push((ch.character_id, ch.active_ship_id));
                    ships
                        .entry(ch.active_ship_id)
                        .or_insert_with(|| (ch.fleet_id, ch.standings.clone()));
                }
            }
            let mut drones: HashMap<i64, (i64, Option<i64>, HashMap<i64, f64>)> = HashMap::new();
            for &id in ball_ids {
                let is_drone_or_fighter = drone_owner_cids.contains_key(&id)
                    || snap.items.get(&id).is_some_and(|it| {
                        ctx.sde.types.get(&it.type_id).is_some_and(|t| {
                            matches!(t.category_id, 18 | 87)
                                && !matches!(t.group_id, 12 | 186 | 340 | 448 | 649)
                        })
                    });
                if !is_drone_or_fighter {
                    continue;
                }
                let ocid_opt = drone_owner_cids.get(&id).copied().or_else(|| {
                    snap.items
                        .get(&id)
                        .and_then(|it| snap.characters.get(&it.owner_id))
                        .map(|c| c.character_id)
                });
                if let Some(ocid) = ocid_opt {
                    if let Some(ch) = snap.characters.get(&ocid) {
                        drones.insert(id, (ch.active_ship_id, ch.fleet_id, ch.standings.clone()));
                    }
                }
            }
            (ships, drones, ship_chars)
        });

        let now_unix = ctx.now_unix();
        let mut location_invulnerable_balls: HashSet<i64> = HashSet::new();
        for (cid, sid) in active_ship_chars {
            if ctx
                .systems
                .is_char_location_invulnerable_in_system(cid, solar_system_id, now_unix)
            {
                location_invulnerable_balls.insert(sid);
            }
        }

        Self {
            ship_owners,
            drone_owners,
            hostile_npc_ids,
            location_invulnerable_balls,
        }
    }

    pub(crate) fn is_location_invulnerable(&self, ball_id: i64) -> bool {
        self.location_invulnerable_balls.contains(&ball_id)
    }

    pub(crate) fn is_candidate_hostile(
        &self,
        npc: &ExtendedNpcEntity,
        cand_id: i64,
        target_id: i64,
    ) -> bool {
        if let Some((cand_fleet_id, standings)) = self.ship_owners.get(&cand_id) {
            let standing = standings.get(&(npc.faction_id as i64)).copied().unwrap_or(0.0);
            if standing < 0.0 {
                return true;
            }
            let target_fleet_id = self.ship_owners.get(&target_id).and_then(|(fid, _)| *fid);
            if let (Some(cfid), Some(tfid)) = (cand_fleet_id, target_fleet_id) {
                if *cfid == tfid && *cfid != 0 {
                    return true;
                }
            }
            return false;
        }
        if let Some((owner_active_ship, _, standings)) = self.drone_owners.get(&cand_id) {
            if *owner_active_ship == target_id {
                return true;
            }
            let standing = standings.get(&(npc.faction_id as i64)).copied().unwrap_or(0.0);
            return standing < 0.0;
        }
        self.hostile_npc_ids.contains(&cand_id)
    }
}

fn apply_extended_damage_to_ball(
    bp: &mut Ballpark,
    npc_ball_id: i64,
    target_id: i64,
    dmg: DamageVector,
    destroyed_in_tick: &mut Vec<(i64, i64, Ball)>,
    homefront_damage_events: &mut Vec<(i64, i64, f64)>,
) {
    if dmg.total() <= 0.0 {
        return;
    }
    if let Some((applied, destroyed, dead_opt)) = bp.apply_damage_to_ball(target_id, dmg) {
        if applied > 0.0 {
            homefront_damage_events.push((npc_ball_id, target_id, applied));
        }
        if destroyed {
            if let Some(dead_ball) = dead_opt {
                destroyed_in_tick.push((npc_ball_id, target_id, dead_ball));
            }
        }
    }
}

fn collect_edencom_arc_hits(
    sde: &SdeRegistry,
    edencom_meta: Option<&EdencomHostilityMeta>,
    npc: &ExtendedNpcEntity,
    bp: &Ballpark,
    target_id: i64,
    target_pos: DVec3,
    destroyed_in_call: &HashSet<i64>,
) -> Vec<i64> {
    let candidates: Vec<(i64, DVec3)> = bp
        .balls
        .iter()
        .filter(|(&other_id, other_ball)| {
            other_id != npc.ball_id
                && other_id != target_id
                && !destroyed_in_call.contains(&other_id)
                && is_edencom_arc_combatant_type(sde, other_ball.type_id)
                && !matches!(other_ball.mode, MovementMode::Warp { .. })
                && bp.combat_states.get(&other_id).is_some_and(|cs| {
                    cs.structure_hp > 0.0 && !cs.is_cloaked && !cs.is_invulnerable()
                })
                && !edencom_meta.is_some_and(|meta| meta.is_location_invulnerable(other_id))
                && (npc.threat_table.contains_key(&other_id)
                    || edencom_meta
                        .is_some_and(|meta| meta.is_candidate_hostile(npc, other_id, target_id)))
        })
        .map(|(&other_id, other_ball)| (other_id, other_ball.position))
        .collect();
    crate::formulas::vorton_arc_targets_with_cap(target_id, target_pos, &candidates, 10_000.0, 5)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_extended_npc_attack_in_bp(
    ctx: &ServerContext,
    bp: &mut Ballpark,
    npc: &mut ExtendedNpcEntity,
    weapon_rt: &mut ExtendedNpcWeaponRuntime,
    target_id: i64,
    target_sig_radii: &HashMap<i64, f64>,
    is_capital_target: bool,
    sleeper_sentry_can_fire: bool,
    edencom_meta: Option<&EdencomHostilityMeta>,
    destroyed_in_call: &HashSet<i64>,
    destroyed_in_tick: &mut Vec<(i64, i64, Ball)>,
    homefront_damage_events: &mut Vec<(i64, i64, f64)>,
) -> bool {
    let nid = npc.ball_id;
    let is_non_1956_drifter = ctx
        .sde
        .types
        .get(&npc.type_id)
        .is_none_or(|t| t.group_id != 1956);
    if npc.archetype == "Drifter" && npc.drifter_overshield_hp > 0.0 {
        if let Some(ncs) = bp.combat_states.get(&nid) {
            let lost = (ncs.max_shield_hp - ncs.shield_hp).max(0.0)
                + (ncs.max_armor_hp - ncs.armor_hp).max(0.0)
                + (ncs.max_structure_hp - ncs.structure_hp).max(0.0);
            if lost >= npc.drifter_overshield_hp {
                npc.drifter_overshield_hp = 0.0;
            }
        }
    }

    if !bp.balls.contains_key(&nid) || !bp.balls.contains_key(&target_id) {
        weapon_rt.decay_cooldowns(1.0);
        return false;
    }

    let drifter_doomsday = npc.archetype == "Drifter"
        && is_non_1956_drifter
        && npc.drifter_overshield_hp <= 0.0
        && !npc.drifter_doomsday_fired;
    if drifter_doomsday {
        npc.drifter_doomsday_fired = true;
        weapon_rt.decay_cooldowns(1.0);
        let fx_guid = weapon_rt
            .profile
            .turret
            .as_ref()
            .map(|t| t.fx_guid.clone())
            .unwrap_or_else(|| NpcWeaponKind::Laser.fx_guid().to_string());
        emit_npc_special_fx(bp, nid, target_id, None, None, fx_guid, 5.0);
        apply_extended_damage_to_ball(
            bp,
            nid,
            target_id,
            DamageVector::new(2_500.0, 2_500.0, 2_500.0, 2_500.0),
            destroyed_in_tick,
            homefront_damage_events,
        );
        return true;
    }

    if npc.archetype == "SleeperSentry" && !sleeper_sentry_can_fire {
        weapon_rt.decay_cooldowns(1.0);
        return false;
    }

    if weapon_rt.uses_synthetic_legacy {
        weapon_rt.decay_cooldowns(1.0);
        let mut cycle_dmg = npc.damage_vector;
        if npc.archetype == "Triglavian" {
            let mult = 1.0 + (npc.entropic_cycles as f64 * 0.10).min(1.50);
            cycle_dmg = cycle_dmg.scaled(mult);
            npc.entropic_cycles = npc.entropic_cycles.saturating_add(1);
        } else if npc.archetype == "Sleeper" && is_capital_target {
            cycle_dmg = cycle_dmg.scaled(2.5);
        } else if npc.archetype.eq_ignore_ascii_case("Edencom") {
            if let Some(target_pos) = bp.balls.get(&target_id).map(|b| b.position) {
                let hits = collect_edencom_arc_hits(
                    &ctx.sde,
                    edencom_meta,
                    npc,
                    bp,
                    target_id,
                    target_pos,
                    destroyed_in_call,
                );
                for arc_id in hits.into_iter().skip(1) {
                    apply_extended_damage_to_ball(
                        bp,
                        nid,
                        arc_id,
                        npc.damage_vector.scaled(0.5),
                        destroyed_in_tick,
                        homefront_damage_events,
                    );
                }
            }
        }
        let fx_guid = match npc.archetype.as_str() {
            "Triglavian" => NpcWeaponKind::TriglavianBeam.fx_guid(),
            "EDENCOM" | "Edencom" => NpcWeaponKind::Vorton.fx_guid(),
            _ => NpcWeaponKind::Laser.fx_guid(),
        };
        emit_npc_special_fx(bp, nid, target_id, None, None, fx_guid.to_string(), 2.0);
        apply_extended_damage_to_ball(
            bp,
            nid,
            target_id,
            cycle_dmg,
            destroyed_in_tick,
            homefront_damage_events,
        );
        return true;
    }

    let Some(surface_dist) = bp
        .balls
        .get(&nid)
        .zip(bp.balls.get(&target_id))
        .map(|(a, b)| a.surface_distance(b))
    else {
        weapon_rt.decay_cooldowns(1.0);
        return false;
    };

    if npc.archetype == "SleeperSentry" {
        weapon_rt.turret_cooldown_s = 0.0;
        weapon_rt.missile_cooldown_s = 0.0;
    }

    let mut dmg_mult = if npc.archetype == "Triglavian" {
        1.0 + (npc.entropic_cycles as f64 * 0.10).min(1.50)
    } else {
        1.0
    };
    if npc.archetype == "Sleeper" && is_capital_target {
        dmg_mult *= 2.5;
    }

    let mut fired = false;

    if let Some(turret) = &weapon_rt.profile.turret {
        let max_r = turret.max_effective_range();
        if surface_dist <= max_r && turret.shot_damage.total() > 0.0 {
            let mut shots = 0_u32;
            while weapon_rt.turret_cooldown_s <= 1e-9 && shots < 16 {
                if !bp.balls.contains_key(&target_id) {
                    break;
                }
                shots += 1;
                weapon_rt.turret_cooldown_s += turret.cycle_duration_s;
                fired = true;
                emit_npc_special_fx(
                    bp,
                    nid,
                    target_id,
                    turret.gfx_type_id,
                    None,
                    turret.fx_guid.clone(),
                    turret.cycle_duration_s,
                );
                if turret.kind == NpcWeaponKind::Vorton {
                    let target_pos = bp.balls.get(&target_id).map(|b| b.position);
                    if let Some(primary_dmg) = evaluate_npc_missile_or_vorton_hit(
                        bp,
                        target_sig_radii,
                        nid,
                        target_id,
                        turret.shot_damage.scaled(dmg_mult),
                        turret.optimal_range,
                        turret.vorton_explosion_radius,
                        turret.vorton_explosion_velocity,
                        turret.vorton_drf,
                    ) {
                        apply_extended_damage_to_ball(
                            bp,
                            nid,
                            target_id,
                            primary_dmg,
                            destroyed_in_tick,
                            homefront_damage_events,
                        );
                    }
                    if npc.archetype.eq_ignore_ascii_case("Edencom") {
                        if let Some(tpos) = target_pos {
                            let hits = collect_edencom_arc_hits(
                                &ctx.sde,
                                edencom_meta,
                                npc,
                                bp,
                                target_id,
                                tpos,
                                destroyed_in_call,
                            );
                            for arc_id in hits.into_iter().skip(1) {
                                if let Some(arc_dmg) = evaluate_npc_missile_or_vorton_hit(
                                    bp,
                                    target_sig_radii,
                                    nid,
                                    arc_id,
                                    turret.shot_damage.scaled(dmg_mult * 0.5),
                                    f64::INFINITY,
                                    turret.vorton_explosion_radius,
                                    turret.vorton_explosion_velocity,
                                    turret.vorton_drf,
                                ) {
                                    apply_extended_damage_to_ball(
                                        bp,
                                        nid,
                                        arc_id,
                                        arc_dmg,
                                        destroyed_in_tick,
                                        homefront_damage_events,
                                    );
                                }
                            }
                        }
                    }
                } else if let Some((shot_dmg, _wreck)) = evaluate_npc_turret_shot(
                    ctx,
                    bp,
                    target_sig_radii,
                    nid,
                    target_id,
                    turret.shot_damage.scaled(dmg_mult),
                    turret.optimal_range,
                    turret.falloff_range,
                    turret.tracking_speed,
                    turret.signature_resolution,
                    max_r,
                ) {
                    apply_extended_damage_to_ball(
                        bp,
                        nid,
                        target_id,
                        shot_dmg,
                        destroyed_in_tick,
                        homefront_damage_events,
                    );
                }
            }
            if bp.balls.contains_key(&target_id) {
                weapon_rt.turret_cooldown_s -= 1.0;
            } else {
                weapon_rt.turret_cooldown_s = (weapon_rt.turret_cooldown_s - 1.0).max(0.0);
            }
        } else {
            weapon_rt.turret_cooldown_s = (weapon_rt.turret_cooldown_s - 1.0).max(0.0);
        }
    }

    if let Some(missile) = &weapon_rt.profile.missile {
        if bp.balls.contains_key(&target_id)
            && surface_dist <= missile.max_flight_range
            && missile.volley_damage.total() > 0.0
        {
            let mut volleys = 0_u32;
            while weapon_rt.missile_cooldown_s <= 1e-9 && volleys < 16 {
                if !bp.balls.contains_key(&target_id) {
                    break;
                }
                volleys += 1;
                weapon_rt.missile_cooldown_s += missile.cycle_duration_s;
                fired = true;
                emit_npc_special_fx(
                    bp,
                    nid,
                    target_id,
                    None,
                    Some(missile.missile_type_id),
                    missile.fx_guid.clone(),
                    missile.cycle_duration_s,
                );
                if let Some(m_dmg) = evaluate_npc_missile_or_vorton_hit(
                    bp,
                    target_sig_radii,
                    nid,
                    target_id,
                    missile.volley_damage.scaled(dmg_mult),
                    missile.max_flight_range,
                    missile.explosion_radius,
                    missile.explosion_velocity,
                    missile.damage_reduction_factor,
                ) {
                    apply_extended_damage_to_ball(
                        bp,
                        nid,
                        target_id,
                        m_dmg,
                        destroyed_in_tick,
                        homefront_damage_events,
                    );
                }
            }
            if bp.balls.contains_key(&target_id) {
                weapon_rt.missile_cooldown_s -= 1.0;
            } else {
                weapon_rt.missile_cooldown_s = (weapon_rt.missile_cooldown_s - 1.0).max(0.0);
            }
        } else {
            weapon_rt.missile_cooldown_s = (weapon_rt.missile_cooldown_s - 1.0).max(0.0);
        }
    }

    if fired && npc.archetype == "Triglavian" {
        npc.entropic_cycles = npc.entropic_cycles.saturating_add(1);
    }

    fired
}

pub(crate) fn resolve_npc_wreck_type_id(
    sde: &SdeRegistry,
    victim_type_id: Option<u32>,
    archetype: Option<&str>,
) -> u32 {
    if let Some(wid) = victim_type_id.and_then(|tid| sde.wrecks.get(&tid).copied()) {
        return wid;
    }
    if archetype == Some("Sleeper") {
        return 30_495;
    }
    if archetype == Some("Drifter") {
        return 37_531;
    }
    if let Some(wid) = victim_type_id.and_then(|tid| sde.resolve_wreck_type_id(tid, None)) {
        return wid;
    }
    match NpcSizeClass::from_sde(sde, victim_type_id) {
        NpcSizeClass::Frigate => 26_557,
        NpcSizeClass::Cruiser => {
            if sde.types.contains_key(&26_558) {
                26_558
            } else {
                26_557
            }
        }
        NpcSizeClass::Battleship => {
            if sde.types.contains_key(&26_559) {
                26_559
            } else {
                26_557
            }
        }
        NpcSizeClass::Capital => {
            if sde.types.contains_key(&26_475) {
                26_475
            } else if sde.types.contains_key(&26_559) {
                26_559
            } else {
                26_557
            }
        }
    }
}

fn first_present_type_id(sde: &SdeRegistry, candidates: &[u32]) -> Option<u32> {
    candidates
        .iter()
        .copied()
        .find(|tid| sde.types.contains_key(tid))
        .or_else(|| candidates.first().copied())
}

fn resolve_turret_ammo_type_id(
    sde: &SdeRegistry,
    kind: NpcWeaponKind,
    size: NpcSizeClass,
    dmg: DamageVector,
) -> Option<u32> {
    let list: &[u32] = match kind {
        NpcWeaponKind::Laser => match size {
            NpcSizeClass::Frigate => &[246, 242, 239],
            NpcSizeClass::Cruiser => &[254, 250, 247],
            NpcSizeClass::Battleship => &[262, 258, 255],
            NpcSizeClass::Capital => &[17_686, 17_690, 17_688, 262],
        },
        NpcWeaponKind::Hybrid => match size {
            NpcSizeClass::Frigate => &[222, 219, 215],
            NpcSizeClass::Cruiser => &[230, 227, 223],
            NpcSizeClass::Battleship => &[238, 235, 231],
            NpcSizeClass::Capital => &[17_648, 17_658, 17_652, 238],
        },
        NpcWeaponKind::Projectile => {
            if dmg.explosive > dmg.thermal && dmg.explosive >= dmg.kinetic {
                match size {
                    NpcSizeClass::Frigate => &[183, 184, 178],
                    NpcSizeClass::Cruiser => &[191, 192, 186],
                    NpcSizeClass::Battleship => &[199, 200, 194],
                    NpcSizeClass::Capital => &[17_670, 17_674, 17_664, 199],
                }
            } else if dmg.em > dmg.kinetic {
                match size {
                    NpcSizeClass::Frigate => &[185, 184, 178],
                    NpcSizeClass::Cruiser => &[193, 192, 186],
                    NpcSizeClass::Battleship => &[201, 200, 194],
                    NpcSizeClass::Capital => &[17_668, 17_674, 17_664, 201],
                }
            } else if dmg.thermal > dmg.kinetic {
                match size {
                    NpcSizeClass::Frigate => &[184, 183, 178],
                    NpcSizeClass::Cruiser => &[192, 191, 186],
                    NpcSizeClass::Battleship => &[200, 199, 194],
                    NpcSizeClass::Capital => &[17_674, 17_670, 17_664, 200],
                }
            } else {
                match size {
                    NpcSizeClass::Frigate => &[178, 184, 183],
                    NpcSizeClass::Cruiser => &[186, 192, 191],
                    NpcSizeClass::Battleship => &[194, 200, 199],
                    NpcSizeClass::Capital => &[17_664, 17_674, 17_670, 194],
                }
            }
        }
        NpcWeaponKind::TriglavianBeam => match size {
            NpcSizeClass::Frigate => &[47_885],
            NpcSizeClass::Cruiser => &[47_886],
            NpcSizeClass::Battleship => &[47_887],
            NpcSizeClass::Capital => &[52_916, 47_887],
        },
        NpcWeaponKind::Vorton => match size {
            NpcSizeClass::Frigate => &[54_769],
            NpcSizeClass::Cruiser => &[54_773],
            NpcSizeClass::Battleship | NpcSizeClass::Capital => &[54_777],
        },
        NpcWeaponKind::Missile => &[],
    };
    first_present_type_id(sde, list)
}

fn resolve_player_missile_type_id(
    sde: &SdeRegistry,
    missile: &NpcMissileProfile,
    size: NpcSizeClass,
) -> Option<u32> {
    if let Some(mt) = sde.types.get(&missile.missile_type_id) {
        if mt.published && mt.market_group_id.is_some() {
            return Some(missile.missile_type_id);
        }
    }
    let d = missile.volley_damage;
    // Pick matching damage flavor (EM, Thermal, Kinetic, Explosive) by size class.
    let idx = if d.em >= d.thermal && d.em >= d.kinetic && d.em >= d.explosive {
        0
    } else if d.thermal >= d.kinetic && d.thermal >= d.explosive {
        1
    } else if d.kinetic >= d.explosive {
        2
    } else {
        3
    };
    let choices: &[u32] = match size {
        NpcSizeClass::Frigate => match idx {
            0 => &[212, 210],
            1 => &[211, 210],
            2 => &[210, 212],
            _ => &[213, 210],
        },
        NpcSizeClass::Cruiser => match idx {
            0 => &[207, 209],
            1 => &[208, 209],
            2 => &[209, 207],
            _ => &[206, 209],
        },
        NpcSizeClass::Battleship => match idx {
            0 => &[202, 204],
            1 => &[204, 203],
            2 => &[203, 204],
            _ => &[205, 204],
        },
        NpcSizeClass::Capital => &[32_436, 203, 204],
    };
    first_present_type_id(sde, choices)
}

pub(crate) fn resolve_npc_weapon_ammo_loot(
    sde: &SdeRegistry,
    npc_type_id: Option<u32>,
    profile: &NpcCombatProfile,
    faction_id: Option<u32>,
) -> Option<(u32, i32)> {
    if !profile.is_synthetic_fallback && profile.turret.is_none() && profile.missile.is_none() {
        return None;
    }
    let size = NpcSizeClass::from_sde(sde, npc_type_id);
    if let Some(turret) = profile.turret.as_ref() {
        let tid = resolve_turret_ammo_type_id(sde, turret.kind, size, turret.shot_damage)?;
        return Some((tid, size.ammo_quantity(turret.kind)));
    }
    if let Some(missile) = profile.missile.as_ref() {
        let tid = resolve_player_missile_type_id(sde, missile, size)?;
        return Some((tid, size.ammo_quantity(NpcWeaponKind::Missile)));
    }
    let kind = match faction_id.unwrap_or(0) {
        500_010 | 500_001 => NpcWeaponKind::Hybrid,
        500_012 | 500_019 | 500_003 => NpcWeaponKind::Laser,
        500_020 | 500_004 => NpcWeaponKind::Hybrid,
        500_011 | 500_002 => NpcWeaponKind::Projectile,
        _ => NpcWeaponKind::Hybrid,
    };
    let tid = resolve_turret_ammo_type_id(sde, kind, size, profile.nominal_damage_vector)?;
    Some((tid, size.ammo_quantity(kind)))
}
