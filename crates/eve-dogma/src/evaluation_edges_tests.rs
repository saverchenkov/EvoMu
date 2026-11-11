use super::*;
use eve_sde::{
    attr_ids, CategoryId, DbuffCollectionInfo, DbuffGroupModifier, DbuffSkillModifier,
    DogmaAttributeType, DogmaEffectType, EffectModifierSpec, GroupId, InvType, SdeRegistry,
};
use std::collections::HashMap;

fn make_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    name: &str,
    mass: f64,
    attrs: &[(AttributeId, f64)],
    effects: &[EffectId],
) -> InvType {
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass,
        volume: 100.0,
        capacity: 100.0,
        radius: 50.0,
        base_price: 100.0,
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
        default_effect_id: effects.first().copied(),
        description: String::new(),
        effect_ids: effects.to_vec(),
        default_attributes,
    }
}

fn make_effect(
    effect_id: EffectId,
    name: &str,
    effect_category: u16,
    modifiers: Vec<EffectModifierSpec>,
) -> DogmaEffectType {
    DogmaEffectType {
        effect_id,
        name: name.to_string(),
        effect_category,
        published: true,
        modifiers,
        ..Default::default()
    }
}

fn mod_spec(
    domain: &str,
    func: &str,
    modified: AttributeId,
    modifying: AttributeId,
    operator: i32,
    group_id: Option<GroupId>,
    skill_type_id: Option<TypeId>,
) -> EffectModifierSpec {
    EffectModifierSpec {
        domain: domain.to_string(),
        func: func.to_string(),
        modified_attribute_id: modified,
        modifying_attribute_id: modifying,
        operator,
        group_id,
        skill_type_id,
        effect_id: None,
    }
}

fn build_eval_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    for &(id, stackable) in &[
        (attr_ids::CPU_OUTPUT, true),
        (attr_ids::POWER_OUTPUT, true),
        (attr_ids::CPU, true),
        (attr_ids::POWER, true),
        (attr_ids::MAX_VELOCITY, false),
        (attr_ids::SIGNATURE_RADIUS, false),
        (attr_ids::ARMOR_HP, true),
        (attr_ids::SHIELD_CAPACITY, true),
        (attr_ids::DAMAGE_MULTIPLIER, false),
        (attr_ids::MAX_RANGE, false),
        (2045u16, false),
        (2112u16, false),
        (2113u16, false),
        (2114u16, false),
        (2115u16, false),
    ] {
        sde.attributes.insert(
            id,
            DogmaAttributeType {
                attribute_id: id,
                name: format!("attr_{id}"),
                default_value: if (2112..=2115).contains(&id) || id == 2045 { 1.0 } else { 0.0 },
                high_is_good: true,
                stackable,
                published: true,
            },
        );
    }

    // Standard slot & category effects
    sde.effects.insert(11, make_effect(11, "loPower", 0, vec![]));
    sde.effects.insert(12, make_effect(12, "hiPower", 0, vec![]));
    sde.effects.insert(13, make_effect(13, "medPower", 0, vec![]));
    sde.effects.insert(16, make_effect(16, "online", 4, vec![]));
    sde.effects.insert(132, make_effect(132, "skillEffect", 0, vec![]));
    sde.effects.insert(3772, make_effect(3772, "subSystem", 0, vec![]));

    // T3C Hull (group 963) + subsystems (category 32)
    sde.types.insert(
        29984,
        make_type(
            29984,
            963,
            6,
            "Tengu",
            9_000_000.0,
            &[
                (attr_ids::CPU_OUTPUT, 500.0),
                (attr_ids::POWER_OUTPUT, 1000.0),
                (attr_ids::MAX_VELOCITY, 200.0),
                (attr_ids::SIGNATURE_RADIUS, 150.0),
                (attr_ids::ARMOR_HP, 2000.0),
                (attr_ids::SHIELD_CAPACITY, 3000.0),
        (attr_ids::HI_SLOTS, 4.0),
        (attr_ids::MED_SLOTS, 4.0),
        (attr_ids::LOW_SLOTS, 4.0),
        (1367u16, 4.0),
                (2112, 1.0),
                (2113, 1.0),
                (2114, 1.0),
                (2115, 1.0),
            ],
            &[],
        ),
    );
    // Subsystem adding +2 high slots and +500 armor hp
    sde.effects.insert(
        9001,
        make_effect(
            9001,
            "subBonus",
            0,
            vec![
                mod_spec("shipID", "ItemModifier", attr_ids::HI_SLOTS, 1374, 2, None, None),
                mod_spec("shipID", "ItemModifier", attr_ids::ARMOR_HP, 98, 2, None, None),
            ],
        ),
    );
    sde.types.insert(
        30100,
        make_type(
            30100,
            954,
            32,
            "Offensive Sub",
            0.0,
            &[(1374, 2.0), (98, 500.0), (1366, 29984.0)],
            &[3772, 9001],
        ),
    );

    // Parameterized skill 1730 (droneDmgBonus) without attr 292 (fallback) & with attr 1255, plus non-0 effect_category skill.
    sde.effects.insert(1730, make_effect(1730, "droneDmgBonus", 0, vec![]));
    sde.effects.insert(
        9002,
        make_effect(
            9002,
            "skillPassive",
            0,
            vec![
                mod_spec("itemID", "ItemModifier", attr_ids::CAPACITY, 292, 6, None, None),
                mod_spec("shipID", "ItemModifier", attr_ids::SHIELD_CAPACITY, 292, 6, None, None),
            ],
        ),
    );
    sde.effects.insert(
        9003,
        make_effect(
            9003,
            "skillActiveIgnored",
            1,
            vec![mod_spec("shipID", "ItemModifier", attr_ids::ARMOR_HP, 292, 2, None, None)],
        ),
    );
    // Skill 3436 (Drone Interfacing) with attr 1255 (fallback check), and skill with no attr 292
    sde.types.insert(
        3436,
        make_type(3436, 273, 16, "Drone Interfacing", 0.0, &[(1255, 10.0), (280, 1.0)], &[132, 1730]),
    );
    sde.types.insert(
        3437,
        make_type(3437, 273, 16, "Dummy Skill No 292", 0.0, &[(292, 5.0), (attr_ids::CAPACITY, 100.0)], &[132, 9002, 9003]),
    );
    sde.types.insert(
        3438,
        make_type(3438, 273, 16, "Skill Only 132", 0.0, &[(280, 1.0)], &[132]),
    );
    sde.types.insert(
        3439,
        make_type(3439, 273, 16, "Fallback 292 Drone Skill", 0.0, &[(280, 1.0)], &[132, 1730]),
    );
    // Drone requiring skill 3436 (type 2488)
    sde.types.insert(
        2488,
        make_type(
            2488,
            100,
            18,
            "Warrior II",
            5.0,
            &[(attr_ids::DAMAGE_MULTIPLIER, 2.0), (182, 3436.0)],
            &[],
        ),
    );

    // Pirate implants: LocationGroupModifier PreMul (operator 0) on implant group 300,
    // suppressed when 1 implant active (multi_implant_set == false) and applied when >= 2 active!
    sde.effects.insert(
        9010,
        make_effect(
            9010,
            "implantSetMod",
            0,
            vec![
                mod_spec("charID", "LocationGroupModifier", 98, 292, 0, Some(300), None),
                mod_spec("charID", "LocationRequiredSkillModifier", 98, 292, 6, None, Some(3437)),
                mod_spec("itemID", "ItemModifier", 97, 98, 2, None, None),
            ],
        ),
    );
    sde.types.insert(
        19540,
        make_type(19540, 300, 20, "Set Implant Alpha", 0.0, &[(292, 1.15), (98, 100.0)], &[9010]),
    );
    sde.types.insert(
        19541,
        make_type(19541, 300, 20, "Set Implant Beta", 0.0, &[(292, 1.15), (98, 200.0)], &[9010]),
    );

    // Launcher + charge tests (missile flight attrs 37, 281, 653, 654 + otherID modifier)
    sde.effects.insert(
        9020,
        make_effect(
            9020,
            "chargeOtherAndShip",
            0,
            vec![
                mod_spec("otherID", "ItemModifier", attr_ids::DAMAGE_MULTIPLIER, 98, 6, None, None),
                mod_spec("shipID", "ItemModifier", attr_ids::CPU_OUTPUT, 98, 2, None, None),
            ],
        ),
    );
    sde.types.insert(
        200,
        make_type(
            200,
            384,
            8,
            "Test Charge",
            0.0,
            &[(98, 25.0), (37, 5000.0), (281, 4.0), (653, 0.2), (654, 0.5)],
            &[9020],
        ),
    );
    sde.types.insert(
        499,
        make_type(
            499,
            508,
            7,
            "Test Launcher",
            0.0,
            &[(attr_ids::CPU, 20.0), (attr_ids::POWER, 10.0), (attr_ids::DAMAGE_MULTIPLIER, 1.0), (37, 9999.0)],
            &[12, 16],
        ),
    );

    // Siege module (6582), Afterburner (6731), MWD without 986 attr (6730)
    sde.effects.insert(6582, make_effect(6582, "moduleBonusSiege", 1, vec![]));
    sde.types.insert(
        20280,
        make_type(20280, 515, 7, "Siege I", 0.0, &[(attr_ids::CPU, 50.0), (attr_ids::POWER, 100.0)], &[12, 16, 6582]),
    );
    sde.effects.insert(6731, make_effect(6731, "moduleBonusAfterburner", 1, vec![]));
    sde.types.insert(
        12058,
        make_type(
            12058,
            46,
            7,
            "10MN Afterburner",
            0.0,
            &[(attr_ids::SPEED_FACTOR, 130.0), (567u16, 500_000.0), (attr_ids::MASS_ADDITION, 1_500_000.0)],
            &[13, 16, 6731],
        ),
    );
    sde.effects.insert(6730, make_effect(6730, "moduleBonusMicrowarpdrive", 1, vec![]));
    sde.types.insert(
        5973,
        make_type(
            5973,
            46,
            7,
            "50MN MWD",
            0.0,
            &[(attr_ids::SPEED_FACTOR, 500.0), (567u16, 1_500_000.0), (attr_ids::MASS_ADDITION, 15_000_000.0)],
            &[13, 16, 6730],
        ),
    );

    // Booster with side-effect effect (effect_category 0) & Neurotoxin Control skill 25538
    sde.effects.insert(
        9031,
        make_effect(
            9031,
            "neurotoxinControl",
            0,
            vec![mod_spec("charID", "LocationGroupModifier", 1126, 292, 6, Some(303), None)],
        ),
    );
    sde.effects.insert(
        9030,
        make_effect(
            9030,
            "boosterPenalized",
            0,
            vec![
                mod_spec("shipID", "ItemModifier", attr_ids::SHIELD_CAPACITY, 1126, 6, None, None),
                mod_spec("itemID", "ItemModifier", 1126, 98, 2, None, None),
            ],
        ),
    );
    sde.types.insert(
        25538,
        make_type(25538, 273, 16, "Neurotoxin Control", 0.0, &[(292, -5.0)], &[132, 9031]),
    );
    sde.types.insert(
        28672,
        make_type(28672, 303, 20, "Standard Blue Pill", 0.0, &[(1126, -20.0), (98, 1.0)], &[9030]),
    );

    // Dbuff collections: 100 (Minimum, unstackable, ship item + location_group_modifiers)
    sde.dbuff_collections.insert(
        100,
        DbuffCollectionInfo {
            dbuff_id: 100,
            name: "ArmorBurst".to_string(),
            operation: "PostPercent".to_string(),
            aggregate_mode: "Minimum".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            target_attribute_ids: vec![attr_ids::ARMOR_HP],
            item_modifiers: vec![attr_ids::ARMOR_HP],
            location_modifiers: vec![],
            location_group_modifiers: vec![DbuffGroupModifier {
                attribute_id: attr_ids::DAMAGE_MULTIPLIER,
                group_id: 508,
            }],
            location_required_skill_modifiers: vec![DbuffSkillModifier {
                attribute_id: attr_ids::MAX_RANGE,
                skill_id: 3436,
            }],
        },
    );
    sde.dbuff_collections.insert(
        101,
        DbuffCollectionInfo {
            dbuff_id: 101,
            name: "AdditiveBurst".to_string(),
            operation: "ModAdd".to_string(),
            aggregate_mode: "Additive".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            target_attribute_ids: vec![attr_ids::SHIELD_CAPACITY],
            item_modifiers: vec![attr_ids::SHIELD_CAPACITY],
            location_modifiers: vec![],
            location_group_modifiers: vec![],
            location_required_skill_modifiers: vec![],
        },
    );

    // Stance type 34319 with category-0 and category-1 effects
    sde.effects.insert(
        9040,
        make_effect(
            9040,
            "stancePassive",
            0,
            vec![mod_spec("shipID", "ItemModifier", attr_ids::MAX_VELOCITY, 98, 2, None, None)],
        ),
    );
    sde.types.insert(
        34319,
        make_type(34319, 1306, 87, "Confessor Defense", 0.0, &[(98, 25.0)], &[9040, 9003]),
    );

    // Projected EWAR types without module effect (attr-only detection: 767 TD, 848 GD, 566 Damp)
    sde.types.insert(
        99001,
        make_type(99001, 208, 7, "Tracking Disruptor Attr", 0.0, &[(767, -25.0), (54, 10_000.0), (158, 10_000.0)], &[]),
    );
    sde.types.insert(
        99002,
        make_type(99002, 208, 7, "Guidance Disruptor Attr", 0.0, &[(848, -20.0), (54, 10_000.0), (158, 10_000.0)], &[]),
    );
    sde.types.insert(
        99003,
        make_type(99003, 208, 7, "Sensor Damp Attr", 0.0, &[(566, -15.0), (54, 10_000.0), (158, 10_000.0)], &[]),
    );

    sde
}

#[test]
fn t3c_offline_subsystem_charge_skills_and_implants_edges() {
    let sde = build_eval_sde();
    let mut eng = ShipFittingEngine::from_sde(1, 29984, &sde);
    eng.set_character_skills(HashMap::from([
        (3436, 5),
        (3437, 0),    // level 0 skipped!
        (3438, 3),    // only effect 132
        (3439, 4),    // parameterized 1730 fallback to 292
        (999_999, 5), // unknown skill type skipped
    ]));

    // Fit T3C subsystem offline vs online in slot 125.
    eng.modules.insert(
        10,
        FittedModule {
            item_id: 10,
            type_id: 30100,
            flag_id: 125,
            state: ModuleState::Offline,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.recalculate_stats(&sde);
    assert_eq!(eng.ship.base_attributes[&attr_ids::ARMOR_HP], 2000.0);
    // Bring subsystem online -> armor +500
    eng.modules.get_mut(&10).unwrap().state = ModuleState::Online;
    eng.recalculate_stats(&sde);
    assert!((eng.ship.get_attribute(attr_ids::ARMOR_HP, &sde) - 2500.0).abs() < 1e-6);

    // Launcher with pre-existing attr 37 and loaded charge (plus unknown charge fallback)
    eng.modules.insert(
        20,
        FittedModule {
            item_id: 20,
            type_id: 499,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(200),
            charge_quantity: 10,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    // Missile flight attr 37 already exists on launcher (9999.0), while 281 comes from charge (4.0)
    assert!((eng.get_module_effective_attribute(20, 37, &sde) - 9999.0).abs() < 1e-6);
    assert!((eng.get_module_effective_attribute(20, 281, &sde) - 4.0).abs() < 1e-6);
    // Missing charge attr falls back to launcher attr & non-duplicate modifiers copied; missing mod returns 0
    assert!((eng.get_charge_effective_attribute(20, attr_ids::CPU, &sde) - 20.0).abs() < 1e-6);
    assert_eq!(eng.get_module_effective_attribute(999, attr_ids::CPU, &sde), 0.0);

    // Unknown charge type ignored cleanly
    eng.modules.get_mut(&20).unwrap().charge_type_id = Some(888_888);
    let _ = eng.evaluated_context(&sde);
    eng.modules.get_mut(&20).unwrap().charge_type_id = Some(200);

    // Single implant vs multi_implant_set (>1): PreMul op 0 suppressed when 1 implant, applied when 2 implants!
    eng.set_implants(vec![19540, 999_888]); // one valid + one unknown
    let ctx_single = eng.evaluated_context(&sde);
    let imp_single = ctx_single
        .char_located_items
        .iter()
        .find(|it| it.type_id == 19540)
        .expect("implant 19540");
    assert!((imp_single.get_attribute(98, &sde) - 100.0).abs() < 1e-6);
    eng.set_implants(vec![19540, 19541]);
    let ctx_multi = eng.evaluated_context(&sde);
    let imp_multi = ctx_multi
        .char_located_items
        .iter()
        .find(|it| it.type_id == 19540)
        .expect("implant 19540 multi");
    assert!(imp_multi.get_attribute(98, &sde) > 120.0);

    // Offline / hypothetical build_effective_module_state vs Overloaded clone of Online module.
    let hyp = eng.build_effective_module_state(999, 499, false, &sde);
    assert!(hyp.get_attribute(attr_ids::DAMAGE_MULTIPLIER, &sde) >= 1.0);
    let over_clone = eng.build_effective_module_state(20, 499, true, &sde);
    assert!(over_clone.get_attribute(attr_ids::DAMAGE_MULTIPLIER, &sde) > 1.0);

    // Drones add/update/remove/missing
    eng.add_drone(50, 2488, &sde);
    eng.add_drone(50, 2488, &sde); // update existing entry
    assert!(eng.drone_state(50, &sde).is_some());
    assert!(eng.get_drone_effective_attribute(50, attr_ids::DAMAGE_MULTIPLIER, &sde) >= 2.0);
    assert_eq!(eng.get_drone_effective_attribute(999, attr_ids::DAMAGE_MULTIPLIER, &sde), 0.0);
    assert!(eng.remove_drone(50, &sde));
    assert!(!eng.remove_drone(50, &sde));
}

#[test]
fn booster_gang_burst_stance_prop_siege_and_projected_ewar_edges() {
    let sde = build_eval_sde();
    let mut eng = ShipFittingEngine::from_sde(2, 29984, &sde);
    eng.set_character_skills(HashMap::from([(25538, 4), (3437, 0), (888_777, 5)]));
    eng.set_boosters(vec![28672, 777_666]); // booster_side_effects == None applies all side effects; unknown ignored
    let shield_none = eng.evaluated_context(&sde).ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde);
    assert!(shield_none < 3000.0);

    // Empty rolled side-effect vec for booster 28672 suppresses effect 9030
    eng.booster_side_effects.insert(28672, vec![]);
    let shield_suppressed = eng.evaluated_context(&sde).ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde);
    assert!((shield_suppressed - 3000.0).abs() < 1e-6);

    // Stance: unknown vs valid (active_stance_type_id)
    eng.active_stance_type_id = Some(999_111);
    assert!((eng.evaluated_context(&sde).ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 200.0).abs() < 1e-6);
    eng.active_stance_type_id = Some(34319);
    assert!((eng.evaluated_context(&sde).ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 225.0).abs() < 1e-6);

    // Gang bursts: multiple bursts with same dbuff_id aggregated (Minimum vs Additive) + unknown dbuff + direct modifiers
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 1,
        source_type_id: None,
        dbuff_id: Some(100),
        value: 10.0,
        modifiers: vec![AttributeModifier {
            source_item_id: 1,
            effect_id: None,
            target_attribute_id: attr_ids::SIGNATURE_RADIUS,
            operator: ModifierOperator::ModAdd,
            value: 5.0,
            stacking_penalized: false,
        }],
    });
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 2,
        source_type_id: None,
        dbuff_id: Some(100),
        value: -20.0, // Minimum keeps -20.0!
        modifiers: vec![],
    });
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 3,
        source_type_id: None,
        dbuff_id: Some(101),
        value: 100.0,
        modifiers: vec![],
    });
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 4,
        source_type_id: None,
        dbuff_id: Some(101),
        value: 50.0, // Additive sums to 150.0!
        modifiers: vec![],
    });
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 5,
        source_type_id: None,
        dbuff_id: Some(999_555), // unknown dbuff skipped cleanly
        value: 99.0,
        modifiers: vec![],
    });
    let burst_ctx = eng.evaluated_context(&sde);
    assert!((burst_ctx.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde) - 155.0).abs() < 1e-6);
    assert!((burst_ctx.ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde) - 3150.0).abs() < 1e-6);

    // Prop modules: AB 6731 & MWD 6730 without attr 986 + Siege 6582 Online vs Active
    eng.modules.insert(
        30,
        FittedModule {
            item_id: 30,
            type_id: 12058,
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
        31,
        FittedModule {
            item_id: 31,
            type_id: 5973,
            flag_id: 20,
            state: ModuleState::Active,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.modules.insert(
        32,
        FittedModule {
            item_id: 32,
            type_id: 20280,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );

    // Upsert/remove projected EWAR using attr-based detection (767 TD, 848 GD, 566 Damp)
    for (idx, tid) in [(100i64, 99001u32), (101, 99002), (102, 99003)] {
        let mut eff_mod = DogmaItemState::from_sde(idx, tid, &sde);
        eff_mod.modifiers = vec![];
        eng.upsert_projected_ewar(
            ProjectedEwarEffect {
                source_module_id: idx,
                source_module_type_id: tid,
                charge_type_id: None,
                distance: 5_000.0, // within optimal 10_000
                overloaded: false,
                effective_module: eff_mod,
            },
            &sde,
        );
    }
    // Near-zero distance-attenuated bonus (way past optimal + falloff) skipped cleanly
    eng.upsert_projected_ewar(
        ProjectedEwarEffect {
            source_module_id: 100,
            source_module_type_id: 99001,
            charge_type_id: None,
            distance: 500_000.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(100, 99001, &sde),
        },
        &sde,
    );
    eng.recalculate_stats(&sde);

    // Online Siege does NOT grant EWAR immunity; Active Siege DOES set resist attrs to 0.0!
    assert!(eng.evaluated_context(&sde).ship.get_attribute(2112, &sde) > 0.5);
    eng.modules.get_mut(&32).unwrap().state = ModuleState::Active;
    let siege_ctx = eng.evaluated_context(&sde);
    assert!((siege_ctx.ship.get_attribute(2112, &sde)).abs() < 1e-6);

    assert!(eng.remove_projected_ewar(100, &sde));
    assert!(!eng.remove_projected_ewar(100, &sde));
}

#[test]
fn dbuff_modifiers_stacking_and_missile_damage_mutants() {
    let mut sde = build_eval_sde();
    sde.attributes.insert(
        212,
        DogmaAttributeType {
            attribute_id: 212,
            name: "missileDamageMultiplier".to_string(),
            default_value: 1.0,
            high_is_good: true,
            stackable: true,
            published: true,
        },
    );
    sde.attributes.insert(
        attr_ids::KINETIC_DAMAGE,
        DogmaAttributeType {
            attribute_id: attr_ids::KINETIC_DAMAGE,
            name: "kineticDamage".to_string(),
            default_value: 0.0,
            high_is_good: true,
            stackable: true,
            published: true,
        },
    );
    sde.effects.insert(
        9101,
        make_effect(
            9101,
            "charMissileDmgBonus",
            0,
            vec![mod_spec("charID", "ItemModifier", 212, 98, 2, None, None)],
        ),
    );
    sde.types.insert(
        3320,
        make_type(3320, 273, 16, "Missile Mult Skill", 0.0, &[(280, 1.0), (98, 0.25)], &[132, 9101]),
    );
    sde.types.insert(
        201,
        make_type(201, 384, 8, "Test Missile Charge", 0.0, &[(attr_ids::KINETIC_DAMAGE, 100.0)], &[]),
    );
    sde.types.insert(
        500,
        make_type(
            500,
            515,
            7,
            "Skill-Req Module",
            0.0,
            &[(182, 3436.0), (attr_ids::MAX_RANGE, 1000.0)],
            &[12, 16],
        ),
    );
    sde.types.insert(
        2489,
        make_type(2489, 100, 18, "Hobgoblin Unreq", 5.0, &[(attr_ids::MAX_RANGE, 500.0)], &[]),
    );
    sde.dbuff_collections.insert(
        102,
        DbuffCollectionInfo {
            dbuff_id: 102,
            name: "FullDbuff".to_string(),
            operation: "PostPercent".to_string(),
            aggregate_mode: "Minimum".to_string(),
            display_name_id: None,
            show_output_value_in_ui: None,
            target_attribute_ids: vec![attr_ids::DAMAGE_MULTIPLIER],
            item_modifiers: vec![],
            location_modifiers: vec![attr_ids::DAMAGE_MULTIPLIER, attr_ids::CPU],
            location_group_modifiers: vec![DbuffGroupModifier {
                attribute_id: attr_ids::MAX_VELOCITY,
                group_id: 508,
            }],
            location_required_skill_modifiers: vec![DbuffSkillModifier {
                attribute_id: attr_ids::MAX_RANGE,
                skill_id: 3436,
            }],
        },
    );

    let mut eng = ShipFittingEngine::from_sde(1, 29984, &sde);
    eng.modules.insert(
        60,
        FittedModule {
            item_id: 60,
            type_id: 499,
            flag_id: 27,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(201),
            charge_quantity: 10,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.modules.insert(
        61,
        FittedModule {
            item_id: 61,
            type_id: 500,
            flag_id: 28,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.add_drone(70, 2488, &sde);
    eng.add_drone(71, 2489, &sde);
    eng.gang_bursts.push(GangBurstEffect {
        source_item_id: 9,
        source_type_id: None,
        dbuff_id: Some(102),
        value: 10.0,
        modifiers: vec![],
    });

    // Base evaluation (missileDamageMultiplier == 1.0)
    let ctx = eng.evaluated_context(&sde);
    assert_eq!(ctx.character.item_id, -1);

    let mod_match = ctx
        .ship_located_items
        .iter()
        .find(|m| m.item_id == 60)
        .expect("mod 60");
    let loc_dmg = mod_match
        .modifiers
        .iter()
        .find(|m| m.target_attribute_id == attr_ids::DAMAGE_MULTIPLIER && m.source_item_id == 9)
        .expect("location_modifiers DAMAGE_MULTIPLIER");
    assert!(loc_dmg.stacking_penalized);
    let loc_cpu = mod_match
        .modifiers
        .iter()
        .find(|m| m.target_attribute_id == attr_ids::CPU && m.source_item_id == 9)
        .expect("location_modifiers CPU");
    assert!(!loc_cpu.stacking_penalized);
    let grp_vel = mod_match
        .modifiers
        .iter()
        .find(|m| m.target_attribute_id == attr_ids::MAX_VELOCITY && m.source_item_id == 9)
        .expect("location_group_modifiers MAX_VELOCITY");
    assert!(grp_vel.stacking_penalized);
    assert!(!mod_match
        .modifiers
        .iter()
        .any(|m| m.target_attribute_id == attr_ids::MAX_RANGE && m.source_item_id == 9));

    let mod_req = ctx
        .ship_located_items
        .iter()
        .find(|m| m.item_id == 61)
        .expect("mod 61");
    assert!(!mod_req
        .modifiers
        .iter()
        .any(|m| m.target_attribute_id == attr_ids::MAX_VELOCITY && m.source_item_id == 9));
    let srq_mod = mod_req
        .modifiers
        .iter()
        .find(|m| m.target_attribute_id == attr_ids::MAX_RANGE && m.source_item_id == 9)
        .expect("location_required_skill_modifiers MAX_RANGE on mod 61");
    assert!(srq_mod.stacking_penalized);

    let drone_req = ctx
        .owned_items
        .iter()
        .find(|d| d.item_id == 70)
        .expect("drone 70");
    let drone_srq = drone_req
        .modifiers
        .iter()
        .find(|m| m.target_attribute_id == attr_ids::MAX_RANGE && m.source_item_id == 9)
        .expect("location_required_skill_modifiers MAX_RANGE on drone 70");
    assert!(drone_srq.stacking_penalized);
    let drone_unreq = ctx
        .owned_items
        .iter()
        .find(|d| d.item_id == 71)
        .expect("drone 71");
    assert!(!drone_unreq
        .modifiers
        .iter()
        .any(|m| m.target_attribute_id == attr_ids::MAX_RANGE && m.source_item_id == 9));

    // When character missileDamageMultiplier == 1.0, no -1 missile damage modifier is added.
    let charge_unboosted = ctx
        .owned_items
        .iter()
        .find(|c| c.item_id == 60 && c.type_id == 201)
        .expect("unboosted charge");
    assert!(!charge_unboosted
        .modifiers
        .iter()
        .any(|m| m.target_attribute_id == attr_ids::KINETIC_DAMAGE));
    assert!((charge_unboosted.get_attribute(attr_ids::KINETIC_DAMAGE, &sde) - 100.0).abs() < 1e-6);

    // When character missileDamageMultiplier != 1.0 (1.25 via skill 3320), source_item_id == -1 modifier is pushed.
    eng.set_character_skills(HashMap::from([(3320, 1)]));
    let ctx_boosted = eng.evaluated_context(&sde);
    let charge_boosted = ctx_boosted
        .owned_items
        .iter()
        .find(|c| c.item_id == 60 && c.type_id == 201)
        .expect("boosted charge");
    let kin_mod = charge_boosted
        .modifiers
        .iter()
        .find(|m| m.target_attribute_id == attr_ids::KINETIC_DAMAGE)
        .expect("missile damage modifier on charge");
    assert_eq!(kin_mod.source_item_id, -1);
    assert_eq!(kin_mod.operator, ModifierOperator::PostMul);
    assert!((kin_mod.value - 1.25).abs() < 1e-6);
    assert!((charge_boosted.get_attribute(attr_ids::KINETIC_DAMAGE, &sde) - 125.0).abs() < 1e-6);
}
