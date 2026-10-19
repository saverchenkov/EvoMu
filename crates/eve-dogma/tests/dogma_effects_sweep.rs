use eve_dogma::{
    compile_effect_to_vm, compile_modifier_spec_to_vm, stacking_penalty_factor, DogmaDomain,
    DogmaEffectContext, DogmaItemState, DogmaModifierFunc, DogmaVmOpcode, ModifierOperator,
    REQUIRED_SKILL_ATTR_IDS,
};
use eve_sde::{attr_ids, SdeRegistry, TypeId};
use serde_json::Value;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn expected_single_modifier_value(base_val: f64, op: ModifierOperator) -> (f64, f64) {
    match op {
        ModifierOperator::PreAssign | ModifierOperator::PostAssign => (42.0, 42.0),
        ModifierOperator::PreMul | ModifierOperator::PostMul => (1.25, base_val * 1.25),
        ModifierOperator::PreDiv | ModifierOperator::PostDiv => (1.25, base_val / 1.25),
        ModifierOperator::ModAdd => (15.0, base_val + 15.0),
        ModifierOperator::ModSub => (15.0, base_val - 15.0),
        ModifierOperator::PostPercent
        | ModifierOperator::PostPercentUnNerfed
        | ModifierOperator::SpecialSkillRelationship => (15.0, base_val * 1.15),
    }
}

#[test]
// parity: plan:DG-1, feat:dogma.modifier-engine, feat:fitting.stacking-penalty, feat:skills.skill-bonus-application, feat:modules.nanofiber-overdrive
fn test_t2_dogma_all_effects_and_modifier_info_sweep() {
    eve_testkit::attest(&[
        "feat:dogma.modifier-engine",
        "feat:fitting.stacking-penalty",
        "feat:skills.skill-bonus-application",
        "feat:modules.nanofiber-overdrive",
    ]);
    let root = repo_root();
    let sde = SdeRegistry::with_canonical_seed();
    assert_eq!(sde.effects.len(), 3_427);
    assert_eq!(sde.attributes.len(), 2_871);

    // 1. Validate docs/index/dogma_effects.json (2,854 Crucible/V24 effects + 75 VM opcodes + 250 sample_effects)
    let dogma_index_path = root.join("docs/index/dogma_effects.json");
    let dogma_index_str =
        std::fs::read_to_string(&dogma_index_path).expect("read docs/index/dogma_effects.json");
    let dogma_index: Value =
        serde_json::from_str(&dogma_index_str).expect("parse docs/index/dogma_effects.json");

    assert_eq!(dogma_index["effects_count"].as_u64(), Some(2_854));
    assert_eq!(dogma_index["operands_count"].as_u64(), Some(75));
    let operands = dogma_index["operands"].as_array().expect("operands array");
    assert_eq!(operands.len(), 75);
    for op_meta in operands {
        let op_id = op_meta["operandID"].as_u64().expect("u8 operandID") as u8;
        let _vm_op = DogmaVmOpcode::from_u8(op_id)
            .unwrap_or_else(|| panic!("Missing DogmaVmOpcode for operand {op_id}"));
        let op_key = op_meta["operandKey"].as_str().expect("operandKey");
        assert!(!op_key.is_empty());
    }

    let sample_effects = dogma_index["sample_effects"]
        .as_array()
        .expect("sample_effects array");
    assert_eq!(sample_effects.len(), 250);
    for se in sample_effects {
        let eid = se["effectID"].as_u64().expect("sample effectID") as u16;
        let ecat = se["effectCategory"].as_u64().expect("sample effectCategory") as u16;
        let ename = se["effectName"].as_str().expect("sample effectName");
        assert!(!ename.is_empty());
        assert!(ecat <= 7, "Unexpected sample effectCategory {ecat} on {eid}");
    }

    // 2. Sweep all 3,427 V24.01 Dogma effects in sde.effects (res:/staticdata/dogmaeffects.fsdbinary).
    //    3,210 effects have modifierInfo entries (5,178 modifier specs total) and are evaluated
    //    both individually (verifying exact expected post-modifier value and domain/location isolation
    //    per modifier spec) and as combined effects (verifying per-effect application and removal).
    //    The remaining 217 effects are engine/script/slot effects without modifierInfo entries.
    let mut sorted_eids: Vec<eve_sde::EffectId> = sde.effects.keys().copied().collect();
    sorted_eids.sort_unstable();
    assert_eq!(sorted_eids.len(), 3_427);
    assert_eq!(*sorted_eids.first().unwrap(), 4);
    assert_eq!(*sorted_eids.last().unwrap(), 13_027);

    let mut rep_type_for_group = std::collections::HashMap::<u32, TypeId>::new();
    for (&tid, tinfo) in &sde.types {
        rep_type_for_group.entry(tinfo.group_id).or_insert(tid);
    }

    let mut total_modifiers = 0usize;
    let mut effects_with_modifiers = 0usize;
    let mut effects_without_modifiers = 0usize;
    let mut domain_counts = std::collections::HashMap::<DogmaDomain, usize>::new();
    let mut func_counts = std::collections::HashMap::<DogmaModifierFunc, usize>::new();
    let mut operator_counts = std::collections::BTreeMap::<ModifierOperator, usize>::new();

    for eid in sorted_eids {
        let effect = sde.effects.get(&eid).expect("effect exists");
        assert_eq!(effect.effect_id, eid);
        assert!(
            !effect.name.is_empty(),
            "Effect {eid} has empty effect name"
        );
        assert!(
            effect.effect_category <= 7,
            "Effect {eid} has invalid effect_category {}",
            effect.effect_category
        );

        if effect.modifiers.is_empty() {
            effects_without_modifiers += 1;
            assert!(compile_effect_to_vm(effect, false).is_none());
            continue;
        }

        effects_with_modifiers += 1;
        total_modifiers += effect.modifiers.len();

        // Compile full effect to VM AST (both add and remove trees)
        let vm_add = compile_effect_to_vm(effect, false)
            .unwrap_or_else(|| panic!("Failed to compile add VM tree for effect {eid}"));
        let vm_rm = compile_effect_to_vm(effect, true)
            .unwrap_or_else(|| panic!("Failed to compile remove VM tree for effect {eid}"));
        assert!(
            matches!(
                vm_add.opcode,
                DogmaVmOpcode::Combine
                    | DogmaVmOpcode::AddItmMod
                    | DogmaVmOpcode::AddLocMod
                    | DogmaVmOpcode::AddLocGrpMod
                    | DogmaVmOpcode::AddLocSrqMod
                    | DogmaVmOpcode::AddOwnSrqMod
                    | DogmaVmOpcode::Efct
            ),
            "Unexpected root add opcode {:?} for effect {eid}",
            vm_add.opcode
        );
        assert!(
            matches!(
                vm_rm.opcode,
                DogmaVmOpcode::Combine
                    | DogmaVmOpcode::RmItmMod
                    | DogmaVmOpcode::RmLocMod
                    | DogmaVmOpcode::RmLocGrpMod
                    | DogmaVmOpcode::RmLocSrqMod
                    | DogmaVmOpcode::RmOwnSrqMod
                    | DogmaVmOpcode::Efct
            ),
            "Unexpected root remove opcode {:?} for effect {eid}",
            vm_rm.opcode
        );

        // Evaluate EVERY individual modifier spec on this effect in a fresh isolated context
        // and assert exact target entity, non-target isolation, and exact numerical output.
        for (idx, spec) in effect.modifiers.iter().enumerate() {
            let domain = DogmaDomain::from_sde_str(&spec.domain).unwrap_or_else(|| {
                panic!("Effect {eid} has unknown modifier domain '{}'", spec.domain)
            });
            let func = DogmaModifierFunc::from_sde_str(&spec.func).unwrap_or_else(|| {
                panic!("Effect {eid} has unknown modifier func '{}'", spec.func)
            });
            *domain_counts.entry(domain).or_insert(0) += 1;
            *func_counts.entry(func).or_insert(0) += 1;

            let single_vm = compile_modifier_spec_to_vm(spec, false)
                .unwrap_or_else(|| panic!("Effect {eid} modifier {idx} failed VM compilation"));

            let source_item_id = 900_000 + i64::from(eid);
            let mut ctx = DogmaEffectContext::new(
                DogmaItemState::from_sde(source_item_id, 1236, &sde),
                DogmaItemState::from_sde(1_001, 587, &sde),
                DogmaItemState::from_sde(2_001, 1373, &sde),
                DogmaItemState::from_sde(3_001, 587, &sde),
                DogmaItemState::from_sde(4_001, 587, &sde),
                DogmaItemState::from_sde(5_001, 35832, &sde),
            );

            if func == DogmaModifierFunc::EffectStopper {
                assert_eq!(single_vm.opcode, DogmaVmOpcode::Efct);
                let stopped_eid = spec
                    .effect_id
                    .unwrap_or_else(|| panic!("Effect {eid} EffectStopper missing effect_id"));
                assert!(
                    sde.effects.contains_key(&stopped_eid),
                    "Effect {eid} EffectStopper references unknown effect_id {stopped_eid}"
                );
                let applied =
                    ctx.apply_modifier_spec_with_effect(source_item_id, Some(eid), spec, &sde);
                assert_eq!(applied, 1);
                assert_eq!(ctx.stopped_target_effects, vec![stopped_eid]);
                continue;
            }

            let op = ModifierOperator::try_from_sde_op(spec.operator).unwrap_or_else(|| {
                panic!(
                    "Effect {eid} modifier {idx} has invalid operator {}",
                    spec.operator
                )
            });
            *operator_counts.entry(op).or_insert(0) += 1;

            assert!(
                sde.attributes.contains_key(&spec.modified_attribute_id),
                "Effect {eid} modifier {idx} references unknown modified_attribute_id {}",
                spec.modified_attribute_id
            );
            assert!(
                sde.attributes.contains_key(&spec.modifying_attribute_id),
                "Effect {eid} modifier {idx} references unknown modifying_attribute_id {}",
                spec.modifying_attribute_id
            );

            let base_val = 100.0;
            let (seed_src_val, expected_val) = expected_single_modifier_value(base_val, op);

            // Seed all 6 domain entities with base_val on modified_attribute_id
            for d in [
                DogmaDomain::Item,
                DogmaDomain::Ship,
                DogmaDomain::Char,
                DogmaDomain::Target,
                DogmaDomain::Other,
                DogmaDomain::Structure,
            ] {
                ctx.entity_for_domain_mut(d)
                    .base_attributes
                    .insert(spec.modified_attribute_id, base_val);
            }
            // Seed source_item (ctx.item) with seed_src_val on modifying_attribute_id
            // Note: if domain == Item and func == ItemModifier and modified == modifying,
            // expected_val is computed from base_val = seed_src_val.
            ctx.item
                .base_attributes
                .insert(spec.modifying_attribute_id, seed_src_val);
            let effective_base_for_item = if spec.modified_attribute_id == spec.modifying_attribute_id
            {
                seed_src_val
            } else {
                base_val
            };
            let (_, expected_item_val) =
                expected_single_modifier_value(effective_base_for_item, op);

            match func {
                DogmaModifierFunc::ItemModifier => {
                    let applied = ctx.apply_modifier_spec_with_effect(
                        source_item_id,
                        Some(eid),
                        spec,
                        &sde,
                    );
                    assert_eq!(applied, 1, "Effect {eid} mod {idx} ItemModifier applied != 1");
                    let actual = ctx
                        .entity_for_domain(domain)
                        .get_attribute(spec.modified_attribute_id, &sde);
                    let exp = if domain == DogmaDomain::Item {
                        expected_item_val
                    } else {
                        expected_val
                    };
                    assert!(
                        (actual - exp).abs() < 1e-6,
                        "Effect {eid} mod {idx} ItemModifier on {domain:?}: got {actual}, expected {exp}"
                    );
                    // Non-target domains must remain untouched at base_val
                    for other_d in [
                        DogmaDomain::Ship,
                        DogmaDomain::Char,
                        DogmaDomain::Target,
                        DogmaDomain::Other,
                        DogmaDomain::Structure,
                    ] {
                        if other_d != domain {
                            assert_eq!(
                                ctx.entity_for_domain(other_d)
                                    .get_attribute(spec.modified_attribute_id, &sde),
                                base_val
                            );
                        }
                    }
                }
                DogmaModifierFunc::LocationModifier => {
                    let mut matching_child = DogmaItemState::from_sde(10_001, 438, &sde);
                    matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);
                    ctx.located_items_for_domain_mut(domain)
                        .push(matching_child);

                    // Also add a child in a different domain to verify domain location isolation
                    let other_domain = if domain == DogmaDomain::Ship {
                        DogmaDomain::Structure
                    } else {
                        DogmaDomain::Ship
                    };
                    let mut isolated_child = DogmaItemState::from_sde(10_002, 438, &sde);
                    isolated_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);
                    ctx.located_items_for_domain_mut(other_domain)
                        .push(isolated_child);

                    let applied = ctx.apply_modifier_spec_with_effect(
                        source_item_id,
                        Some(eid),
                        spec,
                        &sde,
                    );
                    assert_eq!(applied, 1);
                    let actual = ctx.located_items_for_domain(domain)[0]
                        .get_attribute(spec.modified_attribute_id, &sde);
                    assert!(
                        (actual - expected_val).abs() < 1e-6,
                        "Effect {eid} mod {idx} LocationModifier: got {actual}, expected {expected_val}"
                    );
                    // Location entity itself and other_domain child must remain unmodified
                    if domain != DogmaDomain::Item
                        || spec.modified_attribute_id != spec.modifying_attribute_id
                    {
                        assert_eq!(
                            ctx.entity_for_domain(domain)
                                .get_attribute(spec.modified_attribute_id, &sde),
                            base_val
                        );
                    }
                    assert_eq!(
                        ctx.located_items_for_domain(other_domain)[0]
                            .get_attribute(spec.modified_attribute_id, &sde),
                        base_val
                    );
                }
                DogmaModifierFunc::LocationGroupModifier => {
                    let gid = spec.group_id.unwrap_or_else(|| {
                        panic!("Effect {eid} LocationGroupModifier missing group_id")
                    });
                    assert!(
                        sde.groups.contains_key(&gid),
                        "Effect {eid} LocationGroupModifier references unknown group_id {gid}"
                    );
                    let child_tid = rep_type_for_group.get(&gid).copied().unwrap_or(0);
                    let mut matching_child = DogmaItemState::from_sde(20_001, child_tid, &sde);
                    matching_child.group_id = Some(gid);
                    matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);

                    let mut non_matching_child = DogmaItemState::from_sde(20_002, 0, &sde);
                    non_matching_child.group_id = Some(gid.wrapping_add(1).max(1));
                    non_matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);

                    let list = ctx.located_items_for_domain_mut(domain);
                    list.push(matching_child);
                    list.push(non_matching_child);

                    let applied = ctx.apply_modifier_spec_with_effect(
                        source_item_id,
                        Some(eid),
                        spec,
                        &sde,
                    );
                    assert_eq!(applied, 1);
                    let actual = ctx.located_items_for_domain(domain)[0]
                        .get_attribute(spec.modified_attribute_id, &sde);
                    assert!(
                        (actual - expected_val).abs() < 1e-6,
                        "Effect {eid} mod {idx} LocationGroupModifier: got {actual}, expected {expected_val}"
                    );
                    assert_eq!(
                        ctx.located_items_for_domain(domain)[1]
                            .get_attribute(spec.modified_attribute_id, &sde),
                        base_val
                    );
                }
                DogmaModifierFunc::LocationRequiredSkillModifier => {
                    let stid = spec.skill_type_id.unwrap_or_else(|| {
                        panic!("Effect {eid} LocationRequiredSkillModifier missing skill_type_id")
                    });
                    assert!(
                        sde.types.contains_key(&stid),
                        "Effect {eid} LocationRequiredSkillModifier references unknown skill_type_id {stid}"
                    );
                    let mut matching_child = DogmaItemState::from_sde(30_001, 0, &sde);
                    matching_child
                        .base_attributes
                        .insert(REQUIRED_SKILL_ATTR_IDS[0], f64::from(stid));
                    matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);

                    let mut non_matching_child = DogmaItemState::from_sde(30_002, 0, &sde);
                    non_matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);

                    let list = ctx.located_items_for_domain_mut(domain);
                    list.push(matching_child);
                    list.push(non_matching_child);

                    let applied = ctx.apply_modifier_spec_with_effect(
                        source_item_id,
                        Some(eid),
                        spec,
                        &sde,
                    );
                    assert_eq!(applied, 1);
                    let actual = ctx.located_items_for_domain(domain)[0]
                        .get_attribute(spec.modified_attribute_id, &sde);
                    assert!(
                        (actual - expected_val).abs() < 1e-6,
                        "Effect {eid} mod {idx} LocationRequiredSkillModifier: got {actual}, expected {expected_val}"
                    );
                    assert_eq!(
                        ctx.located_items_for_domain(domain)[1]
                            .get_attribute(spec.modified_attribute_id, &sde),
                        base_val
                    );
                }
                DogmaModifierFunc::OwnerRequiredSkillModifier => {
                    let stid = spec.skill_type_id.unwrap_or_else(|| {
                        panic!("Effect {eid} OwnerRequiredSkillModifier missing skill_type_id")
                    });
                    assert!(
                        sde.types.contains_key(&stid),
                        "Effect {eid} OwnerRequiredSkillModifier references unknown skill_type_id {stid}"
                    );
                    let mut matching_child = DogmaItemState::from_sde(40_001, 0, &sde);
                    matching_child
                        .base_attributes
                        .insert(REQUIRED_SKILL_ATTR_IDS[0], f64::from(stid));
                    matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);

                    let mut non_matching_child = DogmaItemState::from_sde(40_002, 0, &sde);
                    non_matching_child
                        .base_attributes
                        .insert(spec.modified_attribute_id, base_val);

                    ctx.owned_items.push(matching_child);
                    ctx.owned_items.push(non_matching_child);

                    let applied = ctx.apply_modifier_spec_with_effect(
                        source_item_id,
                        Some(eid),
                        spec,
                        &sde,
                    );
                    assert_eq!(applied, 1);
                    let actual =
                        ctx.owned_items[0].get_attribute(spec.modified_attribute_id, &sde);
                    assert!(
                        (actual - expected_val).abs() < 1e-6,
                        "Effect {eid} mod {idx} OwnerRequiredSkillModifier: got {actual}, expected {expected_val}"
                    );
                    assert_eq!(
                        ctx.owned_items[1].get_attribute(spec.modified_attribute_id, &sde),
                        base_val
                    );
                }
                DogmaModifierFunc::EffectStopper => unreachable!(),
            }

            // Remove the effect and verify all modifiers are removed and attribute values restore to base
            ctx.remove_effect(source_item_id, effect);
            for d in [
                DogmaDomain::Item,
                DogmaDomain::Ship,
                DogmaDomain::Char,
                DogmaDomain::Target,
                DogmaDomain::Other,
                DogmaDomain::Structure,
            ] {
                assert!(ctx.entity_for_domain(d).modifiers.is_empty());
                let expected_base = if d == DogmaDomain::Item {
                    effective_base_for_item
                } else {
                    base_val
                };
                assert_eq!(
                    ctx.entity_for_domain(d)
                        .get_attribute(spec.modified_attribute_id, &sde),
                    expected_base
                );
                for c in ctx.located_items_for_domain(d) {
                    assert!(c.modifiers.is_empty());
                    assert_eq!(c.get_attribute(spec.modified_attribute_id, &sde), base_val);
                }
            }
            for c in &ctx.owned_items {
                assert!(c.modifiers.is_empty());
                assert_eq!(c.get_attribute(spec.modified_attribute_id, &sde), base_val);
            }
        }
    }

    assert_eq!(effects_with_modifiers, 3_210);
    assert_eq!(effects_without_modifiers, 217);
    assert_eq!(total_modifiers, 5_178);
    assert_eq!(domain_counts.len(), 6);
    assert_eq!(func_counts.len(), 6);
    assert_eq!(func_counts[&DogmaModifierFunc::EffectStopper], 10);

    // 3. Concrete Wiki-Derived Module & Effect Math Verification
    verify_wiki_module_effects(&sde);
}

fn verify_wiki_module_effects(sde: &SdeRegistry) {
    // (A) Overdrive Injector System II (type 1236) on Rifter (type 587)
    //     Effects: 2865 (`velocityBonusOnline`, PostPercent +12.5% on shipID.maxVelocity)
    //              59 (`cargoCapacityMultiply`, PostMul *0.8 on shipID.capacity)
    {
        let overdrive_1 = DogmaItemState::from_sde(10_001, 1236, sde);
        let overdrive_2 = DogmaItemState::from_sde(10_002, 1236, sde);
        let rifter = DogmaItemState::from_sde(1_000, 587, sde);
        assert_eq!(rifter.get_attribute(attr_ids::MAX_VELOCITY, sde), 365.0);
        assert_eq!(rifter.get_attribute(attr_ids::CAPACITY, sde), 140.0);

        let mut ctx = DogmaEffectContext::new(
            overdrive_1,
            rifter,
            DogmaItemState::from_sde(2_000, 1373, sde),
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        let eff_vel = sde.effects.get(&2865).expect("velocityBonusOnline 2865");
        let eff_cargo = sde.effects.get(&59).expect("cargoCapacityMultiply 59");

        ctx.apply_effect(10_001, eff_vel, sde);
        ctx.apply_effect(10_001, eff_cargo, sde);
        assert!(
            (ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) - 365.0 * 1.125).abs() < 1e-6
        );
        assert!((ctx.ship.get_attribute(attr_ids::CAPACITY, sde) - 140.0 * 0.8).abs() < 1e-6);

        // Fit second Overdrive Injector System II -> velocity is stacking penalized S(1), cargo capacity stacks freely
        ctx.item = overdrive_2;
        ctx.apply_effect(10_002, eff_vel, sde);
        ctx.apply_effect(10_002, eff_cargo, sde);
        let expected_vel_2 = 365.0 * 1.125 * (1.0 + 0.125 * stacking_penalty_factor(1));
        let expected_cargo_2 = 140.0 * 0.8 * 0.8;
        assert!(
            (ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) - expected_vel_2).abs() < 1e-6
        );
        assert!(
            (ctx.ship.get_attribute(attr_ids::CAPACITY, sde) - expected_cargo_2).abs() < 1e-6
        );

        // Fit third Overdrive Injector System II -> velocity is stacking penalized S(2), cargo capacity stacks freely
        let overdrive_3 = DogmaItemState::from_sde(10_003, 1236, sde);
        ctx.item = overdrive_3;
        ctx.apply_effect(10_003, eff_vel, sde);
        ctx.apply_effect(10_003, eff_cargo, sde);
        let expected_vel_3 = expected_vel_2 * (1.0 + 0.125 * stacking_penalty_factor(2));
        let expected_cargo_3 = expected_cargo_2 * 0.8;
        assert!(
            (ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) - expected_vel_3).abs() < 1e-6
        );
        assert!(
            (ctx.ship.get_attribute(attr_ids::CAPACITY, sde) - expected_cargo_3).abs() < 1e-6
        );

        // Remove all three Overdrive Injector System II modules via remove_effect and verify base restoration
        for mid in [10_003, 10_002, 10_001] {
            ctx.remove_effect(mid, eff_vel);
            ctx.remove_effect(mid, eff_cargo);
        }
        assert_eq!(ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde), 365.0);
        assert_eq!(ctx.ship.get_attribute(attr_ids::CAPACITY, sde), 140.0);
    }

    // (B) Ballistic Control System II (type 22291) with Heavy Missile Launcher II (type 2410)
    //     Effects: 763 (`missileDMGBonus`, PreMul 1.10 on charID attr 212 `missileDamageMultiplier`)
    //              889 (`missileLauncherSpeedMultiplier`, LocationRequiredSkillModifier skill 3319, PostMul 0.895 on attr 51 `speed`)
    {
        let bcs = DogmaItemState::from_sde(10_010, 22291, sde);
        let mut char_state = DogmaItemState::from_sde(2_000, 1373, sde);
        char_state.base_attributes.insert(212, 1.0); // base missileDamageMultiplier = 1.0
        let lml2 = DogmaItemState::from_sde(20_001, 2410, sde); // Heavy Missile Launcher II (requires 3319 Missile Launcher Operation)
        let ac2 = DogmaItemState::from_sde(20_002, 2881, sde); // 150mm Light AutoCannon II (does NOT require 3319)
        let base_lml_speed = lml2.get_attribute(51, sde);
        let base_ac_speed = ac2.get_attribute(51, sde);
        assert!(base_lml_speed > 0.0);

        let mut ctx = DogmaEffectContext::new(
            bcs,
            DogmaItemState::from_sde(1_000, 587, sde),
            char_state,
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        ctx.ship_located_items.push(lml2);
        ctx.ship_located_items.push(ac2);

        let eff_dmg = sde.effects.get(&763).expect("missileDMGBonus 763");
        let eff_rof = sde
            .effects
            .get(&889)
            .expect("missileLauncherSpeedMultiplier 889");
        assert_eq!(ctx.apply_effect(10_010, eff_dmg, sde), 1);
        assert_eq!(ctx.apply_effect(10_010, eff_rof, sde), 1); // Only lml2 matches skill 3319!

        assert!((ctx.character.get_attribute(212, sde) - 1.10).abs() < 1e-6);
        assert!(
            (ctx.ship_located_items[0].get_attribute(51, sde) - base_lml_speed * 0.895).abs()
                < 1e-6
        );
        assert_eq!(
            ctx.ship_located_items[1].get_attribute(51, sde),
            base_ac_speed
        );
    }

    // (C) Gyrostabilizer II (type 519) with 150mm Light AutoCannon II (type 2881, group 55 Projectile Weapon)
    //     Effects: 89 (`projectileWeaponSpeedMultiply`, LocationGroupModifier group 55, PostMul 0.895 on attr 51 `speed`)
    //              92 (`projectileWeaponDamageMultiply`, LocationGroupModifier group 55, PostMul 1.10 on attr 64 `damageMultiplier`)
    {
        let gyro = DogmaItemState::from_sde(10_020, 519, sde);
        let ac2 = DogmaItemState::from_sde(20_010, 2881, sde); // group 55
        let lml2 = DogmaItemState::from_sde(20_011, 2410, sde); // group 509
        let base_ac_speed = ac2.get_attribute(51, sde);
        let base_ac_dmg = ac2.get_attribute(64, sde);
        let base_lml_speed = lml2.get_attribute(51, sde);

        let mut ctx = DogmaEffectContext::new(
            gyro,
            DogmaItemState::from_sde(1_000, 587, sde),
            DogmaItemState::from_sde(2_000, 1373, sde),
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        ctx.ship_located_items.push(ac2);
        ctx.ship_located_items.push(lml2);

        let eff_rof = sde
            .effects
            .get(&89)
            .expect("projectileWeaponSpeedMultiply 89");
        let eff_dmg = sde
            .effects
            .get(&92)
            .expect("projectileWeaponDamageMultiply 92");
        assert_eq!(ctx.apply_effect(10_020, eff_rof, sde), 1);
        assert_eq!(ctx.apply_effect(10_020, eff_dmg, sde), 1);

        assert!(
            (ctx.ship_located_items[0].get_attribute(51, sde) - base_ac_speed * 0.895).abs() < 1e-6
        );
        assert!(
            (ctx.ship_located_items[0].get_attribute(64, sde) - base_ac_dmg * 1.10).abs() < 1e-6
        );
        assert_eq!(
            ctx.ship_located_items[1].get_attribute(51, sde),
            base_lml_speed
        );
    }

    // (D) Shield Boost Amplifier II (type 24443) with X-Large Shield Booster I (type 10840, requires 3416 Shield Operation)
    //     Effect: 1720 (`shieldBoostAmplifier`, LocationRequiredSkillModifier skill 3416, PostPercent +36% on attr 68 `shieldBonus`)
    {
        let sba2 = DogmaItemState::from_sde(10_030, 24443, sde);
        let msb2 = DogmaItemState::from_sde(20_020, 10840, sde);
        let base_boost = msb2.get_attribute(68, sde);
        assert!(base_boost > 0.0);

        let mut ctx = DogmaEffectContext::new(
            sba2,
            DogmaItemState::from_sde(1_000, 587, sde),
            DogmaItemState::from_sde(2_000, 1373, sde),
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        ctx.ship_located_items.push(msb2);

        let eff_sba = sde.effects.get(&1720).expect("shieldBoostAmplifier 1720");
        assert_eq!(ctx.apply_effect(10_030, eff_sba, sde), 1);
        assert!(
            (ctx.ship_located_items[0].get_attribute(68, sde) - base_boost * 1.36).abs() < 1e-6
        );
    }

    // (E) Warp Scrambler II (type 448) on Target Ship + 5MN Microwarpdrive II (type 440, requires 3454 High Speed Maneuvering)
    //     Effects: 5934 (`warpScrambleBlockMWDWithNPCEffect`: +2 warpScrambleStatus on targetID,
    //                    LocationRequiredSkillModifier skill 3454 on targetID, EffectStopper 6441 & 6442)
    //              3174 (`overloadSelfRangeBonus`: +20% maxRange on itemID)
    {
        let scram2 = DogmaItemState::from_sde(10_040, 448, sde);
        let base_range = scram2.get_attribute(54, sde);
        assert_eq!(base_range, 9_000.0);
        let mwd2 = DogmaItemState::from_sde(30_001, 440, sde);

        let mut ctx = DogmaEffectContext::new(
            scram2,
            DogmaItemState::from_sde(1_000, 587, sde),
            DogmaItemState::from_sde(2_000, 1373, sde),
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        ctx.target_located_items.push(mwd2);

        let eff_scram = sde
            .effects
            .get(&5934)
            .expect("warpScrambleBlockMWDWithNPCEffect 5934");
        let eff_ovl = sde.effects.get(&3174).expect("overloadSelfRangeBonus 3174");

        ctx.apply_effect(10_040, eff_scram, sde);
        assert_eq!(ctx.target.get_attribute(104, sde), 2.0); // warpScrambleStatus == 2
        assert_eq!(ctx.stopped_target_effects, vec![6441, 6442]);

        ctx.apply_effect(10_040, eff_ovl, sde);
        assert!((ctx.item.get_attribute(54, sde) - 10_800.0).abs() < 1e-6);
    }

    // (F) Small Armor Repairer II (type 1183)
    //     Effect: 3200 (`overloadSelfArmorDamageAmountDurationBonus`: +10% armorDamageAmount (84), -15% duration (73) on itemID)
    {
        let sar2 = DogmaItemState::from_sde(10_050, 1183, sde);
        let base_rep = sar2.get_attribute(84, sde);
        let base_dur = sar2.get_attribute(73, sde);
        assert!(base_rep > 0.0 && base_dur > 0.0);

        let mut ctx = DogmaEffectContext::new(
            sar2,
            DogmaItemState::from_sde(1_000, 587, sde),
            DogmaItemState::from_sde(2_000, 1373, sde),
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        let eff_ovl = sde
            .effects
            .get(&3200)
            .expect("overloadSelfArmorDamageAmountDurationBonus 3200");
        assert_eq!(ctx.apply_effect(10_050, eff_ovl, sde), 2);
        assert!((ctx.item.get_attribute(84, sde) - base_rep * 1.10).abs() < 1e-6);
        assert!((ctx.item.get_attribute(73, sde) - base_dur * 0.85).abs() < 1e-6);
    }

    // (G) Multispectrum Shield Hardener II (type 2281) on Rifter (type 587)
    //     Effect: 5230 (`modifyActiveShieldResonancePostPercent`: -32.5% PostPercent on shipID attrs 271..=274)
    {
        let msh2 = DogmaItemState::from_sde(10_060, 2281, sde);
        let rifter = DogmaItemState::from_sde(1_000, 587, sde);
        let base_em = rifter.get_attribute(271, sde);
        let base_expl = rifter.get_attribute(272, sde);
        let base_kin = rifter.get_attribute(273, sde);
        let base_therm = rifter.get_attribute(274, sde);

        let mut ctx = DogmaEffectContext::new(
            msh2,
            rifter,
            DogmaItemState::from_sde(2_000, 1373, sde),
            DogmaItemState::from_sde(3_000, 587, sde),
            DogmaItemState::from_sde(4_000, 587, sde),
            DogmaItemState::from_sde(5_000, 35832, sde),
        );
        let eff_active = sde
            .effects
            .get(&5230)
            .expect("modifyActiveShieldResonancePostPercent 5230");
        assert_eq!(ctx.apply_effect(10_060, eff_active, sde), 4);
        assert!((ctx.ship.get_attribute(271, sde) - base_em * 0.675).abs() < 1e-6);
        assert!((ctx.ship.get_attribute(272, sde) - base_expl * 0.675).abs() < 1e-6);
        assert!((ctx.ship.get_attribute(273, sde) - base_kin * 0.675).abs() < 1e-6);
        assert!((ctx.ship.get_attribute(274, sde) - base_therm * 0.675).abs() < 1e-6);
    }
}
