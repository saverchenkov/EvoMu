#![forbid(unsafe_code)]

use eve_dogma::{
    compile_modifier_spec_to_vm, compute_passive_recharge_rate,
    resolve_system_wormhole_phenomenon, wormhole_phenomenon_type_for_class_and_name,
    DogmaEffectContext, DogmaExpressionNode, DogmaItemState, DogmaModifierFunc, DogmaVmOpcode,
    ModifierOperator, ProjectedEwarEffect, ShipFittingEngine,
};
use eve_sde::{
    attr_ids, DbuffCollectionInfo, DbuffGroupModifier, DbuffSkillModifier, DogmaEffectType,
    EffectModifierSpec, InvType, SdeRegistry, SolarSystem,
};
use std::collections::HashMap;

fn inv_type(
    base: &InvType,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
    effect_ids: Vec<u16>,
) -> InvType {
    let mut t = InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        volume: 10.0,
        published: true,
        default_attributes: HashMap::new(),
        effect_ids: vec![],
        ..base.clone()
    };
    t.default_attributes = default_attributes;
    t.effect_ids = effect_ids;
    t
}

#[test]
fn test_projected_ewar_five_types_command_bursts_and_effective_module_state() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base = sde.types.get(&587).expect("Rifter").clone();

    // Register 5 hostile EWAR module types (Tracking Disruptor, Guidance Disruptor, Sensor Dampener, Target Painter, Web)
    sde.types.insert(
        70001,
        inv_type(
            &base,
            70001,
            291,
            7,
            "TD II",
            HashMap::from([
                (attr_ids::MAX_RANGE, 20_000.0),
                (attr_ids::FALLOFF, 10_000.0),
                (351, -25.0),
                (349, -20.0),
                (767, -30.0),
            ]),
            vec![6424],
        ),
    );
    sde.types.insert(
        70002,
        inv_type(
            &base,
            70002,
            1308,
            7,
            "GD II",
            HashMap::from([
                (attr_ids::MAX_RANGE, 20_000.0),
                (547, -20.0),
                (596, -20.0),
                (847, -25.0),
                (848, 30.0),
            ]),
            vec![6423],
        ),
    );
    sde.types.insert(
        70003,
        inv_type(
            &base,
            70003,
            208,
            7,
            "Sensor Damp II",
            HashMap::from([(attr_ids::MAX_RANGE, 30_000.0), (566, -30.0), (309, -30.0)]),
            vec![],
        ),
    );
    sde.types.insert(
        70004,
        inv_type(
            &base,
            70004,
            379,
            7,
            "Target Painter II",
            HashMap::from([(attr_ids::MAX_RANGE, 30_000.0), (554, 30.0)]),
            vec![],
        ),
    );
    sde.types.insert(
        70005,
        inv_type(
            &base,
            70005,
            65,
            7,
            "Web II",
            HashMap::from([(attr_ids::MAX_RANGE, 10_000.0), (20, -60.0)]),
            vec![],
        ),
    );

    // Register turret (group 53), charge with otherID modifier, and command burst + dbuff collection
    sde.effects.insert(
        7901,
        DogmaEffectType {
            effect_id: 7901,
            name: "chargeDmgBoost".into(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "otherID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_ids::DAMAGE_MULTIPLIER,
                modifying_attribute_id: 120,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types.insert(
        70010,
        inv_type(
            &base,
            70010,
            53,
            7,
            "Light Autocannon II",
            HashMap::from([
                (attr_ids::CPU, 10.0),
                (attr_ids::POWER, 5.0),
                (attr_ids::DAMAGE_MULTIPLIER, 2.0),
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
            ]),
            vec![12],
        ),
    );
    sde.types.insert(
        70011,
        inv_type(
            &base,
            70011,
            83,
            8,
            "Hail S",
            HashMap::from([(120, 50.0)]),
            vec![7901],
        ),
    );

    // Command burst with dbuff collection covering location, location_group, and location_required_skill modifiers
    sde.dbuff_collections.insert(
        777,
        DbuffCollectionInfo {
            dbuff_id: 777,
            name: "InfoBurst".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Maximum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: vec![],
            location_modifiers: vec![attr_ids::SIGNATURE_RADIUS],
            location_group_modifiers: vec![DbuffGroupModifier {
                group_id: 53,
                attribute_id: attr_ids::TRACKING_SPEED,
            }],
            location_required_skill_modifiers: vec![DbuffSkillModifier {
                skill_id: 3300,
                attribute_id: attr_ids::MAX_RANGE,
            }],
            target_attribute_ids: vec![],
        },
    );
    sde.types.insert(
        70020,
        inv_type(
            &base,
            70020,
            1770,
            7,
            "Information Command Burst",
            HashMap::from([
                (attr_ids::CPU, 20.0),
                (attr_ids::POWER, 10.0),
                (2436, 777.0),
                (2440, 15.0),
            ]),
            vec![12, 6732],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde)
        .with_character_skills(HashMap::from([(3300, 5)]));
    eng.fit_module(101, 70010, 27, &sde).expect("fit gun");
    eng.load_charge(101, 70011, 40, &sde).expect("load charge");
    eng.fit_module(102, 70020, 28, &sde).expect("fit burst");
    eng.activate_module(102, false, &sde).expect("activate burst");
    eng.add_drone(201, 2454, &sde);

    // Project all 5 EWAR types
    for (idx, tid) in [70001u32, 70002, 70003, 70004, 70005]
        .into_iter()
        .enumerate()
    {
        eng.upsert_projected_ewar(
            ProjectedEwarEffect {
                source_module_id: 9000 + idx as i64,
                source_module_type_id: tid,
                charge_type_id: None,
                distance: 5_000.0,
                overloaded: false,
                effective_module: DogmaItemState::from_sde(9000 + idx as i64, tid, &sde),
            },
            &sde,
        );
    }

    assert!(eng.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) < 350.0);
    assert!(eng.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde) > 35.0);

    // build_effective_module_state (overloaded + normal)
    let eff_gun = eng.build_effective_module_state(101, 70010, true, &sde);
    assert!(eff_gun.get_attribute(attr_ids::DAMAGE_MULTIPLIER, &sde) >= 2.0);
    let eff_norm = eng.build_effective_module_state(101, 70010, false, &sde);
    assert_eq!(eff_norm.item_id, 101);
}

#[test]
fn test_context_all_domains_cascade_offline_dynamic_attr_and_all_wh_classes() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base = sde.types.get(&587).expect("Rifter").clone();

    // DogmaEffectContext across all domains + falloff + EffectStopper
    let item = DogmaItemState::from_sde(1, 587, &sde);
    let mut ctx = DogmaEffectContext::new(
        item.clone(),
        item.clone(),
        item.clone(),
        item.clone(),
        item.clone(),
        item.clone(),
    );
    ctx.item.base_attributes.insert(attr_ids::SPEED_FACTOR, 20.0);
    for ent in [
        &mut ctx.ship,
        &mut ctx.target,
        &mut ctx.other,
        &mut ctx.character,
        &mut ctx.structure,
    ] {
        ent.base_attributes.insert(attr_ids::MAX_VELOCITY, 100.0);
    }
    ctx.ship_located_items.push(item.clone());
    ctx.target_located_items.push(item.clone());
    ctx.char_located_items.push(item.clone());
    ctx.other_located_items.push(item.clone());
    ctx.structure_located_items.push(item.clone());
    ctx.owned_items.push(item.clone());

    for domain in ["shipID", "targetID", "otherID", "charID", "structureID"] {
        let spec = EffectModifierSpec {
            domain: domain.into(),
            func: "ItemModifier".into(),
            modified_attribute_id: attr_ids::MAX_VELOCITY,
            modifying_attribute_id: attr_ids::SPEED_FACTOR,
            operator: 6,
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        assert_eq!(ctx.apply_modifier_spec(10, &spec, &sde), 1);
        assert_eq!(
            ctx.apply_modifier_spec_with_falloff(
                11,
                Some(99),
                &spec,
                5_000.0,
                10_000.0,
                5_000.0,
                &sde
            ),
            1
        );
    }
    let stopper = EffectModifierSpec {
        domain: "targetID".into(),
        func: "EffectStopper".into(),
        modified_attribute_id: 0,
        modifying_attribute_id: 0,
        operator: 0,
        group_id: None,
        skill_type_id: None,
        effect_id: Some(6441),
    };
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(12, Some(39), &stopper, 5_000.0, 10_000.0, 0.0, &sde),
        1
    );
    assert!(compute_passive_recharge_rate(250.0, 1000.0, 200.0) > 0.0);

    // Cascade offline when taking CPU expander offline + dynamic attribute
    sde.types.insert(
        70030,
        inv_type(
            &base,
            70030,
            60,
            7,
            "Co-Processor II",
            HashMap::from([(attr_ids::CPU_OUTPUT, 200.0)]),
            vec![11],
        ),
    );
    sde.types.insert(
        70031,
        inv_type(
            &base,
            70031,
            53,
            7,
            "Heavy Gun",
            HashMap::from([(attr_ids::CPU, 80.0), (attr_ids::POWER, 5.0)]),
            vec![12],
        ),
    );
    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.fit_module(50, 70030, 11, &sde).expect("fit cpu mod");
    eng.fit_module(51, 70031, 27, &sde).expect("fit heavy gun");
    eng.set_module_dynamic_attribute_and_recalculate(51, attr_ids::CPU, 75.0, &sde);
    assert!(eng.modules.contains_key(&51));
    assert!(eng.set_ship_stance(34319, &sde).is_err());
    eng.take_module_offline(50, &sde).expect("take cpu offline");

    // All wormhole phenomenon classes (1..=6, 12, 13, 14..=18) + resolve_system_wormhole_phenomenon
    for class_id in [1u8, 2, 3, 4, 5, 6, 12, 13, 14, 15, 16, 17, 18] {
        for name in [
            "Pulsar",
            "Black Hole",
            "Cataclysmic Variable",
            "Magnetar",
            "Red Giant",
            "Wolf-Rayet",
        ] {
            assert!(wormhole_phenomenon_type_for_class_and_name(class_id, name).is_some());
        }
    }
    let sys_drifter = SolarSystem {
        solar_system_id: 31000001,
        constellation_id: 21000001,
        region_id: 11000001,
        name: "J000001".into(),
        security_status: -0.99,
        radius: 1e12,
        galactic_pos_ly: [0.0; 3],
        planet_ids: vec![],
        stations: vec![],
        wormhole_class_id: Some(17),
    };
    let sys_thera = SolarSystem {
        solar_system_id: 31000002,
        wormhole_class_id: Some(12),
        ..sys_drifter.clone()
    };
    sde.solar_systems.insert(31000001, sys_drifter);
    sde.solar_systems.insert(31000002, sys_thera);
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000001, Some("Pulsar")),
        Some((17, 2, "Pulsar", 30865))
    );
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000001, Some("Magnetar")),
        Some((17, 2, "Magnetar", 30860))
    );
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000002, Some("Pulsar")),
        None
    );

    // Remaining DogmaVmOpcode variants & compile_modifier_spec_to_vm
    for code in 1..=75 {
        assert!(DogmaVmOpcode::from_u8(code).is_some());
    }
    assert!(DogmaVmOpcode::from_u8(0).is_none());
    assert!(DogmaVmOpcode::from_u8(76).is_none());

    let n = |op: DogmaVmOpcode, v: Option<f64>| DogmaExpressionNode {
        opcode: op,
        value_f64: v,
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    };
    assert_eq!(
        DogmaExpressionNode {
            opcode: DogmaVmOpcode::If,
            value_f64: None,
            attribute_id: None,
            operator: None,
            arg1: Some(Box::new(n(DogmaVmOpcode::DefBool, Some(1.0)))),
            arg2: Some(Box::new(n(DogmaVmOpcode::DefInt, Some(42.0)))),
        }
        .evaluate(&item, &sde),
        42.0
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("OwnerRequiredSkillModifier"),
        Some(DogmaModifierFunc::OwnerRequiredSkillModifier)
    );
    assert!(compile_modifier_spec_to_vm(
        &EffectModifierSpec {
            domain: "charID".into(),
            func: "OwnerRequiredSkillModifier".into(),
            modified_attribute_id: attr_ids::DAMAGE_MULTIPLIER,
            modifying_attribute_id: attr_ids::SPEED_FACTOR,
            operator: ModifierOperator::PostPercent as i32,
            group_id: None,
            skill_type_id: Some(3300),
            effect_id: None,
        },
        false
    )
    .is_some());
}
