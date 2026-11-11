//! Edge-case unit tests for `wormhole_pve_escalations_ops`:
//! Sleeper Sentry Tower stats resolution (SDE + missing/zero attribute fallbacks),
//! `build_sde_sleeper_sentry_npc`, and `configure_drifter_response_boss_entity`
//! (both zeroed fallback defaults and preserving pre-populated attributes).

use super::*;
use eve_sde::{attr_ids, InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::OnceLock;

fn shared_sde() -> &'static SdeRegistry {
    static SDE: OnceLock<SdeRegistry> = OnceLock::new();
    SDE.get_or_init(SdeRegistry::with_canonical_seed)
}

fn empty_inv_type(type_id: TypeId, attrs: HashMap<u16, f64>) -> InvType {
    InvType {
        type_id,
        group_id: 999,
        category_id: 11,
        name: "Test Sentry".into(),
        mass: 5_000_000.0,
        volume: 100.0,
        capacity: 0.0,
        radius: 300.0,
        base_price: 0.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: vec![],
        default_attributes: attrs,
    }
}

fn blank_npc(type_id: TypeId) -> ExtendedNpcEntity {
    ExtendedNpcEntity {
        ball_id: 500_001,
        type_id,
        faction_id: 500_005,
        archetype: "Drifter".into(),
        solar_system_id: 31_000_005,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range: 0.0,
        orbit_range: 0.0,
        damage_vector: DamageVector::default(),
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
        spawn_position: DVec3::ZERO,
        sleeper_wh_class: 5,
        custom_cargo_loot: None,
    }
}

#[test]
fn resolve_sleeper_sentry_stats_canonical_and_fallback_cases() {
    let sde = shared_sde();

    for tid in [
        SLEEPER_SENTRY_VIGILANT,
        SLEEPER_SENTRY_WAKEFUL,
        SLEEPER_SENTRY_RESTLESS,
    ] {
        let (opt, cycle_ms, sig, total_dmg) = resolve_sleeper_sentry_stats(sde, tid);
        assert!(opt >= 250_000.0);
        assert!(cycle_ms > 0.0);
        assert!(sig > 0.0);
        assert!(total_dmg > 0.0);
    }

    // Missing type hits full default tuple
    assert_eq!(
        resolve_sleeper_sentry_stats(sde, 999_999_999),
        (250_000.0, 15_000.0, 50.0, 180.0)
    );

    // Synthetic SDE type with zeroed/empty attributes exercises fallback branches
    let mut synth = SdeRegistry::default();
    synth.types.insert(
        SLEEPER_SENTRY_VIGILANT,
        empty_inv_type(
            SLEEPER_SENTRY_VIGILANT,
            HashMap::from([
                (attr_ids::MAX_RANGE, 0.0),
                (attr_ids::SPEED, 0.0),
                (attr_ids::SIGNATURE_RADIUS, 0.0),
                (attr_ids::DAMAGE_MULTIPLIER, 0.0),
                (attr_ids::EM_DAMAGE, 0.0),
                (attr_ids::THERMAL_DAMAGE, 0.0),
            ]),
        ),
    );
    assert_eq!(
        resolve_sleeper_sentry_stats(&synth, SLEEPER_SENTRY_VIGILANT),
        (250_000.0, 15_000.0, 50.0, 180.0)
    );

    // Synthetic SDE type with positive EM + Thermal + multiplier
    synth.types.insert(
        SLEEPER_SENTRY_WAKEFUL,
        empty_inv_type(
            SLEEPER_SENTRY_WAKEFUL,
            HashMap::from([
                (attr_ids::MAX_RANGE, 300_000.0),
                (attr_ids::SPEED, 12_000.0),
                (attr_ids::SIGNATURE_RADIUS, 65.0),
                (attr_ids::DAMAGE_MULTIPLIER, 2.0),
                (attr_ids::EM_DAMAGE, 40.0),
                (attr_ids::THERMAL_DAMAGE, 50.0),
            ]),
        ),
    );
    assert_eq!(
        resolve_sleeper_sentry_stats(&synth, SLEEPER_SENTRY_WAKEFUL),
        (300_000.0, 12_000.0, 65.0, 180.0)
    );
}

#[test]
fn build_sde_sleeper_sentry_npc_canonical_and_fallback_damage() {
    let sde = shared_sde();
    let pos = DVec3::new(10_000.0, 20_000.0, -5_000.0);

    let (ball, cs, entity) =
        build_sde_sleeper_sentry_npc(sde, 31_000_005, 880_001, SLEEPER_SENTRY_VIGILANT, pos, 5);
    assert_eq!(ball.id, 880_001);
    assert_eq!(ball.type_id, SLEEPER_SENTRY_VIGILANT);
    assert_eq!(ball.position, pos);
    assert_eq!(ball.max_velocity, 0.0);
    assert!(cs.signature_radius > 0.0);
    assert_eq!(entity.faction_id, 500_005);
    assert_eq!(entity.archetype, "SleeperSentry");
    assert_eq!(entity.sleeper_wh_class, 5);
    assert!(entity.damage_vector.em + entity.damage_vector.thermal > 0.0);

    // Unknown sentry type hits 50/50 split fallback damage & default mass/radius
    let (fb_ball, fb_cs, fb_entity) =
        build_sde_sleeper_sentry_npc(sde, 31_000_006, 880_002, 999_999_999, pos, 6);
    assert_eq!(fb_ball.mass, 10_000_000.0);
    assert_eq!(fb_ball.radius, 450.0);
    assert_eq!(fb_cs.signature_radius, 50.0);
    assert_eq!(fb_entity.damage_vector.em, 90.0);
    assert_eq!(fb_entity.damage_vector.thermal, 90.0);
}

#[test]
fn configure_drifter_response_boss_entity_populates_fallbacks_and_preserves_existing() {
    let sde = shared_sde();

    // Real Drifter boss type IDs (`37473` Arithmos Tyrannos, `86498` Hyleus Tyrannos)
    for boss_tid in [37_473u32, 86_498u32, 999_999_999u32] {
        let mut ent = blank_npc(boss_tid);
        configure_drifter_response_boss_entity(sde, &mut ent);
        assert!(ent.damage_vector.em > 0.0);
        assert!(ent.damage_vector.thermal > 0.0);
        assert!(ent.damage_vector.kinetic > 0.0);
        assert!(ent.damage_vector.explosive > 0.0);
        assert!(ent.optimal_range > 0.0);
        assert!(ent.orbit_range > 0.0);
        assert!(ent.web_speed_factor < 0.0);
        assert!(ent.web_range > 0.0);
        assert!(ent.scram_strength > 0.0);
        assert!(ent.scram_range > 0.0);
        assert!(ent.neut_amount > 0.0);
        assert_eq!(ent.tag_drop_type_id, Some(DRIFTER_BOSS_ELEMENT_TYPE_ID));
        assert_eq!(
            ent.custom_cargo_loot,
            Some((DRIFTER_BOSS_BLUE_LOOT_TYPE_ID, DRIFTER_BOSS_BLUE_LOOT_QUANTITY))
        );
    }

    // Pre-populated values should NOT be overwritten, while loot tags are always set
    let mut custom = blank_npc(37_473);
    custom.damage_vector = DamageVector {
        em: 500.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    };
    custom.optimal_range = 99_000.0;
    custom.orbit_range = 33_000.0;
    custom.web_speed_factor = -75.0;
    custom.web_range = 42_000.0;
    custom.scram_strength = 6.0;
    custom.scram_range = 31_000.0;
    custom.neut_amount = 1_200.0;
    configure_drifter_response_boss_entity(sde, &mut custom);
    assert_eq!(custom.damage_vector.em, 500.0);
    assert_eq!(custom.optimal_range, 99_000.0);
    assert_eq!(custom.orbit_range, 33_000.0);
    assert_eq!(custom.web_speed_factor, -75.0);
    assert_eq!(custom.web_range, 42_000.0);
    assert_eq!(custom.scram_strength, 6.0);
    assert_eq!(custom.scram_range, 31_000.0);
    assert_eq!(custom.neut_amount, 1_200.0);
    assert_eq!(custom.tag_drop_type_id, Some(DRIFTER_BOSS_ELEMENT_TYPE_ID));
    assert_eq!(
        custom.custom_cargo_loot,
        Some((DRIFTER_BOSS_BLUE_LOOT_TYPE_ID, DRIFTER_BOSS_BLUE_LOOT_QUANTITY))
    );
}
