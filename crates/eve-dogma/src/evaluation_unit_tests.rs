//! Unit and mutation-killing tests for `evaluation`.

use super::*;
use eve_sde::{
    DogmaEffectType,
    EffectModifierSpec, InvType, SdeRegistry,
};
use std::collections::HashMap;

#[cfg(test)]
#[path = "evaluation_projected_and_dbuff_unit_tests.rs"]
mod evaluation_projected_and_dbuff_unit_tests;

pub(super) fn make_type(
    type_id: TypeId,
    group_id: u32,
    category_id: u32,
    attrs: &[(AttributeId, f64)],
    effects: &[u16],
) -> InvType {
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type{type_id}"),
        mass: 1000.0,
        volume: 10.0,
        capacity: 10.0,
        radius: 10.0,
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
        effect_ids: effects.to_vec(),
        default_attributes,
    }
}

#[test]
fn builders_drones_and_t3c_subsystem_slots() {
    let mut sde = SdeRegistry::default();
    // T3C hull (group 963)
    sde.types.insert(
        29984,
        make_type(29984, 963, 6, &[(attr_ids::MAX_VELOCITY, 150.0)], &[]),
    );
    // Subsystem on flag 125 adding slots
    sde.types.insert(
        30001,
        make_type(
            30001,
            954,
            32,
            &[
                (1374, 5.0),
                (1375, 4.0),
                (1376, 3.0),
                (1368, 2.0),
                (1369, 3.0),
            ],
            &[3772],
        ),
    );
    // Drone type 2454 requiring skill 3436 (Drones)
    sde.types.insert(
        2454,
        make_type(
            2454,
            100,
            18,
            &[
                (attr_ids::DAMAGE_MULTIPLIER, 1.0),
                (attr_ids::REQUIRED_SKILL_1, 3436.0),
            ],
            &[],
        ),
    );
    // Skill 3436 with effect 1730 (droneDmgBonus +20%/lvl in attr 292)
    sde.types
        .insert(3436, make_type(3436, 273, 16, &[(292, 20.0)], &[1730]));
    sde.effects.insert(
        1730,
        DogmaEffectType {
            effect_id: 1730,
            name: "droneDmgBonus".to_string(),
            effect_category: 0,
            modifiers: Vec::new(),
            ..Default::default()
        },
    );

    let mut eng = ShipFittingEngine::from_sde(1, 29984, &sde)
        .with_character_skills(HashMap::from([(3436, 5)]))
        .with_implants(vec![1])
        .with_boosters(vec![2]);
    eng.set_implants(Vec::new());
    eng.set_boosters(Vec::new());

    eng.modules.insert(
        10,
        FittedModule {
            item_id: 10,
            type_id: 30001,
            flag_id: 125,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.recalculate_stats(&sde);
    assert_eq!(eng.ship.get_attribute(attr_ids::HI_SLOTS, &sde), 5.0);
    assert_eq!(eng.ship.get_attribute(attr_ids::MED_SLOTS, &sde), 4.0);
    assert_eq!(eng.ship.get_attribute(attr_ids::LOW_SLOTS, &sde), 3.0);
    assert_eq!(eng.ship.get_attribute(attr_ids::TURRET_SLOTS_LEFT, &sde), 2.0);
    assert_eq!(eng.ship.get_attribute(attr_ids::LAUNCHER_SLOTS_LEFT, &sde), 3.0);

    // Add, update, query, and remove drone
    eng.add_drone(500, 2454, &sde);
    eng.add_drone(500, 2454, &sde); // update path
    assert_eq!(eng.drones.len(), 1);
    assert!(eng.drone_state(500, &sde).is_some());
    assert!((eng.get_drone_effective_attribute(500, attr_ids::DAMAGE_MULTIPLIER, &sde) - 1.2).abs() < 1e-9);
    assert_eq!(eng.get_drone_effective_attribute(999, attr_ids::DAMAGE_MULTIPLIER, &sde), 0.0);
    assert!(eng.remove_drone(500, &sde));
    assert!(!eng.remove_drone(500, &sde));
}

#[test]
fn skills_implants_boosters_stances_and_active_modules() {
    let mut sde = SdeRegistry::default();
    // T3D hull (group 1305)
    sde.types.insert(
        34317,
        make_type(
            34317,
            1305,
            6,
            &[
                (attr_ids::MAX_VELOCITY, 200.0),
                (attr_ids::SIGNATURE_RADIUS, 50.0),
                (attr_ids::MASS, 1_000_000.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (2045, 1.0),
            ],
            &[],
        ),
    );
    // Stance type 34321 increasing MAX_VELOCITY by +50%
    sde.effects.insert(
        8001,
        DogmaEffectType {
            effect_id: 8001,
            name: "stanceVel".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: attr_ids::MAX_VELOCITY,
                modifying_attribute_id: 20,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types
        .insert(34321, make_type(34321, 1306, 6, &[(20, 50.0)], &[8001]));

    // Two pirate implants (group 300) with set multiplier (op 0 LocationGroupModifier on charID) and ship bonus
    sde.effects.insert(
        8002,
        DogmaEffectType {
            effect_id: 8002,
            name: "impSet".to_string(),
            effect_category: 0,
            modifiers: vec![
                EffectModifierSpec {
                    domain: "charID".to_string(),
                    func: "LocationGroupModifier".to_string(),
                    modified_attribute_id: 20,
                    modifying_attribute_id: 800,
                    operator: 0,
                    group_id: Some(300),
                    skill_type_id: None,
                    effect_id: None,
                },
                EffectModifierSpec {
                    domain: "shipID".to_string(),
                    func: "ItemModifier".to_string(),
                    modified_attribute_id: attr_ids::MAX_VELOCITY,
                    modifying_attribute_id: 20,
                    operator: 6,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    sde.types.insert(
        4001,
        make_type(4001, 300, 20, &[(20, 10.0), (800, 2.0)], &[8002]),
    );
    sde.types.insert(
        4002,
        make_type(4002, 300, 20, &[(20, 0.0), (800, 1.0)], &[8002]),
    );

    // Combat booster (group 303) with a main bonus (effect 8003) and a side-effect penalty (effect 8004)
    sde.effects.insert(
        8003,
        DogmaEffectType {
            effect_id: 8003,
            name: "neuroControl".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "charID".to_string(),
                func: "LocationGroupModifier".to_string(),
                modified_attribute_id: 901,
                modifying_attribute_id: 20,
                operator: 6,
                group_id: Some(303),
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types
        .insert(25538, make_type(25538, 273, 16, &[(20, -25.0)], &[8003]));
    sde.effects.insert(
        8004,
        DogmaEffectType {
            effect_id: 8004,
            name: "boosterSideEffect".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: attr_ids::SIGNATURE_RADIUS,
                modifying_attribute_id: 901,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types
        .insert(4500, make_type(4500, 303, 20, &[(901, 20.0)], &[8004]));

    let mut eng = ShipFittingEngine::from_sde(1, 34317, &sde);
    eng.active_stance_type_id = Some(34321);
    eng.set_implants(vec![4001, 4002]);
    eng.set_boosters(vec![4500]);
    eng.set_character_skills(HashMap::from([(25538, 5)]));

    // Side-effect suppressed when booster_side_effects has empty vec
    eng.booster_side_effects.insert(4500, Vec::new());
    eng.recalculate_stats(&sde);
    assert_eq!(eng.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde), 50.0);
    // Velocity: 200 * 1.5 (stance) * 1.2 (implant 4001 doubled by set multiplier) = 360.0
    assert!((eng.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 360.0).abs() < 1e-9);

    // Side-effect active when listed in booster_side_effects (20% reduced by 25% -> +15% sig)
    eng.booster_side_effects.insert(4500, vec![8004]);
    eng.recalculate_stats(&sde);
    assert!((eng.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde) - 57.5).abs() < 1e-9);

    // Active MWD (effect 6730) + Bastion (effect 6658) + overload effect (cat 5)
    sde.effects.insert(
        6730,
        DogmaEffectType {
            effect_id: 6730,
            name: "mwd".to_string(),
            effect_category: 1,
            ..Default::default()
        },
    );
    sde.effects.insert(
        6658,
        DogmaEffectType {
            effect_id: 6658,
            name: "bastion".to_string(),
            effect_category: 1,
            ..Default::default()
        },
    );
    sde.effects.insert(
        3175,
        DogmaEffectType {
            effect_id: 3175,
            name: "ovlSpeed".to_string(),
            effect_category: 5,
            modifiers: vec![EffectModifierSpec {
                domain: "itemID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: attr_ids::SPEED_FACTOR,
                modifying_attribute_id: 1223,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types.insert(
        5001,
        make_type(
            5001,
            46,
            7,
            &[
                (attr_ids::CPU, 25.0),
                (attr_ids::POWER, 15.0),
                (attr_ids::UPGRADE_COST, 50.0),
                (attr_ids::SPEED_FACTOR, 100.0),
                (1223, 50.0),
                (attr_ids::MASS_ADDITION, 500_000.0),
                (554, 100.0),
            ],
            &[6730, 6658, 3175],
        ),
    );
    eng.modules.insert(
        20,
        FittedModule {
            item_id: 20,
            type_id: 5001,
            flag_id: 19,
            state: ModuleState::Overloaded,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.recalculate_stats(&sde);
    assert_eq!(eng.ship.get_attribute(attr_ids::CPU_LOAD, &sde), 25.0);
    assert_eq!(eng.ship.get_attribute(attr_ids::POWER_LOAD, &sde), 15.0);
    assert_eq!(eng.ship.get_attribute(attr_ids::UPGRADE_CAPACITY, &sde), 350.0);
    assert_eq!(eng.ship.get_attribute(2045, &sde), 0.0); // Bastion immunity
}

#[test]
fn persisted_dogma_context_and_invalidation_on_direct_mutation() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        602,
        make_type(
            602,
            25,
            6,
            &[
                (attr_ids::MAX_VELOCITY, 250.0),
                (attr_ids::MASS, 1_000_000.0),
                (attr_ids::CPU_OUTPUT, 200.0),
                (attr_ids::POWER_OUTPUT, 100.0),
            ],
            &[],
        ),
    );
    sde.effects.insert(
        6731,
        DogmaEffectType {
            effect_id: 6731,
            name: "afterburner".to_string(),
            effect_category: 1,
            ..Default::default()
        },
    );
    sde.types.insert(
        439,
        make_type(
            439,
            46,
            7,
            &[
                (attr_ids::SPEED_FACTOR, 100.0),
                (attr_ids::MASS_ADDITION, 500_000.0),
                (attr_ids::REQUIRED_SKILL_1, 3450.0),
            ],
            &[6731],
        ),
    );
    // Mining crystal charge (requires 3386) + Strip Miner (requires 3386)
    sde.types.insert(
        17482,
        make_type(
            17482,
            464,
            7,
            &[(attr_ids::DURATION, 60_000.0), (attr_ids::REQUIRED_SKILL_1, 3386.0)],
            &[],
        ),
    );
    sde.types.insert(
        18036,
        make_type(
            18036,
            482,
            8,
            &[(9, 1.0), (783, 0.10), (784, 0.025), (786, 1.0), (attr_ids::REQUIRED_SKILL_1, 3386.0)],
            &[],
        ),
    );
    // Drone requiring 3436
    sde.types.insert(
        2454,
        make_type(
            2454,
            100,
            18,
            &[(attr_ids::DAMAGE_MULTIPLIER, 1.0), (attr_ids::REQUIRED_SKILL_1, 3436.0)],
            &[],
        ),
    );
    sde.effects.insert(
        146,
        DogmaEffectType {
            effect_id: 146,
            name: "damageMultiplierSkillBonus".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "itemID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: 292,
                modifying_attribute_id: 280,
                operator: 0,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        1730,
        DogmaEffectType {
            effect_id: 1730,
            name: "droneDmgBonus".to_string(),
            effect_category: 0,
            ..Default::default()
        },
    );
    sde.types
        .insert(3436, make_type(3436, 273, 16, &[(292, 10.0)], &[146, 1730]));

    // Dbuff 22 (Skirmish Rapid Deployment: +20% AB speedFactor) & Dbuff 25 (-30% crystal volatility)
    sde.dbuff_collections.insert(
        22,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 22,
            name: "rapidDeployment".to_string(),
            operation: "PostPercent".to_string(),
            aggregate_mode: "Maximum".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![eve_sde::DbuffSkillModifier {
                attribute_id: attr_ids::SPEED_FACTOR,
                skill_id: 3450,
            }],
            target_attribute_ids: Vec::new(),
        },
    );
    sde.dbuff_collections.insert(
        25,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 25,
            name: "crystalPreservation".to_string(),
            operation: "PostPercent".to_string(),
            aggregate_mode: "Minimum".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![eve_sde::DbuffSkillModifier {
                attribute_id: 783,
                skill_id: 3386,
            }],
            target_attribute_ids: Vec::new(),
        },
    );

    let mut eng = ShipFittingEngine::from_sde(100, 602, &sde)
        .with_character_skills(HashMap::from([(3436, 5)]));
    // Before recalculate_stats, persisted_context is None
    assert!(eng.persisted_context().is_none());

    eng.modules.insert(
        11,
        FittedModule {
            item_id: 11,
            type_id: 439,
            flag_id: 19,
            state: ModuleState::Active,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.modules.insert(
        12,
        FittedModule {
            item_id: 12,
            type_id: 17482,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(18036),
            charge_quantity: 1,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.drones.push((901, 2454));
    eng.add_gang_burst(GangBurstEffect {
        source_item_id: 5000,
        source_type_id: None,
        dbuff_id: Some(22),
        value: 20.0,
        modifiers: Vec::new(),
    });
    eng.add_gang_burst(GangBurstEffect {
        source_item_id: 5001,
        source_type_id: None,
        dbuff_id: Some(25),
        value: -30.0,
        modifiers: Vec::new(),
    });
    eng.recalculate_stats(&sde);

    // Persisted context, modules, charges, and drones are populated
    assert!(eng.persisted_context().is_some());
    assert!(eng.evaluated_modules.contains_key(&11));
    assert!(eng.evaluated_modules.contains_key(&12));
    assert!(eng.evaluated_charges.contains_key(&12));
    assert!(eng.evaluated_drones.contains_key(&901));

    // Skirmish Rapid Deployment boosts AB SPEED_FACTOR 100 -> 120, which boosts ship MAX_VELOCITY 250 * (1 + 1.20) = 550.0!
    assert!((eng.get_module_effective_attribute(11, attr_ids::SPEED_FACTOR, &sde) - 120.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 550.0).abs() < 1e-9);
    // Mining Equipment Preservation reduces crystal volatility 0.10 -> 0.07 without double-counting!
    assert!((eng.get_charge_effective_attribute(12, 783, &sde) - 0.07).abs() < 1e-9);
    assert!((eng.get_drone_effective_attribute(901, attr_ids::DAMAGE_MULTIPLIER, &sde) - 1.5).abs() < 1e-9);

    // Negative / edge-case: mutating character_skills or gang_bursts directly without calling recalculate_stats
    // immediately invalidates persisted_context() and recomputes fresh non-stale values!
    eng.character_skills.insert(3436, 2);
    assert!(eng.persisted_context().is_none());
    assert!((eng.get_drone_effective_attribute(901, attr_ids::DAMAGE_MULTIPLIER, &sde) - 1.2).abs() < 1e-9);

    eng.gang_bursts.clear();
    assert!((eng.get_charge_effective_attribute(12, 783, &sde) - 0.10).abs() < 1e-9);
}

#[test]
fn missile_damage_multiplier_propagates_to_missile_charges_not_turret_ammo() {
    let mut sde = SdeRegistry::default();
    // Attribute 212 (missileDamageMultiplier) is non-stackable so two BCS get EVE stacking penalty!
    sde.attributes.insert(
        212,
        eve_sde::DogmaAttributeType {
            attribute_id: 212,
            name: "missileDamageMultiplier".to_string(),
            default_value: 1.0,
            high_is_good: true,
            stackable: false,
            published: true,
        },
    );
    sde.types.insert(602, make_type(602, 25, 6, &[], &[]));
    // Ballistic Control System effect (PostMul on charID attr 212 from attr 213)
    sde.effects.insert(
        763,
        DogmaEffectType {
            effect_id: 763,
            name: "bcsBonus".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "charID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: 212,
                modifying_attribute_id: 213,
                operator: 4, // PostMul
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types
        .insert(22291, make_type(22291, 367, 7, &[(213, 1.10)], &[763]));
    // Missile Launcher (group 509) + Light Missile (group 384, effect 9)
    sde.types.insert(1877, make_type(1877, 509, 7, &[], &[]));
    sde.types.insert(
        27361,
        make_type(27361, 384, 8, &[(attr_ids::KINETIC_DAMAGE, 100.0)], &[]),
    );
    // Projectile Turret (group 55) + Projectile Ammo (group 83)
    sde.types.insert(2881, make_type(2881, 55, 7, &[], &[]));
    sde.types.insert(
        192,
        make_type(192, 83, 8, &[(attr_ids::KINETIC_DAMAGE, 50.0)], &[]),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 602, &sde);
    for (id, tid, flag, charge) in [
        (10i64, 22291u32, 11u16, None),
        (11, 22291, 12, None),
        (20, 1877, 27, Some(27361)),
        (21, 2881, 28, Some(192)),
    ] {
        eng.modules.insert(
            id,
            FittedModule {
                item_id: id,
                type_id: tid,
                flag_id: flag,
                state: ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: charge,
                charge_quantity: if charge.is_some() { 10 } else { 0 },
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }
    eng.recalculate_stats(&sde);

    // Expected stacking-penalized 2x +10% BCS multiplier: 1.10 * (1.0 + 0.10 * exp(-(1/2.67)^2))
    let second_pen = (-(1.0_f64 / 2.67).powi(2)).exp();
    let exp_mult = 1.10 * (1.0 + 0.10 * second_pen);
    assert!((eng.get_character_effective_attribute(212, &sde) - exp_mult).abs() < 1e-9);
    assert!(
        (eng.get_charge_effective_attribute(20, attr_ids::KINETIC_DAMAGE, &sde) - 100.0 * exp_mult)
            .abs()
            < 1e-7,
        "Missile charge kinetic damage must include stacking-penalized BCS multiplier"
    );
    // Negative check: Projectile turret ammo must NOT be modified by Ballistic Control Systems!
    assert!(
        (eng.get_charge_effective_attribute(21, attr_ids::KINETIC_DAMAGE, &sde) - 50.0).abs()
            < 1e-9,
        "Turret ammo must not receive missileDamageMultiplier"
    );
}
