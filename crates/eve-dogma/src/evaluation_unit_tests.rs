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
