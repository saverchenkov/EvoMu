//! V24 SDE-backed W-Space Sleeper Sentry Towers, C5/C6 Capital Escalations,
//! Drifter Response Boss loot (`Antikythera Element` + Sleeper Blue Loot), and
//! downtime despawn helpers (`feat:pve.sleeper-sentry-towers`,
//! `feat:pve.wormhole-capital-escalations`, `feat:pve.wormhole-drifter-response-escalation`,
//! `feat:pve.wormhole-site-capital-escalation-cap`, `feat:pve.wormhole-site-downtime-despawn`).

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::ExtendedNpcEntity;
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::HashMap;

/// Canonical V24 SDE Sleeper Sentry Tower type IDs (`Vigilant`, `Wakeful`, `Restless`).
pub const SLEEPER_SENTRY_VIGILANT: TypeId = 30_460;
pub const SLEEPER_SENTRY_WAKEFUL: TypeId = 30_461;
pub const SLEEPER_SENTRY_RESTLESS: TypeId = 30_462;

/// Canonical V24 SDE Drifter Response Battleship (`Arithmos Tyrannos` / `Hyleus Tyrannos`) loot table:
/// - `34_575` (`Antikythera Element`, primary Drifter component drop)
/// - `30_747` (`Sleeper Drone AI Nexus`, 5,000,000 ISK blue loot x 60 = 300M ISK)
pub const DRIFTER_BOSS_ELEMENT_TYPE_ID: TypeId = 34_575;
pub const DRIFTER_BOSS_BLUE_LOOT_TYPE_ID: TypeId = 30_747;
pub const DRIFTER_BOSS_BLUE_LOOT_QUANTITY: i64 = 60;

/// Resolves Sleeper Sentry Tower (`30460 Vigilant`, `30461 Wakeful`, `30462 Restless`) stats
/// directly from V24 `sde.types` (15s cycle, EM/Thermal damage up to 250 km, 50 m signature radius).
pub fn resolve_sleeper_sentry_stats(sde: &SdeRegistry, sentry_type_id: TypeId) -> (f64, f64, f64, f64) {
    let Some(t) = sde.types.get(&sentry_type_id) else {
        return (250_000.0, 15_000.0, 50.0, 180.0);
    };
    let a = &t.default_attributes;
    let opt = a
        .get(&eve_sde::attr_ids::MAX_RANGE)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(250_000.0)
        .max(250_000.0);
    let cycle_ms = a
        .get(&eve_sde::attr_ids::SPEED)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(15_000.0);
    let sig = a
        .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(50.0);
    let dmg_mult = a
        .get(&eve_sde::attr_ids::DAMAGE_MULTIPLIER)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(1.0);
    let em = a.get(&eve_sde::attr_ids::EM_DAMAGE).copied().unwrap_or(0.0) * dmg_mult;
    let th = a
        .get(&eve_sde::attr_ids::THERMAL_DAMAGE)
        .copied()
        .unwrap_or(0.0)
        * dmg_mult;
    let total_em_th = if em + th > 0.0 { em + th } else { 180.0 };
    (opt, cycle_ms, sig, total_em_th)
}

/// Builds a Sleeper Sentry Tower (`Ball`, `CombatState`, `ExtendedNpcEntity`) from V24 SDE attributes.
pub fn build_sde_sleeper_sentry_npc(
    sde: &SdeRegistry,
    solar_system_id: SolarSystemId,
    ball_id: i64,
    sentry_type_id: TypeId,
    pos: DVec3,
    wh_class: u8,
) -> (Ball, CombatState, ExtendedNpcEntity) {
    let (optimal_range, _cycle_ms, sig, em_th_total) =
        resolve_sleeper_sentry_stats(sde, sentry_type_id);
    let sde_type = sde.types.get(&sentry_type_id);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    let mut cs = build_type_combat_state_from_sde(sde, sentry_type_id);
    cs.signature_radius = sig;
    let dmg_mult = attr(eve_sde::attr_ids::DAMAGE_MULTIPLIER).unwrap_or(1.0);
    let raw_em = attr(eve_sde::attr_ids::EM_DAMAGE).unwrap_or(0.0) * dmg_mult;
    let raw_th = attr(eve_sde::attr_ids::THERMAL_DAMAGE).unwrap_or(0.0) * dmg_mult;
    let (em, thermal) = if raw_em + raw_th > 0.0 {
        (raw_em, raw_th)
    } else {
        (em_th_total * 0.5, em_th_total * 0.5)
    };
    let ball = Ball {
        id: ball_id,
        type_id: sentry_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: sde_type.map(|t| t.mass).unwrap_or(10_000_000.0),
        radius: sde_type.map(|t| t.radius).unwrap_or(450.0),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let entity = ExtendedNpcEntity {
        ball_id,
        type_id: sentry_type_id,
        faction_id: 500_005,
        archetype: "SleeperSentry".into(),
        solar_system_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range,
        orbit_range: 0.0,
        damage_vector: DamageVector {
            em,
            thermal,
            kinetic: 0.0,
            explosive: 0.0,
        },
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: pos,
        sleeper_wh_class: wh_class,
        custom_cargo_loot: None,
    };
    (ball, cs, entity)
}

/// Ensures V24 SDE fallback damage & EWAR attributes on `Arithmos Tyrannos` / `Hyleus Tyrannos`
/// (`37473` / `86498`) so Drifter bosses always wield real scram, web, neut, omni-DPS, and
/// `30,747` (`Sleeper Drone AI Nexus`, 300M ISK blue loot) + `34,575` (`Antikythera Element`) drops.
pub fn configure_drifter_response_boss_entity(sde: &SdeRegistry, entity: &mut ExtendedNpcEntity) {
    let tid = entity.type_id;
    let attr = |aid: u16| -> Option<f64> {
        sde.types
            .get(&tid)
            .and_then(|t| t.default_attributes.get(&aid).copied())
    };
    let dmg_mult = attr(64).unwrap_or(1.0).max(1.0);
    if entity.damage_vector.em + entity.damage_vector.thermal + entity.damage_vector.kinetic <= 0.0 {
        entity.damage_vector = DamageVector {
            em: attr(114).unwrap_or(120.0) * dmg_mult,
            thermal: attr(118).unwrap_or(120.0) * dmg_mult,
            kinetic: attr(117).unwrap_or(120.0) * dmg_mult,
            explosive: attr(116).unwrap_or(120.0) * dmg_mult,
        };
    }
    if entity.optimal_range <= 0.0 {
        entity.optimal_range = attr(54).unwrap_or(65_000.0);
    }
    if entity.orbit_range <= 0.0 {
        entity.orbit_range = attr(416).unwrap_or(28_000.0);
    }
    if entity.web_speed_factor >= 0.0 {
        entity.web_speed_factor = attr(20).filter(|&v| v < 0.0).unwrap_or(-60.0);
    }
    if entity.web_range <= 0.0 {
        entity.web_range = attr(514).or_else(|| attr(580)).unwrap_or(30_000.0);
    }
    if entity.scram_strength <= 0.0 {
        entity.scram_strength = attr(105).filter(|&v| v > 0.0).unwrap_or(3.0);
    }
    if entity.scram_range <= 0.0 {
        entity.scram_range = attr(103).or_else(|| attr(582)).unwrap_or(24_000.0);
    }
    if entity.neut_amount <= 0.0 {
        entity.neut_amount = attr(97).filter(|&v| v > 0.0).unwrap_or(600.0);
    }
    entity.tag_drop_type_id = Some(DRIFTER_BOSS_ELEMENT_TYPE_ID);
    entity.custom_cargo_loot = Some((
        DRIFTER_BOSS_BLUE_LOOT_TYPE_ID,
        DRIFTER_BOSS_BLUE_LOOT_QUANTITY,
    ));
}
