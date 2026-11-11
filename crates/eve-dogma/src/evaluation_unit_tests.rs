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

    // Negative / edge-case: mutating any of the 12 EvalInputsSnapshot fields directly without calling
    // recalculate_stats immediately invalidates persisted_context() and recomputes fresh non-stale values!
    let orig_ship_id = eng.ship.item_id;
    eng.ship.item_id = 999;
    assert!(eng.persisted_context().is_none());
    eng.ship.item_id = orig_ship_id;
    assert!(eng.persisted_context().is_some());

    let orig_ship_tid = eng.ship.type_id;
    eng.ship.type_id = 9999;
    assert!(eng.persisted_context().is_none());
    eng.ship.type_id = orig_ship_tid;
    assert!(eng.persisted_context().is_some());

    eng.active_stance_type_id = Some(34319);
    assert!(eng.persisted_context().is_none());
    eng.active_stance_type_id = None;
    assert!(eng.persisted_context().is_some());

    eng.modules.get_mut(&11).unwrap().damage_hp = 1.0;
    assert!(eng.persisted_context().is_none());
    eng.modules.get_mut(&11).unwrap().damage_hp = 0.0;
    assert!(eng.persisted_context().is_some());

    eng.implants.push(19540);
    assert!(eng.persisted_context().is_none());
    eng.implants.clear();
    assert!(eng.persisted_context().is_some());

    eng.boosters.push(28672);
    assert!(eng.persisted_context().is_none());
    eng.boosters.clear();
    assert!(eng.persisted_context().is_some());

    eng.booster_side_effects.insert(28672, vec![1]);
    assert!(eng.persisted_context().is_none());
    eng.booster_side_effects.clear();
    assert!(eng.persisted_context().is_some());

    eng.projected_ewar.push(ProjectedEwarEffect {
        source_module_id: 777,
        source_module_type_id: 439,
        charge_type_id: None,
        distance: 1000.0,
        overloaded: false,
        effective_module: DogmaItemState::from_sde(777, 439, &sde),
    });
    assert!(eng.persisted_context().is_none());
    eng.projected_ewar.clear();
    assert!(eng.persisted_context().is_some());

    eng.drones.push((902, 2454));
    assert!(eng.persisted_context().is_none());
    eng.drones.pop();
    assert!(eng.persisted_context().is_some());

    eng.ship.base_attributes.insert(9999, 1.0);
    assert!(eng.persisted_context().is_none());
    eng.ship.base_attributes.remove(&9999);
    assert!(eng.persisted_context().is_some());

    let saved_bursts = eng.gang_bursts.clone();
    eng.gang_bursts.clear();
    assert!(eng.persisted_context().is_none());
    eng.gang_bursts = saved_bursts;
    assert!(eng.persisted_context().is_some());

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

#[test]
fn eval_inputs_snapshot_invalidates_independently_on_all_12_fields() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        602,
        make_type(602, 25, 6, &[(attr_ids::MAX_VELOCITY, 250.0)], &[]),
    );
    sde.types.insert(
        603,
        make_type(603, 25, 6, &[(attr_ids::MAX_VELOCITY, 300.0)], &[]),
    );
    sde.types
        .insert(439, make_type(439, 46, 7, &[(attr_ids::CPU, 10.0)], &[]));
    sde.types
        .insert(3436, make_type(3436, 273, 16, &[(292, 10.0)], &[1730]));
    sde.types.insert(4001, make_type(4001, 300, 20, &[], &[]));
    sde.types.insert(4500, make_type(4500, 303, 20, &[], &[]));
    sde.types.insert(34321, make_type(34321, 1306, 6, &[], &[]));
    sde.types.insert(2454, make_type(2454, 100, 18, &[], &[]));

    let mut eng = ShipFittingEngine::from_sde(10, 602, &sde);
    eng.modules.insert(
        11,
        FittedModule {
            item_id: 11,
            type_id: 439,
            flag_id: 19,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.character_skills.insert(3436, 5);
    eng.implants = vec![4001];
    eng.boosters = vec![4500];
    eng.booster_side_effects.insert(4500, vec![8004]);
    eng.projected_ewar.push(ProjectedEwarEffect {
        source_module_id: 700,
        source_module_type_id: 439,
        effective_module: DogmaItemState::from_sde(700, 439, &sde),
        distance: 5_000.0,
        charge_type_id: None,
        overloaded: false,
    });
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 800,
        source_type_id: None,
        dbuff_id: Some(22),
        value: 15.0,
        modifiers: Vec::new(),
    });
    eng.active_stance_type_id = Some(34321);
    eng.drones.push((901, 2454));

    eng.recalculate_stats(&sde);
    assert!(eng.has_fresh_evaluation());
    assert!(eng.persisted_context().is_some());

    // Non-snapshot fields do not invalidate the evaluation cache
    let mut non_snap = eng.clone();
    non_snap.rack_heat[0] = 0.75;
    non_snap.last_stance_switch_ms = Some(42_000);
    assert!(non_snap.has_fresh_evaluation());
    assert!(non_snap.persisted_context().is_some());

    // Clearing evaluated_context_cache while snapshot remains Some invalidates has_fresh_evaluation
    let mut missing_cache = eng.clone();
    missing_cache.evaluated_context_cache = None;
    assert!(!missing_cache.has_fresh_evaluation());
    assert!(missing_cache.persisted_context().is_none());

    // Mutate each of the 12 snapshot fields one at a time and verify independent invalidation
    let mut c1 = eng.clone();
    c1.ship.item_id = 999;
    assert!(!c1.has_fresh_evaluation() && c1.persisted_context().is_none());

    let mut c2 = eng.clone();
    c2.ship.type_id = 603;
    assert!(!c2.has_fresh_evaluation() && c2.persisted_context().is_none());

    let mut c3 = eng.clone();
    c3.active_stance_type_id = None;
    assert!(!c3.has_fresh_evaluation() && c3.persisted_context().is_none());

    let mut c4 = eng.clone();
    c4.modules.get_mut(&11).unwrap().state = ModuleState::Active;
    assert!(!c4.has_fresh_evaluation() && c4.persisted_context().is_none());

    let mut c5 = eng.clone();
    c5.character_skills.insert(3436, 4);
    assert!(!c5.has_fresh_evaluation() && c5.persisted_context().is_none());

    let mut c6 = eng.clone();
    c6.implants.push(4002);
    assert!(!c6.has_fresh_evaluation() && c6.persisted_context().is_none());

    let mut c7 = eng.clone();
    c7.boosters.clear();
    assert!(!c7.has_fresh_evaluation() && c7.persisted_context().is_none());

    let mut c8 = eng.clone();
    c8.booster_side_effects.get_mut(&4500).unwrap().clear();
    assert!(!c8.has_fresh_evaluation() && c8.persisted_context().is_none());

    let mut c9 = eng.clone();
    c9.projected_ewar[0].distance += 1.0;
    assert!(!c9.has_fresh_evaluation() && c9.persisted_context().is_none());

    let mut c10 = eng.clone();
    c10.gang_bursts[0].value += 5.0;
    assert!(!c10.has_fresh_evaluation() && c10.persisted_context().is_none());

    let mut c11 = eng.clone();
    c11.drones[0].1 = 2456;
    assert!(!c11.has_fresh_evaluation() && c11.persisted_context().is_none());

    let mut c12 = eng.clone();
    c12.ship
        .base_attributes
        .insert(attr_ids::MAX_VELOCITY, 260.0);
    assert!(!c12.has_fresh_evaluation() && c12.persisted_context().is_none());
}

#[test]
fn fresh_and_stale_accessors_hit_expected_paths_and_isolate_owned_slices() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(602, make_type(602, 25, 6, &[], &[]));
    // Drone type 2454 with DAMAGE_MULTIPLIER = 1.5
    sde.types.insert(
        2454,
        make_type(2454, 100, 18, &[(attr_ids::DAMAGE_MULTIPLIER, 1.5)], &[]),
    );
    // Launcher module type 1877 with MAX_RANGE = 55.0 and DURATION = 4000.0
    sde.types.insert(
        1877,
        make_type(
            1877,
            509,
            7,
            &[(attr_ids::MAX_RANGE, 55.0), (attr_ids::DURATION, 4000.0)],
            &[],
        ),
    );
    // Charge type 27361 with explicit MAX_RANGE = 0.0 and KINETIC_DAMAGE = 80.0 (no DURATION)
    sde.types.insert(
        27361,
        make_type(
            27361,
            384,
            8,
            &[(attr_ids::MAX_RANGE, 0.0), (attr_ids::KINETIC_DAMAGE, 80.0)],
            &[],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 602, &sde);
    // Use overlapping ID 901 for both a drone and a charged launcher module so .take(drones_len)
    // vs .skip(drones_len) slice boundaries are strictly verified in stale mode!
    eng.drones.push((901, 2454));
    eng.drones.push((902, 2454));
    eng.modules.insert(
        901,
        FittedModule {
            item_id: 901,
            type_id: 1877,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(27361),
            charge_quantity: 10,
            dynamic_attribute_overrides: HashMap::from([(attr_ids::DURATION, 3500.0)]),
        },
    );
    // Offline module 903 with dynamic override (not in ship_located_items, hits fallback in get_module_effective_attribute)
    eng.modules.insert(
        903,
        FittedModule {
            item_id: 903,
            type_id: 1877,
            flag_id: 28,
            state: ModuleState::Offline,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::from([(attr_ids::DURATION, 2500.0)]),
        },
    );

    // 1. Stale mode (before recalculate_stats)
    assert!(!eng.has_fresh_evaluation());
    assert_eq!(eng.get_character_effective_attribute(192, &sde), 2.0);
    assert_eq!(eng.drone_state(901, &sde).unwrap().type_id, 2454);
    assert!(eng.drone_state(999_999, &sde).is_none());
    assert_eq!(
        eng.get_drone_effective_attribute(901, attr_ids::DAMAGE_MULTIPLIER, &sde),
        1.5
    );
    assert_eq!(
        eng.get_drone_effective_attribute(999_999, attr_ids::DAMAGE_MULTIPLIER, &sde),
        0.0
    );
    assert_eq!(eng.module_state(901, &sde).unwrap().type_id, 1877);
    assert!(eng.module_state(903, &sde).is_none());
    assert_eq!(
        eng.get_module_effective_attribute(901, attr_ids::DURATION, &sde),
        3500.0
    );
    assert_eq!(
        eng.get_module_effective_attribute(903, attr_ids::DURATION, &sde),
        2500.0
    );
    assert_eq!(
        eng.get_module_effective_attribute(999_999, attr_ids::DURATION, &sde),
        0.0
    );
    // Charge state at ID 901 must skip the drone at ID 901 and find the charge (type 27361)!
    assert_eq!(eng.charge_state(901, &sde).unwrap().type_id, 27361);
    assert!(eng.charge_state(999_999, &sde).is_none());
    // Charge attribute cases in stale mode:
    // - non-zero on charge (80.0)
    assert_eq!(
        eng.get_charge_effective_attribute(901, attr_ids::KINETIC_DAMAGE, &sde),
        80.0
    );
    // - explicit 0.0 in charge base_attributes (must return 0.0, NOT fall back to launcher's 55.0)
    assert_eq!(
        eng.get_charge_effective_attribute(901, attr_ids::MAX_RANGE, &sde),
        0.0
    );
    // - absent on charge -> falls back to launcher module's effective DURATION (3500.0)
    assert_eq!(
        eng.get_charge_effective_attribute(901, attr_ids::DURATION, &sde),
        3500.0
    );
    assert_eq!(eng.evaluated_context(&sde).owned_items.len(), 3);

    // 2. Fresh mode (after recalculate_stats)
    eng.recalculate_stats(&sde);
    assert!(eng.has_fresh_evaluation());
    assert_eq!(eng.get_character_effective_attribute(192, &sde), 2.0);
    assert_eq!(eng.drone_state(901, &sde).unwrap().type_id, 2454);
    assert!(eng.drone_state(999_999, &sde).is_none());
    assert_eq!(
        eng.get_drone_effective_attribute(901, attr_ids::DAMAGE_MULTIPLIER, &sde),
        1.5
    );
    assert_eq!(eng.module_state(901, &sde).unwrap().type_id, 1877);
    assert!(eng.module_state(903, &sde).is_none());
    assert_eq!(
        eng.get_module_effective_attribute(901, attr_ids::DURATION, &sde),
        3500.0
    );
    // Offline module 903 is not in evaluated_modules, so fresh path falls back to modules.get(&903)
    assert_eq!(
        eng.get_module_effective_attribute(903, attr_ids::DURATION, &sde),
        2500.0
    );
    assert_eq!(
        eng.get_module_effective_attribute(999_999, attr_ids::DURATION, &sde),
        0.0
    );
    assert_eq!(eng.charge_state(901, &sde).unwrap().type_id, 27361);
    assert!(eng.charge_state(999_999, &sde).is_none());
    assert_eq!(
        eng.get_charge_effective_attribute(901, attr_ids::KINETIC_DAMAGE, &sde),
        80.0
    );
    assert_eq!(
        eng.get_charge_effective_attribute(901, attr_ids::MAX_RANGE, &sde),
        0.0
    );
    assert_eq!(
        eng.get_charge_effective_attribute(901, attr_ids::DURATION, &sde),
        3500.0
    );
    assert_eq!(eng.evaluated_context(&sde).owned_items.len(), 3);

    // Exercise fresh-mode fallback when evaluated_drones entry is missing while cache is still fresh
    eng.evaluated_drones.remove(&902);
    assert!(eng.has_fresh_evaluation());
    assert_eq!(
        eng.get_drone_effective_attribute(902, attr_ids::DAMAGE_MULTIPLIER, &sde),
        1.5
    );
}

#[test]
fn offline_charged_modules_populate_owned_items_sorted_skip_unknown_and_receive_bcs() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(602, make_type(602, 25, 6, &[], &[]));
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
                operator: 4,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types
        .insert(22291, make_type(22291, 367, 7, &[(213, 1.10)], &[763]));
    sde.types.insert(1877, make_type(1877, 509, 7, &[], &[]));
    sde.types.insert(
        27361,
        make_type(27361, 384, 8, &[(attr_ids::KINETIC_DAMAGE, 100.0)], &[]),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 602, &sde);
    // Online BCS
    eng.modules.insert(
        10,
        FittedModule {
            item_id: 10,
            type_id: 22291,
            flag_id: 11,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    // Two offline charged launchers (flag 28 id 30, flag 27 id 25) + offline uncharged + offline unknown charge
    for (id, flag, charge) in [
        (30_i64, 28_u16, Some(27361_u32)),
        (25, 27, Some(27361)),
        (40, 29, None),
        (50, 30, Some(999_999)),
    ] {
        eng.modules.insert(
            id,
            FittedModule {
                item_id: id,
                type_id: 1877,
                flag_id: flag,
                state: ModuleState::Offline,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: charge,
                charge_quantity: if charge.is_some() { 5 } else { 0 },
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }

    eng.recalculate_stats(&sde);
    assert!(!eng.evaluated_modules.contains_key(&25));
    assert!(!eng.evaluated_modules.contains_key(&30));
    assert!(eng.evaluated_charges.contains_key(&25));
    assert!(eng.evaluated_charges.contains_key(&30));
    assert!(!eng.evaluated_charges.contains_key(&40));
    assert!(!eng.evaluated_charges.contains_key(&50));

    let ctx = eng.persisted_context().unwrap();
    let owned_ids: Vec<i64> = ctx.owned_items.iter().map(|i| i.item_id).collect();
    assert_eq!(owned_ids, vec![25, 30]);

    // Offline missile charges still receive character's missileDamageMultiplier (100 * 1.10 = 110.0)
    assert!(
        (eng.get_charge_effective_attribute(25, attr_ids::KINETIC_DAMAGE, &sde) - 110.0).abs()
            < 1e-9
    );
    assert!(
        (eng.get_charge_effective_attribute(30, attr_ids::KINETIC_DAMAGE, &sde) - 110.0).abs()
            < 1e-9
    );
}

#[test]
fn launcher_structural_attributes_excluded_from_charge_propagation_and_deduped() {
    let mut sde = SdeRegistry::default();
    // Hull effect modifying launcher structural attrs (9, 30, 50, 1153, 1211) AND missile attr (281)
    let mut hull_mods = Vec::new();
    for target_attr in [9_u16, 30, 50, 1153, 1211, 281] {
        hull_mods.push(EffectModifierSpec {
            domain: "shipID".to_string(),
            func: "LocationGroupModifier".to_string(),
            modified_attribute_id: target_attr,
            modifying_attribute_id: 20,
            operator: 6, // PreMul / PostPercent
            group_id: Some(509),
            skill_type_id: None,
            effect_id: None,
        });
    }
    sde.effects.insert(
        9001,
        DogmaEffectType {
            effect_id: 9001,
            name: "hullLauncherMods".to_string(),
            effect_category: 0,
            modifiers: hull_mods,
            ..Default::default()
        },
    );
    sde.types
        .insert(602, make_type(602, 25, 6, &[(20, 25.0)], &[9001]));
    sde.types.insert(
        1877,
        make_type(
            1877,
            509,
            7,
            &[(9, 40.0), (30, 10.0), (50, 20.0), (1153, 50.0), (1211, 1.0)],
            &[],
        ),
    );
    sde.types.insert(
        27361,
        make_type(
            27361,
            384,
            8,
            &[(9, 1.0), (30, 1.0), (50, 1.0), (1153, 1.0), (1211, 1.0), (281, 5000.0)],
            &[],
        ),
    );
    // Dbuff 22 applying identical modifier to both launcher and charge via LocationRequiredSkillModifier
    // to exercise !ch.modifiers.contains(m) deduplication
    sde.dbuff_collections.insert(
        22,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 22,
            name: "dupCheck".to_string(),
            operation: "PostPercent".to_string(),
            aggregate_mode: "Maximum".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![eve_sde::DbuffSkillModifier {
                attribute_id: 281,
                skill_id: 3319,
            }],
            target_attribute_ids: Vec::new(),
        },
    );
    sde.types
        .get_mut(&1877)
        .unwrap()
        .default_attributes
        .insert(attr_ids::REQUIRED_SKILL_1, 3319.0);
    sde.types
        .get_mut(&27361)
        .unwrap()
        .default_attributes
        .insert(attr_ids::REQUIRED_SKILL_1, 3319.0);

    let mut eng = ShipFittingEngine::from_sde(1, 602, &sde);
    eng.modules.insert(
        20,
        FittedModule {
            item_id: 20,
            type_id: 1877,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(27361),
            charge_quantity: 10,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.add_gang_burst(GangBurstEffect {
        source_item_id: 5000,
        source_type_id: None,
        dbuff_id: Some(22),
        value: 10.0,
        modifiers: Vec::new(),
    });
    eng.recalculate_stats(&sde);

    let ch = eng.charge_state(20, &sde).unwrap();
    for excluded_attr in [9_u16, 30, 50, 1153, 1211] {
        assert!(
            !ch.modifiers
                .iter()
                .any(|m| m.target_attribute_id == excluded_attr),
            "structural launcher attribute {excluded_attr} must NOT propagate onto charge"
        );
        assert_eq!(ch.get_attribute(excluded_attr, &sde), 1.0);
    }
    // Non-structural attr 281 gets the hull modifier (+25%) AND the dbuff modifier (+10%, deduped to 1 copy)
    let dbuff_count = ch
        .modifiers
        .iter()
        .filter(|m| m.source_item_id == 5000 && m.target_attribute_id == 281)
        .count();
    assert_eq!(dbuff_count, 1, "shared dbuff modifier must not be duplicated");
    assert!((ch.get_attribute(281, &sde) - 5000.0 * 1.25 * 1.10).abs() < 1e-6);
}

#[test]
fn missile_damage_multiplier_or_legs_are_independently_necessary_and_modifier_only_damage_scales() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(602, make_type(602, 25, 6, &[], &[]));
    // BCS giving 1.25x missileDamageMultiplier (attr 212)
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
                operator: 4,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types
        .insert(22291, make_type(22291, 367, 7, &[(213, 1.25)], &[763]));

    // Effect on launcher adding +50 THERMAL_DAMAGE to launcher (propagated to charge via Step 7)
    // while charge has base EM_DAMAGE = 100.0 and base THERMAL_DAMAGE = 0.0 (modifier-only damage!)
    sde.effects.insert(
        9002,
        DogmaEffectType {
            effect_id: 9002,
            name: "addThermal".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "itemID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: attr_ids::THERMAL_DAMAGE,
                modifying_attribute_id: 20,
                operator: 2, // ModAdd
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );

    // 6 distinct (launcher_type_id, charge_type_id) pairs isolating each `is_missile_charge` OR leg:
    // 1. charge effect_id == 9 only
    sde.types
        .insert(7001, make_type(7001, 1, 7, &[(20, 50.0)], &[9002]));
    sde.types.insert(
        8001,
        make_type(8001, 1, 8, &[(attr_ids::EM_DAMAGE, 100.0)], &[9]),
    );
    // 2. charge effect_id == 104 only
    sde.types.insert(7002, make_type(7002, 1, 7, &[], &[]));
    sde.types.insert(
        8002,
        make_type(8002, 1, 8, &[(attr_ids::EM_DAMAGE, 100.0)], &[104]),
    );
    // 3. charge group_id == 89 (torpedo) only
    sde.types.insert(7003, make_type(7003, 1, 7, &[], &[]));
    sde.types.insert(
        8003,
        make_type(8003, 89, 8, &[(attr_ids::EM_DAMAGE, 100.0)], &[]),
    );
    // 4. launcher effect_id == 101 only
    sde.types.insert(7004, make_type(7004, 1, 7, &[], &[101]));
    sde.types.insert(
        8004,
        make_type(8004, 1, 8, &[(attr_ids::EM_DAMAGE, 100.0)], &[]),
    );
    // 5. launcher group_id == 506 only
    sde.types.insert(7005, make_type(7005, 506, 7, &[], &[]));
    sde.types.insert(
        8005,
        make_type(8005, 1, 8, &[(attr_ids::EM_DAMAGE, 100.0)], &[]),
    );
    // 6. charge requires skill 3319 (Missile Launcher Operation) only
    sde.types.insert(7006, make_type(7006, 1, 7, &[], &[]));
    sde.types.insert(
        8006,
        make_type(
            8006,
            1,
            8,
            &[(attr_ids::EM_DAMAGE, 100.0), (attr_ids::REQUIRED_SKILL_1, 3319.0)],
            &[],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 602, &sde);
    eng.modules.insert(
        10,
        FittedModule {
            item_id: 10,
            type_id: 22291,
            flag_id: 11,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    for (idx, (ltid, ctid)) in [
        (7001_u32, 8001_u32),
        (7002, 8002),
        (7003, 8003),
        (7004, 8004),
        (7005, 8005),
        (7006, 8006),
    ]
    .into_iter()
    .enumerate()
    {
        let item_id = 100 + (idx as i64);
        eng.modules.insert(
            item_id,
            FittedModule {
                item_id,
                type_id: ltid,
                flag_id: 27 + (idx as u16),
                state: ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: Some(ctid),
                charge_quantity: 10,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }

    eng.recalculate_stats(&sde);
    for item_id in 100..106 {
        assert!(
            (eng.get_charge_effective_attribute(item_id, attr_ids::EM_DAMAGE, &sde) - 125.0).abs()
                < 1e-9,
            "missile charge leg on module {item_id} must receive 1.25x BCS multiplier"
        );
    }
    // Verify modifier-only THERMAL_DAMAGE (+50 ModAdd on launcher 100) also receives 1.25x PostMul -> 62.5,
    // while zero/unmodified EXPLOSIVE_DAMAGE receives no PostMul modifier.
    let ch100 = eng.charge_state(100, &sde).unwrap();
    assert!((ch100.get_attribute(attr_ids::THERMAL_DAMAGE, &sde) - 62.5).abs() < 1e-9);
    assert!(
        !ch100
            .modifiers
            .iter()
            .any(|m| m.target_attribute_id == attr_ids::EXPLOSIVE_DAMAGE)
    );
}

