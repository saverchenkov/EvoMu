//! Semantic unit tests split from `evaluation_unit_tests`.

use super::*;
use eve_sde::{
    DbuffCollectionInfo, DbuffGroupModifier, DbuffSkillModifier, DogmaEffectType,
    EffectModifierSpec, SdeRegistry,
};
use std::collections::HashMap;

#[test]
fn projected_ewar_dbuffs_and_effective_module_charge_states() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        587,
        make_type(
            587,
            25,
            6,
            &[
                (attr_ids::MAX_VELOCITY, 300.0),
                (attr_ids::SIGNATURE_RADIUS, 40.0),
                (attr_ids::SCAN_RESOLUTION, 400.0),
                (attr_ids::MAX_TARGET_RANGE, 40_000.0),
                (2112, 1.0),
                (2113, 1.0),
                (2114, 1.0),
                (2115, 1.0),
            ],
            &[],
        ),
    );
    // Launcher type 6001 & missile charge type 6002 (with otherID modifier)
    sde.effects.insert(
        8010,
        DogmaEffectType {
            effect_id: 8010,
            name: "chargeMod".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "otherID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: attr_ids::MAX_RANGE,
                modifying_attribute_id: 20,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types.insert(
        6001,
        make_type(
            6001,
            509,
            7,
            &[
                (attr_ids::MAX_RANGE, 10_000.0),
                (attr_ids::TRACKING_SPEED, 1.0),
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
            ],
            &[],
        ),
    );
    sde.types.insert(
        6002,
        make_type(6002, 384, 8, &[(37, 2500.0), (654, 100.0), (20, 50.0)], &[8010]),
    );

    // Projected EWAR types: TD (6424), GD (6423), Damp (group 208), TP (group 379), Web (group 65)
    sde.types.insert(7001, make_type(7001, 209, 7, &[], &[6424]));
    sde.types.insert(7002, make_type(7002, 210, 7, &[], &[6423]));
    sde.types.insert(7003, make_type(7003, 208, 7, &[], &[]));
    sde.types.insert(7004, make_type(7004, 379, 7, &[], &[]));
    sde.types.insert(7005, make_type(7005, 65, 7, &[], &[]));

    // Dbuff collection 100 modifying item, location, location_group, location_required_skill
    sde.dbuff_collections.insert(
        100,
        DbuffCollectionInfo {
            dbuff_id: 100,
            name: "testDbuff".to_string(),
            operation: "PostPercent".to_string(),
            aggregate_mode: "Maximum".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: vec![attr_ids::MAX_VELOCITY],
            location_modifiers: vec![attr_ids::TRACKING_SPEED],
            location_group_modifiers: vec![DbuffGroupModifier {
                attribute_id: attr_ids::MAX_RANGE,
                group_id: 509,
            }],
            location_required_skill_modifiers: vec![DbuffSkillModifier {
                attribute_id: attr_ids::TRACKING_SPEED,
                skill_id: 3300,
            }],
            target_attribute_ids: Vec::new(),
        },
    );

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.modules.insert(
        30,
        FittedModule {
            item_id: 30,
            type_id: 6001,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(6002),
            charge_quantity: 40,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );

    for (id, tid, attrs) in [
        (1, 7001, vec![(351, -20.0), (349, -20.0), (767, -20.0), (54, 50_000.0)]),
        (2, 7002, vec![(547, -20.0), (596, -20.0), (847, -20.0), (848, 25.0), (54, 50_000.0)]),
        (3, 7003, vec![(566, -30.0), (309, -30.0), (54, 50_000.0)]),
        (4, 7004, vec![(554, 30.0), (54, 50_000.0)]),
        (5, 7005, vec![(20, -50.0), (54, 50_000.0)]),
    ] {
        let mut eff_mod = DogmaItemState::from_sde(id, tid, &sde);
        for (a, v) in attrs {
            eff_mod.base_attributes.insert(a, v);
        }
        let proj = ProjectedEwarEffect {
            source_module_id: id,
            source_module_type_id: tid,
            charge_type_id: None,
            distance: 10_000.0,
            overloaded: false,
            effective_module: eff_mod,
        };
        eng.upsert_projected_ewar(proj.clone(), &sde);
        eng.upsert_projected_ewar(proj, &sde); // upsert update path
    }

    eng.add_gang_burst_and_recalculate(
        GangBurstEffect {
            source_item_id: 900,
            source_type_id: None,
            dbuff_id: Some(100),
            value: 10.0,
            modifiers: Vec::new(),
        },
        &sde,
    );

    assert!((eng.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde) - 52.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &sde) - 280.0).abs() < 1e-9);
    assert!((eng.get_charge_effective_attribute(30, 654, &sde) - 125.0).abs() < 1e-9);

    let eff_state = eng.build_effective_module_state(30, 6001, true, &sde);
    assert!(eff_state.get_attribute(attr_ids::MAX_RANGE, &sde) > 10_000.0);

    // Unfitted hypothetical module state
    let hypo_state = eng.build_effective_module_state(999, 6001, false, &sde);
    assert_eq!(hypo_state.get_attribute(attr_ids::MAX_RANGE, &sde), 10_000.0);

    assert!(eng.remove_projected_ewar(1, &sde));
    assert!(!eng.remove_projected_ewar(1, &sde));
}

#[test]
fn skill_parameterized_effects_and_dbuff_operator_variants() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        587,
        make_type(
            587,
            25,
            6,
            &[
                (attr_ids::MAX_VELOCITY, 100.0),
                (attr_ids::SHIELD_CAPACITY, 100.0),
                (attr_ids::ARMOR_HP, 100.0),
                (attr_ids::HP_STRUCT, 100.0),
            ],
            &[],
        ),
    );
    // Skills with effects 660, 661, 662, 668, 848, 1851, and 1730 (with 1255)
    for eid in [660u16, 661, 662, 668, 848, 1851, 1730] {
        sde.effects.insert(
            eid,
            DogmaEffectType {
                effect_id: eid,
                name: format!("eff{eid}"),
                effect_category: 0,
                ..Default::default()
            },
        );
    }
    sde.types.insert(3001, make_type(3001, 255, 16, &[(292, 5.0)], &[660]));
    sde.types.insert(3002, make_type(3002, 255, 16, &[(1255, 5.0)], &[661]));
    sde.types.insert(3003, make_type(3003, 255, 16, &[(292, 5.0)], &[662]));
    sde.types.insert(3004, make_type(3004, 255, 16, &[(292, 5.0)], &[668]));
    sde.types.insert(3005, make_type(3005, 255, 16, &[(619, -10.0)], &[848]));
    sde.types.insert(3006, make_type(3006, 255, 16, &[(293, -5.0)], &[1851]));
    sde.types.insert(3007, make_type(3007, 255, 16, &[(1255, 10.0)], &[1730]));

    // Module requiring skills 3005 and 3006
    sde.types.insert(
        4000,
        make_type(
            4000,
            50,
            7,
            &[
                (560, 30.0),
                (attr_ids::SPEED, 1000.0),
                (attr_ids::REQUIRED_SKILL_1, 3005.0),
                (attr_ids::REQUIRED_SKILL_2, 3006.0),
            ],
            &[],
        ),
    );
    // Missile charge requiring 3001..3004
    sde.types.insert(
        4001,
        make_type(
            4001,
            384,
            8,
            &[
                (attr_ids::EM_DAMAGE, 100.0),
                (attr_ids::EXPLOSIVE_DAMAGE, 100.0),
                (attr_ids::THERMAL_DAMAGE, 100.0),
                (attr_ids::KINETIC_DAMAGE, 100.0),
                (attr_ids::REQUIRED_SKILL_1, 3001.0),
                (attr_ids::REQUIRED_SKILL_2, 3002.0),
                (attr_ids::REQUIRED_SKILL_3, 3003.0),
                (1285, 3004.0),
            ],
            &[],
        ),
    );

    // Dbuffs with PostMul, ModAdd, PostAssignment, and fallback operation
    for (id, op, aid) in [
        (201u32, "PostMul", attr_ids::MAX_VELOCITY),
        (202, "ModAdd", attr_ids::SHIELD_CAPACITY),
        (203, "PostAssignment", attr_ids::ARMOR_HP),
        (204, "FallbackOp", attr_ids::HP_STRUCT),
    ] {
        sde.dbuff_collections.insert(
            id,
            DbuffCollectionInfo {
                dbuff_id: id,
                name: format!("db{id}"),
                operation: op.to_string(),
                aggregate_mode: "Maximum".to_string(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: vec![aid],
                location_modifiers: Vec::new(),
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: Vec::new(),
            },
        );
    }

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde).with_character_skills(HashMap::from([
        (3001, 5),
        (3002, 5),
        (3003, 5),
        (3004, 5),
        (3005, 5),
        (3006, 5),
        (3007, 5),
    ]));
    eng.modules.insert(
        10,
        FittedModule {
            item_id: 10,
            type_id: 4000,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(4001),
            charge_quantity: 10,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    for (id, val) in [(201, 1.5), (202, 50.0), (203, 250.0), (204, 20.0)] {
        eng.add_gang_burst(GangBurstEffect {
            source_item_id: id as i64,
            source_type_id: None,
            dbuff_id: Some(id),
            value: val,
            modifiers: Vec::new(),
        });
    }
    eng.recalculate_stats(&sde);

    assert!((eng.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 150.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde) - 150.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::ARMOR_HP, &sde) - 250.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::HP_STRUCT, &sde) - 120.0).abs() < 1e-9);
    assert!((eng.get_module_effective_attribute(10, 560, &sde) - 27.0).abs() < 1e-9);
    assert!((eng.get_module_effective_attribute(10, attr_ids::SPEED, &sde) - 950.0).abs() < 1e-9);
    assert!((eng.get_charge_effective_attribute(10, attr_ids::EM_DAMAGE, &sde) - 105.0).abs() < 1e-9);
}
