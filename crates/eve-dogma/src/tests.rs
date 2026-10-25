    use super::*;
    use eve_sde::attr_ids;
    use std::collections::HashMap;

    #[test]
    fn test_sde_operator_and_vm_opcode_mapping() {
        assert_eq!(
            ModifierOperator::try_from_sde_op(-1),
            Some(ModifierOperator::PreAssign)
        );
        assert_eq!(
            ModifierOperator::try_from_sde_op(8),
            Some(ModifierOperator::PostPercentUnNerfed)
        );
        assert_eq!(
            ModifierOperator::try_from_sde_op(9),
            Some(ModifierOperator::SpecialSkillRelationship)
        );
        assert_eq!(DogmaVmOpcode::from_u8(1), Some(DogmaVmOpcode::Add));
        assert_eq!(DogmaVmOpcode::from_u8(75), Some(DogmaVmOpcode::TransferCap));
        assert_eq!(DogmaVmOpcode::from_u8(76), None);
    }

    #[test]
    fn test_stacking_penalty_coefficients() {
        assert!((stacking_penalty_factor(0) - 1.0).abs() < 1e-6);
        assert!((stacking_penalty_factor(1) - 0.869118).abs() < 1e-4);
        assert!((stacking_penalty_factor(2) - 0.570583).abs() < 1e-4);
        assert!((stacking_penalty_factor(3) - 0.282955).abs() < 1e-4);
    }

    #[test]
    fn test_rifter_velocity_with_overdrives_and_skills() {
        let sde = SdeRegistry::with_canonical_seed();
        let mut rifter = DogmaItemState::from_sde(1001, 587, &sde);

        assert_eq!(rifter.get_attribute(attr_ids::MAX_VELOCITY, &sde), 365.0);

        rifter.add_modifier(AttributeModifier {
            source_item_id: 2001,
            effect_id: None,
            target_attribute_id: attr_ids::MAX_VELOCITY,
            operator: ModifierOperator::PostPercent,
            value: 25.0,
            stacking_penalized: false,
        });

        for id in [3001, 3002] {
            rifter.add_modifier(AttributeModifier {
                source_item_id: id,
                effect_id: None,
                target_attribute_id: attr_ids::MAX_VELOCITY,
                operator: ModifierOperator::PostPercent,
                value: 12.5,
                stacking_penalized: true,
            });
        }

        let effective_vel = rifter.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let expected = 365.0 * 1.25 * (1.0 + 0.125) * (1.0 + 0.125 * stacking_penalty_factor(1));
        assert!((effective_vel - expected).abs() < 1e-6);
    }

    #[test]
    fn test_dogma_effect_context_domain_isolation_and_semantics() {
        let sde = SdeRegistry::with_canonical_seed();

        // 1. LocationModifier must modify ONLY items located in the target domain, NOT the location entity itself,
        //    and per-domain location lists (shipID vs structureID vs charID vs targetID) must be strictly isolated.
        let mut src_mod = DogmaItemState::from_sde(100, 1236, &sde); // Overdrive II (Module, category 7)
        src_mod.base_attributes.insert(20, 25.0);
        let mut ship = DogmaItemState::from_sde(1, 587, &sde);
        ship.base_attributes.insert(37, 200.0);
        let mut structure = DogmaItemState::from_sde(2, 35832, &sde);
        structure.base_attributes.insert(37, 200.0);

        let mut ctx = DogmaEffectContext::new(
            src_mod,
            ship,
            DogmaItemState::from_sde(3, 1373, &sde),
            DogmaItemState::from_sde(4, 587, &sde),
            DogmaItemState::from_sde(5, 587, &sde),
            structure,
        );
        let mut ship_child = DogmaItemState::from_sde(10, 438, &sde);
        ship_child.base_attributes.insert(37, 100.0);
        let mut struct_child = DogmaItemState::from_sde(11, 438, &sde);
        struct_child.base_attributes.insert(37, 100.0);
        ctx.ship_located_items.push(ship_child);
        ctx.structure_located_items.push(struct_child);

        let loc_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationModifier".into(),
            modified_attribute_id: 37,
            modifying_attribute_id: 20,
            operator: 6, // PostPercent (+25%)
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        assert_eq!(ctx.apply_modifier_spec_with_effect(100, Some(9001), &loc_spec, &sde), 1);
        // Ship itself must NOT be modified by LocationModifier
        assert_eq!(ctx.ship.get_attribute(37, &sde), 200.0);
        // Item located in shipID is modified (+25% -> 125.0)
        assert!((ctx.ship_located_items[0].get_attribute(37, &sde) - 125.0).abs() < 1e-6);
        // Item located in structureID is isolated and untouched (100.0)
        assert_eq!(ctx.structure_located_items[0].get_attribute(37, &sde), 100.0);

        // 2. LocationRequiredSkillModifier must NOT apply to items that have no required skills
        let mut no_skill_item = DogmaItemState {
            item_id: 12,
            type_id: 0,
            group_id: Some(10),
            category_id: Some(7),
            base_attributes: HashMap::from([(51, 1000.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let srq_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationRequiredSkillModifier".into(),
            modified_attribute_id: 51,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: None,
            skill_type_id: Some(3319),
            effect_id: None,
        };
        let srq_vm = compile_modifier_spec_to_vm(&srq_spec, false).unwrap();
        srq_vm.execute_on_target(100, &ctx.item, &mut no_skill_item, &sde);
        assert!(
            no_skill_item.modifiers.is_empty(),
            "Item with no required skills must not match LocationRequiredSkillModifier"
        );

        // 3. LocationGroupModifier must NOT apply when the target item's group is unknown (0 / None)
        let mut unknown_grp_item = DogmaItemState {
            item_id: 13,
            type_id: 0,
            group_id: None,
            category_id: None,
            base_attributes: HashMap::from([(64, 100.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let grp_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationGroupModifier".into(),
            modified_attribute_id: 64,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: Some(55),
            skill_type_id: None,
            effect_id: None,
        };
        let grp_vm = compile_modifier_spec_to_vm(&grp_spec, false).unwrap();
        grp_vm.execute_on_target(100, &ctx.item, &mut unknown_grp_item, &sde);
        assert!(
            unknown_grp_item.modifiers.is_empty(),
            "Item with unknown group must not match LocationGroupModifier"
        );

        // 4. Per-effect removal: removing one effect from source_item_id must leave other effects from the same source intact
        let eff_a = DogmaEffectType {
            effect_id: 9010,
            name: "effA".into(),
            effect_category: 4,
            is_offensive: false,
            is_assistance: false,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 2, // ModAdd (+25)
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        };
        let eff_b = DogmaEffectType {
            effect_id: 9011,
            name: "effB".into(),
            effect_category: 4,
            is_offensive: false,
            is_assistance: false,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 6, // PostPercent (+25%)
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        };
        ctx.apply_effect(100, &eff_a, &sde);
        ctx.apply_effect(100, &eff_b, &sde);
        assert_eq!(ctx.ship.modifiers.len(), 2);
        assert!((ctx.ship.get_attribute(37, &sde) - (200.0 + 25.0) * 1.25).abs() < 1e-6);

        // Remove only eff_a; eff_b from the same source_item_id (100) must remain active!
        ctx.remove_effect(100, &eff_a);
        assert_eq!(ctx.ship.modifiers.len(), 1);
        assert_eq!(ctx.ship.modifiers[0].effect_id, Some(9011));
        assert!((ctx.ship.get_attribute(37, &sde) - 200.0 * 1.25).abs() < 1e-6);

        // 5. Source-category stacking-penalty exemptions:
        //    Skills (category 16), Ships (category 6), Implants (category 20), Subsystems (category 32),
        //    and Boosters (group 303) are exempt (stacking_penalized == false),
        //    whereas Modules (category 7) are stacking-penalized (stacking_penalized == true).
        let mut skill_src = DogmaItemState::from_sde(201, 3449, &sde); // Navigation (Skill, category 16)
        skill_src.base_attributes.insert(20, 25.0);
        let mut ship_src = DogmaItemState::from_sde(202, 587, &sde); // Rifter (Ship, category 6)
        ship_src.base_attributes.insert(20, 25.0);
        let implant_src = DogmaItemState {
            item_id: 203,
            type_id: 0,
            group_id: Some(300),
            category_id: Some(20), // Implant (category 20)
            base_attributes: HashMap::from([(20, 25.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let subsys_src = DogmaItemState {
            item_id: 204,
            type_id: 0,
            group_id: Some(954),
            category_id: Some(32), // Subsystem (category 32)
            base_attributes: HashMap::from([(20, 25.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let booster_src = DogmaItemState {
            item_id: 205,
            type_id: 0,
            group_id: Some(303), // Booster (group 303)
            category_id: Some(20),
            base_attributes: HashMap::from([(20, 25.0)]),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };
        let mut target_ship = DogmaItemState::from_sde(1, 587, &sde);
        let pct_vm = compile_modifier_spec_to_vm(&eff_b.modifiers[0], false).unwrap();
        pct_vm.execute_on_target_with_effect(201, Some(9011), &skill_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(202, Some(9012), &ship_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(203, Some(9013), &implant_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(204, Some(9014), &subsys_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(205, Some(9015), &booster_src, &mut target_ship, &sde);
        pct_vm.execute_on_target_with_effect(100, Some(9016), &ctx.item, &mut target_ship, &sde);
        assert!(!target_ship.modifiers[0].stacking_penalized);
        assert!(!target_ship.modifiers[1].stacking_penalized);
        assert!(!target_ship.modifiers[2].stacking_penalized);
        assert!(!target_ship.modifiers[3].stacking_penalized);
        assert!(!target_ship.modifiers[4].stacking_penalized);
        assert!(target_ship.modifiers[5].stacking_penalized);

        // Module modifying a stackable attribute (38 `capacity`, stackable == true) must have stacking_penalized == false
        let cap_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "ItemModifier".into(),
            modified_attribute_id: 38,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        let cap_vm = compile_modifier_spec_to_vm(&cap_spec, false).unwrap();
        cap_vm.execute_on_target_with_effect(100, Some(9017), &ctx.item, &mut target_ship, &sde);
        assert!(!target_ship.modifiers[6].stacking_penalized);

        // Multi-attribute effect modifying 3 distinct non-stackable attributes (37 maxVelocity, 70 agility, 552 signatureRadius)
        // must evaluate rank 0 (1.0 penalty factor) per attribute independently without cross-attribute penalty contamination.
        let mut multi_attr_ship = DogmaItemState::from_sde(99, 587, &sde);
        multi_attr_ship.base_attributes.insert(37, 100.0);
        multi_attr_ship.base_attributes.insert(70, 100.0);
        multi_attr_ship.base_attributes.insert(552, 100.0);
        for (idx, attr_id) in [37_u16, 70, 552].into_iter().enumerate() {
            let spec = EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_id,
                modifying_attribute_id: 20,
                operator: 6, // +25%
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            };
            let vm = compile_modifier_spec_to_vm(&spec, false).unwrap();
            vm.execute_on_target_with_effect(
                100,
                Some(9030 + idx as u16),
                &ctx.item,
                &mut multi_attr_ship,
                &sde,
            );
        }
        assert!((multi_attr_ship.get_attribute(37, &sde) - 125.0).abs() < 1e-6);
        assert!((multi_attr_ship.get_attribute(70, &sde) - 125.0).abs() < 1e-6);
        assert!((multi_attr_ship.get_attribute(552, &sde) - 125.0).abs() < 1e-6);

        // 6. OwnerRequiredSkillModifier must modify ONLY items in ctx.owned_items that require
        //    the specified skill, and NEVER modify ship_located_items, ship, character, or owned items
        //    that do not require that skill.
        let mut owned_matching = DogmaItemState::from_sde(301, 438, &sde); // 1MN Afterburner II (requires 3450 Afterburner)
        owned_matching.base_attributes.insert(73, 10_000.0);
        let mut owned_non_matching = DogmaItemState::from_sde(302, 1236, &sde); // Overdrive II (does NOT require 3450)
        owned_non_matching.base_attributes.insert(73, 10_000.0);
        let mut unowned_ship_located = DogmaItemState::from_sde(303, 438, &sde); // Same type 438, but in ship_located_items only
        unowned_ship_located.base_attributes.insert(73, 10_000.0);

        let mut char_src = DogmaItemState::from_sde(300, 3450, &sde); // Afterburner skill
        char_src.base_attributes.insert(20, -10.0);
        let mut owner_ctx = DogmaEffectContext::new(
            char_src,
            DogmaItemState::from_sde(1, 587, &sde),
            DogmaItemState::from_sde(3, 1373, &sde),
            DogmaItemState::from_sde(4, 587, &sde),
            DogmaItemState::from_sde(5, 587, &sde),
            DogmaItemState::from_sde(2, 35832, &sde),
        );
        owner_ctx.ship.base_attributes.insert(73, 10_000.0);
        owner_ctx.character.base_attributes.insert(73, 10_000.0);
        owner_ctx.ship_located_items.push(unowned_ship_located);
        owner_ctx.owned_items.push(owned_matching);
        owner_ctx.owned_items.push(owned_non_matching);

        let orq_spec = EffectModifierSpec {
            domain: "charID".into(),
            func: "OwnerRequiredSkillModifier".into(),
            modified_attribute_id: 73,
            modifying_attribute_id: 20,
            operator: 6, // PostPercent (-10%)
            group_id: None,
            skill_type_id: Some(3450),
            effect_id: None,
        };
        assert_eq!(
            owner_ctx.apply_modifier_spec_with_effect(300, Some(9020), &orq_spec, &sde),
            1
        );
        assert!((owner_ctx.owned_items[0].get_attribute(73, &sde) - 9_000.0).abs() < 1e-6);
        assert_eq!(owner_ctx.owned_items[1].get_attribute(73, &sde), 10_000.0);
        assert_eq!(owner_ctx.ship_located_items[0].get_attribute(73, &sde), 10_000.0);
        assert_eq!(owner_ctx.ship.get_attribute(73, &sde), 10_000.0);
        assert_eq!(owner_ctx.character.get_attribute(73, &sde), 10_000.0);
    }

    #[test]
    // parity:, feat:dogma.modifier-engine, feat:fitting.stacking-penalty, feat:skills.skill-bonus-application, feat:modules.nanofiber-overdrive
    fn test_ship_fitting_thermodynamics_boosters_and_abyssal_roll() {
        eve_testkit::attest(&[
            "feat:dogma.modifier-engine",
            "feat:fitting.stacking-penalty",
            "feat:skills.skill-bonus-application",
            "feat:modules.nanofiber-overdrive",
        ]);
        let sde = SdeRegistry::with_canonical_seed();

        // 1. Prove Rifter (587) with Navigation V (3449 -> 5) and 2x Overdrive Injector System II (1236)
        //    fitted through ShipFittingEngine matches exact published attribute values:
        //    - Base Rifter maxVelocity (37) = 365.0 m/s, capacity (38) = 140.0 m3
        //    - Navigation V (category 16 skill, unpenalized): +25% maxVelocity (5% * level 5)
        //    - 2x Overdrive Injector System II (category 7 module, effects 59 & 2865):
        //      +12.5% maxVelocity (stacking-penalized across the 2 modules)
        //      and -20% cargo capacity (PostMul 0.8 * 0.8, unpenalized)
        {
            let mut rifter_fit = ShipFittingEngine::from_sde(1000, 587, &sde);
            rifter_fit.set_character_skills(HashMap::from([
                (3449, 5), // Navigation V
                (3394, 2), // Hull Upgrades II
            ]));
            rifter_fit.fit_and_online_module(4001, 1236, 11, &sde).unwrap();
            rifter_fit.fit_and_online_module(4002, 1236, 12, &sde).unwrap();

            let expected_vel =
                365.0 * 1.25 * (1.0 + 0.125) * (1.0 + 0.125 * stacking_penalty_factor(1));
            let actual_vel = rifter_fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
            assert!(
                (actual_vel - expected_vel).abs() < 1e-6,
                "Rifter + Navigation V + 2x Overdrive II maxVelocity mismatch: got {actual_vel}, expected {expected_vel}"
            );

            let expected_cap = 140.0 * 0.8 * 0.8;
            let actual_cap = rifter_fit.ship.get_attribute(attr_ids::CAPACITY, &sde);
            assert!(
                (actual_cap - expected_cap).abs() < 1e-6,
                "Rifter + 2x Overdrive II cargo capacity mismatch: got {actual_cap}, expected {expected_cap}"
            );

            // Fit Nanofiber Internal Structure II (2605) in slot 13 and Inertial Stabilizers II (1405) in slot 12
            // to verify all 3 passive speed/agility low-slot module families (Overdrive, Nanofiber, Inertial Stabilizer)
            rifter_fit.fit_and_online_module(4002, 1405, 12, &sde).unwrap();
            rifter_fit.fit_and_online_module(4003, 2605, 13, &sde).unwrap();

            let base_agility = sde.types[&587].default_attributes[&attr_ids::AGILITY];
            let base_hull_hp = sde.types[&587].default_attributes[&attr_ids::HP_STRUCT];
            let base_sig = sde.types[&587].default_attributes[&attr_ids::SIGNATURE_RADIUS];

            // Velocity: Navigation V (+25% unpenalized), Overdrive II (+12.5% rank 0), Nanofiber II (+9.5% rank 1)
            let expected_mixed_vel =
                365.0 * 1.25 * (1.0 + 0.125) * (1.0 + 0.095 * stacking_penalty_factor(1));
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - expected_mixed_vel)
                    .abs()
                    < 1e-6
            );

            // Agility: Inertial Stabilizers II (-20.0% rank 0), Nanofiber II (-15.75% rank 1)
            let expected_agility =
                base_agility * (1.0 - 0.20) * (1.0 - 0.1575 * stacking_penalty_factor(1));
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::AGILITY, &sde) - expected_agility).abs()
                    < 1e-6
            );

            // Structure HP: Nanofiber II (PostMul 0.8)
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::HP_STRUCT, &sde) - base_hull_hp * 0.8)
                    .abs()
                    < 1e-6
            );

            // Signature radius: Inertial Stabilizers II (+11% PostPercent)
            assert!(
                (rifter_fit.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde)
                    - base_sig * 1.11)
                    .abs()
                    < 1e-6
            );
        }

        let mut fit = ShipFittingEngine::from_sde(1001, 587, &sde);

        // Fitting without trained skills must fail
        assert!(fit.fit_and_online_module(5001, 438, 19, &sde).is_err());

        fit.set_character_skills(HashMap::from([
            (3449, 4), // Navigation IV
            (3450, 4), // Afterburner IV
            (3452, 4), // Acceleration Control IV
            (3394, 2), // Hull Upgrades II
            (3300, 5), // Gunnery V
            (3302, 5), // Small Projectile Turret V
            (33094, 5), // Medium Precursor Weapon V
        ]));

        // Heavy Entropic Disintegrator II (47918) is restricted to Triglavian cruisers/HACs and must be rejected on Rifter (587)
        assert!(fit.fit_and_online_module(5099, 47918, 27, &sde).is_err());

        // Fit 1MN Afterburner II (type 438) and Overdrive Injector System II (type 1236)
        fit.fit_and_online_module(5001, 438, 19, &sde).unwrap();
        fit.fit_and_online_module(5002, 1236, 11, &sde).unwrap();
        fit.activate_module(5001, true, &sde).unwrap();

        assert!(fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) > 800.0);
        assert_eq!(fit.ship.get_attribute(attr_ids::CPU_LOAD, &sde), 15.0);

        // Verify LocationRequiredSkillModifier from trained Afterburner IV (3450 -> 4, -5%/lvl duration)
        // and Acceleration Control IV (3452 -> 4, +5%/lvl speedFactor) plus overloadSelfSpeedBonus (3175, +50% speedFactor)
        // were applied to the fitted Afterburner II (5001)
        let fitted_ab_ovl = fit.module_state(5001, &sde).unwrap();
        assert!((fitted_ab_ovl.get_attribute(attr_ids::DURATION, &sde) - 10_000.0 * 0.8).abs() < 1e-6);
        assert!((fitted_ab_ovl.get_attribute(attr_ids::SPEED_FACTOR, &sde) - 135.0 * 1.2 * 1.5).abs() < 1e-6);

        // Deactivate 1MN Afterburner II and verify active/overload effects are removed cleanly while passive/online effects remain
        fit.deactivate_module(5001, &sde).unwrap();
        let fitted_ab_online = fit.module_state(5001, &sde).unwrap();
        assert!((fitted_ab_online.get_attribute(attr_ids::SPEED_FACTOR, &sde) - 135.0 * 1.2).abs() < 1e-6);
        let expected_passive_vel = 365.0 * 1.20 * 1.125;
        assert!(
            (fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - expected_passive_vel).abs() < 1e-6
        );

        // Thermodynamics check
        let heat = thermodynamics::step_rack_heat(0.0, 0.04, 0.01, 10.0);
        assert!(heat > 0.3 && heat < 0.4);
        assert_eq!(thermodynamics::effective_heat_damage(1.4, 5), 1.4 * 0.75);

        // Booster side-effect check
        let penalty = evaluate_booster_side_effect(0.4, -20.0, 5, 5, 0.1).unwrap();
        assert!((penalty - (-15.0)).abs() < 1e-6);

        // Abyssal mutaplasmid roll check (47748 = Gravid 1MN Afterburner Mutaplasmid, 0.95..1.15x SPEED_FACTOR)
        let ab = sde.types.get(&438).unwrap();
        let muta = sde.dynamic_item_attributes.get(&47748).unwrap();
        let mut rolls = HashMap::new();
        rolls.insert(attr_ids::SPEED_FACTOR, 1.0); // Max roll (1.15x)
        let rolled = roll_abyssal_attributes(&ab.default_attributes, &muta.attribute_ranges, &rolls);
        assert!((rolled[&attr_ids::SPEED_FACTOR] - 135.0 * 1.15).abs() < 1e-4);
    }

    #[test]
    fn test_dg1_implant_sets_t3d_stances_drones_ewar_and_polarized() {
        let sde = SdeRegistry::with_canonical_seed();

        // 1. High-grade Snake full set (19540, 19551, 19553, 19554, 19555, 19556) compounding on Rifter (587)
        let mut snake_fit = ShipFittingEngine::from_sde(2000, 587, &sde);
        let base_vel = snake_fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let set_mult = 1.15_f64.powi(5) * 3.0;
        snake_fit.set_implants(vec![19540, 19551, 19553, 19554, 19555, 19556]);
        snake_fit.recalculate_stats(&sde);
        let mut expected_snake_vel = base_vel;
        for base_imp_pct in [0.5, 0.625, 0.75, 0.875, 1.0] {
            expected_snake_vel *= 1.0 + (base_imp_pct * set_mult) / 100.0;
        }
        let actual_snake_vel = snake_fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        assert!(
            (actual_snake_vel - expected_snake_vel).abs() < 1e-5,
            "High-grade Snake velocity mismatch: got {actual_snake_vel}, expected {expected_snake_vel}"
        );

        // 2. Confessor (34317) T3D stances (Defense 34319, Sharpshooter 34321, Propulsion 34323) + 10s cooldown
        let mut confessor = ShipFittingEngine::from_sde(2001, 34317, &sde);
        let conf_base_agility = confessor.ship.get_attribute(attr_ids::AGILITY, &sde);
        let conf_base_em_res = confessor.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, &sde);
        assert_eq!(confessor.switch_ship_stance(34319, 1_000, &sde).unwrap(), 34319);
        assert!(
            (confessor.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, &sde)
                - conf_base_em_res / 1.5)
                .abs()
                < 1e-6
        );
        // Switching before 10,000 ms cooldown expires must fail
        assert!(confessor.switch_ship_stance(34323, 5_000, &sde).is_err());
        // Switching at 11,000 ms succeeds and applies Propulsion Mode (`PostDiv` 1.5 on agility)
        assert_eq!(confessor.switch_ship_stance(34323, 11_000, &sde).unwrap(), 34323);
        assert!(
            (confessor.ship.get_attribute(attr_ids::AGILITY, &sde) - conf_base_agility / 1.5)
                .abs()
                < 1e-6
        );

        // 3. Drone damage skills (Light Drone Operation 24241, Gallente Drone Spec 12486, Drone Interfacing 3442, Gallente Cruiser 3332)
        let mut vexor = ShipFittingEngine::from_sde(2002, 626, &sde);
        vexor.set_character_skills(HashMap::from([
            (3436, 5),  // Drones V
            (24241, 5), // Light Drone Operation V (+25%)
            (12486, 5), // Gallente Drone Specialization V (+10% on T2 Gallente drones only)
            (3442, 5),  // Drone Interfacing V (+50%)
            (3332, 5),  // Gallente Cruiser V (+50% hull bonus)
        ]));
        vexor.add_drone(9001, 2454, &sde); // Hobgoblin I (T1)
        vexor.add_drone(9002, 2456, &sde); // Hobgoblin II (T2)
        let hob1_base = sde.types[&2454].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
        let hob2_base = sde.types[&2456].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
        assert!(
            (vexor.get_drone_effective_attribute(9001, attr_ids::DAMAGE_MULTIPLIER, &sde)
                - hob1_base * 1.25 * 1.50 * 1.50)
                .abs()
                < 1e-5
        );
        assert!(
            (vexor.get_drone_effective_attribute(9002, attr_ids::DAMAGE_MULTIPLIER, &sde)
                - hob2_base * 1.25 * 1.10 * 1.50 * 1.50)
                .abs()
                < 1e-5
        );

        // 4. Polarized 200mm AutoCannon (34284) zeroes all shield, armor, and hull resistances (resonance = 1.0)
        let mut rifter_pol = ShipFittingEngine::from_sde(2003, 587, &sde);
        rifter_pol.set_character_skills(HashMap::from([
            (3300, 5),
            (3302, 5),
            (3318, 5),
            (3394, 5),
            (11084, 4),
        ]));
        rifter_pol.fit_and_online_module(7001, 2048, 11, &sde).unwrap(); // Damage Control II
        assert!(rifter_pol.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, &sde) < 0.6);
        rifter_pol.fit_and_online_module(7002, 34284, 27, &sde).unwrap(); // Polarized 200mm AutoCannon
        for res_attr in [109_u16, 110, 111, 113, 267, 268, 269, 270, 271, 272, 273, 274] {
            assert!(
                (rifter_pol.ship.get_attribute(res_attr, &sde) - 1.0).abs() < 1e-6,
                "Polarized weapon failed to force resonance attribute {res_attr} to 1.0"
            );
        }
    }

    #[test]
    fn test_skill_check_tool_target_skills_and_verify_target_group() {
        let sde = SdeRegistry::with_canonical_seed();
        let mut item = DogmaItemState::from_sde(1001, 587, &sde);

        let verify_ok = DogmaExpressionNode {
            opcode: DogmaVmOpcode::VerifyTargetGroup,
            value_f64: Some(25.0),
            attribute_id: None,
            operator: None,
            arg1: None,
            arg2: None,
        };
        assert_eq!(verify_ok.evaluate(&item, &sde), 1.0);

        let verify_fail = DogmaExpressionNode {
            opcode: DogmaVmOpcode::VerifyTargetGroup,
            value_f64: Some(26.0),
            attribute_id: None,
            operator: None,
            arg1: None,
            arg2: None,
        };
        assert_eq!(verify_fail.evaluate(&item, &sde), 0.0);

        let skill_check_node = DogmaExpressionNode {
            opcode: DogmaVmOpcode::SkillCheck,
            value_f64: None,
            attribute_id: None,
            operator: None,
            arg1: Some(Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefType,
                value_f64: Some(3329.0),
                attribute_id: None,
                operator: None,
                arg1: None,
                arg2: None,
            })),
            arg2: Some(Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefInt,
                value_f64: Some(3.0),
                attribute_id: None,
                operator: None,
                arg1: None,
                arg2: None,
            })),
        };
        assert_eq!(skill_check_node.evaluate(&item, &sde), 0.0);
        item.set_skill_level(3329, 2);
        assert_eq!(skill_check_node.evaluate(&item, &sde), 0.0);
        item.set_skill_level(3329, 3);
        assert_eq!(skill_check_node.evaluate(&item, &sde), 1.0);

        let tool_check = DogmaExpressionNode {
            opcode: DogmaVmOpcode::ToolTargetSkills,
            value_f64: None,
            attribute_id: None,
            operator: None,
            arg1: None,
            arg2: None,
        };
        let mut untrained_item = DogmaItemState::from_sde(1002, 587, &sde);
        assert_eq!(tool_check.evaluate(&untrained_item, &sde), 0.0);
        untrained_item.set_skill_level(3329, 1);
        assert_eq!(tool_check.evaluate(&untrained_item, &sde), 1.0);
    }
