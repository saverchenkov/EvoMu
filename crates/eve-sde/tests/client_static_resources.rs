//! Client Static Data & SDE Resource Sweeps (`D-1`, `Z-4`).
//!
//! Verifies lossless loading and full-row field coverage for all 213 V24.01 client
//! `.fsdbinary` and `.static` resources, sweeps all Dogma effects across `sde_effects`
//! with full opcode compilation and VM evaluation, and spot-checks exact client data
//! across types, skins, blueprints, clone grades, compressible types, and fighter abilities.

#![forbid(unsafe_code)]

// parity: plan:D-1

use eve_dogma::{
    compile_effect_to_vm, compile_modifier_spec_to_vm, DogmaDomain, DogmaEffectContext,
    DogmaItemState, DogmaModifierFunc, DogmaVmOpcode, ModifierOperator, REQUIRED_SKILL_ATTR_IDS,
};
use eve_sde::{attr_ids, SdeRegistry, TypeId};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
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
// parity: plan:D-1
fn test_client_static_resources_lossless_sweep_and_spot_checks() {
    let root = repo_root();
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real V24.01 SDE SQLite database must open");

    // 1. Verify all 213 resources from manifest & staticdata catalog
    let manifest_path = root.join("data").join("sde_import_manifest.json");
    let manifest_val: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).expect("read data/sde_import_manifest.json"),
    )
    .expect("parse data/sde_import_manifest.json");
    assert_eq!(manifest_val["client_release"].as_str(), Some("V24.01"));

    let catalog_path = root
        .join("docs")
        .join("index")
        .join("staticdata_catalog.json");
    let catalog_arr: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&catalog_path).expect("read docs/index/staticdata_catalog.json"),
    )
    .expect("parse docs/index/staticdata_catalog.json");
    assert_eq!(catalog_arr.len(), 213);
    assert_eq!(sde.resources.len(), 213);

    // 2. Full-row domain entity counts and field completeness verification (D-1, Z-4)
    assert_eq!(sde.categories.len(), 48);
    for cat in sde.categories.values() {
        assert!(!cat.name.is_empty());
    }

    assert_eq!(sde.groups.len(), 1_611);
    for grp in sde.groups.values() {
        assert!(!grp.name.is_empty());
        assert!(sde.categories.contains_key(&grp.category_id));
    }

    // Types full-row check
    assert_eq!(sde.types.len(), 53_288);
    let mut published_types = 0usize;
    let mut with_desc = 0usize;
    let mut with_icon = 0usize;
    let mut with_graphic = 0usize;
    let mut with_meta = 0usize;
    let mut with_effects = 0usize;
    let mut with_attrs = 0usize;

    for t in sde.types.values() {
        assert!(!t.name.is_empty());
        assert!(sde.groups.contains_key(&t.group_id));
        assert!(sde.categories.contains_key(&t.category_id));
        assert!(t.portion_size >= 1);
        assert!(t.mass >= 0.0);
        assert!(t.volume >= 0.0);
        assert!(t.capacity >= 0.0);
        assert!(t.radius >= 0.0);
        assert!(t.base_price >= 0.0);

        if t.published {
            published_types += 1;
        }
        if t.description_id.is_some() {
            with_desc += 1;
        }
        if t.icon_id.is_some() {
            with_icon += 1;
        }
        if t.graphic_id.is_some() {
            with_graphic += 1;
        }
        if t.meta_group_id.is_some() {
            with_meta += 1;
        }
        if !t.effect_ids.is_empty() {
            with_effects += 1;
        }
        if !t.default_attributes.is_empty() {
            with_attrs += 1;
        }

        for &aid in t.default_attributes.keys() {
            assert!(
                sde.attributes.contains_key(&aid),
                "Type {} has unknown attribute {}",
                t.type_id,
                aid
            );
        }
        for &eid in &t.effect_ids {
            assert!(
                sde.effects.contains_key(&eid),
                "Type {} has unknown effect {}",
                t.type_id,
                eid
            );
        }
    }

    assert_eq!(published_types, 27_309);
    assert_eq!(with_desc, 35_030);
    assert_eq!(with_icon, 23_088);
    assert_eq!(with_graphic, 18_816);
    assert_eq!(with_meta, 13_905);
    assert_eq!(with_effects, 16_204);
    assert_eq!(with_attrs, 26_927);

    // Dogma Attributes full-row verification
    assert_eq!(sde.attributes.len(), 2_871);
    let mut published_attrs = 0usize;
    for attr in sde.attributes.values() {
        assert!(!attr.name.is_empty());
        if attr.published {
            published_attrs += 1;
        }
    }
    assert_eq!(published_attrs, 1_277);

    // Dogma Effects full-row verification
    assert_eq!(sde.effects.len(), 3_427);
    let mut published_effects = 0usize;
    let mut warp_safe_effects = 0usize;
    for eff in sde.effects.values() {
        assert!(!eff.name.is_empty());
        assert!(eff.effect_category <= 7);
        if eff.published {
            published_effects += 1;
        }
        if eff.is_warp_safe {
            warp_safe_effects += 1;
        }
    }
    assert_eq!(published_effects, 115);
    assert_eq!(warp_safe_effects, 85);

    // Domain collections verification
    assert_eq!(sde.races.len(), 11);
    assert_eq!(sde.bloodlines.len(), 18);
    assert_eq!(sde.ancestries.len(), 43);
    assert_eq!(sde.schools.len(), 23);
    assert_eq!(sde.regions.len(), 114);
    assert_eq!(sde.constellations.len(), 1_184);
    assert_eq!(sde.solar_systems.len(), 8_490);
    assert_eq!(sde.jumps.len(), 13_978);
    assert_eq!(sde.stations.len(), 5_210);
    assert_eq!(sde.celestial_count, 475_859);
    assert_eq!(sde.factions.len(), 27);
    assert_eq!(sde.npc_corporations.len(), 283);
    assert_eq!(sde.agents.len(), 10_972);
    assert_eq!(sde.market_groups.len(), 2_114);
    assert_eq!(sde.planet_schematics.len(), 68);
    assert_eq!(sde.wormhole_types.len(), 132);
    assert_eq!(sde.npc_spawn_groups.len(), 392);
    assert_eq!(sde.clone_grades.len(), 4);
    assert_eq!(sde.dbuff_collections.len(), 277);
    assert_eq!(sde.fighter_abilities.len(), 36);
    assert_eq!(sde.fighter_abilities_by_type.len(), 94);
    assert_eq!(sde.wrecks.len(), 5_703);
    assert_eq!(sde.compressible_types.len(), 212);
    assert_eq!(sde.type_materials.len(), 9_866);
    assert_eq!(sde.blueprints.len(), 5_100);
    assert_eq!(sde.dynamic_item_attributes.len(), 441);
    assert_eq!(sde.space_components_by_type.len(), 11_238);
    assert_eq!(sde.missions.len(), 2_892);
    assert_eq!(sde.dungeons.len(), 5_516);
    assert_eq!(sde.epic_arcs.len(), 21);
    assert_eq!(sde.node_graphs.len(), 584);
    assert_eq!(sde.station_operations.len(), 69);
    assert_eq!(sde.station_services.len(), 27);
    assert_eq!(sde.skin_licenses.len(), 11_877);
    assert_eq!(sde.skin_materials.len(), 875);
    assert_eq!(sde.skins.len(), 7_048);

    // 3. Spot assertions verifying exact client data values across domains (D-1, Z-4)
    // Types spot checks
    // Raven (638)
    let raven = &sde.types[&638];
    assert_eq!(raven.name, "Raven");
    assert_eq!(raven.group_id, 27);
    assert_eq!(raven.category_id, 6);
    assert_eq!(raven.portion_size, 1);
    assert_eq!(raven.base_price, 153_900_000.0);
    assert_eq!(raven.volume, 470_000.0);
    assert_eq!(raven.mass, 99_300_000.0);
    assert_eq!(raven.radius, 250.0);
    assert!(raven.published);
    assert_eq!(raven.description_id, Some(84205));
    assert_eq!(raven.icon_id, None);
    assert_eq!(raven.graphic_id, Some(43));
    assert_eq!(raven.sound_id, Some(20068));
    assert_eq!(raven.meta_group_id, Some(1));
    assert_eq!(raven.meta_level, Some(0));
    assert_eq!(raven.tech_level, Some(1));
    assert_eq!(raven.default_attributes[&attr_ids::MAX_VELOCITY], 113.0);
    assert_eq!(
        raven.default_attributes[&attr_ids::SHIELD_CAPACITY],
        7_700.0
    );
    assert_eq!(raven.default_attributes[&attr_ids::ARMOR_HP], 6_380.0);
    assert_eq!(raven.default_attributes[&attr_ids::HP_STRUCT], 7_040.0);
    assert_eq!(raven.default_attributes[&attr_ids::RIG_SIZE], 3.0);

    // Tritanium (34)
    let tritanium = &sde.types[&34];
    assert_eq!(tritanium.name, "Tritanium");
    assert_eq!(tritanium.group_id, 18);
    assert_eq!(tritanium.category_id, 4);
    assert_eq!(tritanium.portion_size, 1);
    assert_eq!(tritanium.volume, 0.01);
    assert_eq!(tritanium.description_id, Some(87134));
    assert_eq!(tritanium.icon_id, Some(22));
    assert_eq!(tritanium.graphic_id, None);
    assert_eq!(tritanium.sound_id, None);
    assert_eq!(tritanium.meta_group_id, None);

    // Rifter (587)
    let rifter = &sde.types[&587];
    assert_eq!(rifter.name, "Rifter");
    assert_eq!(rifter.group_id, 25);
    assert_eq!(rifter.category_id, 6);
    assert_eq!(rifter.portion_size, 1);
    assert_eq!(rifter.description_id, Some(93841));
    assert_eq!(rifter.icon_id, None);
    assert_eq!(rifter.graphic_id, Some(46));
    assert_eq!(rifter.sound_id, Some(20078));
    assert_eq!(rifter.meta_group_id, Some(1));
    assert_eq!(rifter.meta_level, Some(0));
    assert_eq!(rifter.tech_level, Some(1));

    // Nemesis (11377)
    let nemesis = &sde.types[&11377];
    assert_eq!(nemesis.name, "Nemesis");
    assert_eq!(nemesis.group_id, 834);
    assert_eq!(nemesis.category_id, 6);
    assert_eq!(nemesis.portion_size, 1);
    assert_eq!(nemesis.description_id, Some(93939));
    assert_eq!(nemesis.icon_id, None);
    assert_eq!(nemesis.graphic_id, Some(1909));
    assert_eq!(nemesis.sound_id, Some(20072));
    assert_eq!(nemesis.meta_group_id, Some(2));
    assert_eq!(nemesis.meta_level, Some(5));
    assert_eq!(nemesis.tech_level, Some(2));

    // Skins spot checks
    let skin5 = &sde.skins[&5];
    assert_eq!(skin5.skin_id, 5);
    assert_eq!(skin5.internal_name, "Megathron Quafe");
    assert_eq!(skin5.skin_material_id, 3);
    assert_eq!(skin5.types, vec![641]);
    assert!(skin5.allow_ccp_devs);
    assert!(!skin5.visible_serenity);
    assert!(skin5.visible_tranquility);
    assert!(!skin5.is_structure_skin);

    let mat1 = &sde.skin_materials[&1];
    assert_eq!(mat1.skin_material_id, 1);
    assert_eq!(mat1.display_name_id, Some(505346));
    assert_eq!(mat1.material_set_id, Some(1));

    // Blueprints spot checks
    let rifter_bp = &sde.blueprints[&691];
    assert_eq!(rifter_bp.blueprint_type_id, 691);
    assert_eq!(rifter_bp.product_type_id, 587);
    assert_eq!(rifter_bp.product_quantity, 1);
    assert_eq!(rifter_bp.manufacturing_time_sec, 6_000);
    assert!(rifter_bp.materials.iter().any(|m| m.type_id == 34)); // Tritanium
    assert!(rifter_bp.materials.iter().any(|m| m.type_id == 35)); // Pyerite

    let raven_bp = &sde.blueprints[&688];
    assert_eq!(raven_bp.blueprint_type_id, 688);
    assert_eq!(raven_bp.product_type_id, 638);
    assert_eq!(raven_bp.product_quantity, 1);
    assert_eq!(raven_bp.manufacturing_time_sec, 18_000);

    // Clone grades spot checks
    let alpha_grade = &sde.clone_grades[&1];
    assert_eq!(alpha_grade.grade_id, 1);
    assert!(!alpha_grade.alpha_skill_caps.is_empty());
    for (&sk_tid, &lvl) in &alpha_grade.alpha_skill_caps {
        assert!(sde.types.contains_key(&sk_tid));
        assert!(lvl <= 5);
    }

    // Compressible types spot checks
    for (&raw_tid, &comp_tid) in &sde.compressible_types {
        let raw_t = &sde.types[&raw_tid];
        let comp_t = &sde.types[&comp_tid];
        assert!(
            comp_t.volume < raw_t.volume,
            "Compressed type {} volume {} must be < raw type {} volume {}",
            comp_t.name,
            comp_t.volume,
            raw_t.name,
            raw_t.volume
        );
    }
    assert_eq!(sde.compressible_types[&1230], 62516); // Veldspar -> Compressed Veldspar
    assert_eq!(sde.types[&62516].volume, 0.001);
    assert_eq!(sde.compressible_types[&1228], 62520); // Scordite -> Compressed Scordite
    assert_eq!(sde.compressible_types[&1224], 62524); // Pyroxeres -> Compressed Pyroxeres
    assert_eq!(sde.compressible_types[&18], 62528); // Plagioclase -> Compressed Plagioclase

    // Fighter abilities spot checks
    let fa2 = &sde.fighter_abilities[&2];
    assert_eq!(fa2.ability_id, 2);
    assert_eq!(fa2.tooltip_text_id, Some(516650));

    let fa18 = &sde.fighter_abilities[&18];
    assert_eq!(fa18.ability_id, 18);
    assert_eq!(fa18.turret_graphic_id, Some(11515));
    assert_eq!(fa18.tooltip_text_id, Some(516660));

    for (&ftid, slots) in &sde.fighter_abilities_by_type {
        assert!(sde.types.contains_key(&ftid));
        assert!(!slots.is_empty());
        for slot in slots {
            assert!(slot.slot_index <= 2);
            assert!(sde.fighter_abilities.contains_key(&slot.ability_id));
        }
    }
}

#[test]
// parity: plan:D-1
fn test_sweep_all_dogma_effects_across_sde_effects_table() {
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("Real SDE SQLite database must open");

    // 1. Verify direct SQLite access to sde_effects view / sde_dogma_effects table
    let conn = Connection::open(&db_path).expect("open connection to SDE sqlite");
    let effects_view_count: i64 = conn
        .query_row("SELECT count(*) FROM sde_effects", [], |r| r.get(0))
        .expect("query count from sde_effects view");
    assert_eq!(effects_view_count, 3_427);

    let effects_tbl_count: i64 = conn
        .query_row("SELECT count(*) FROM sde_dogma_effects", [], |r| r.get(0))
        .expect("query count from sde_dogma_effects table");
    assert_eq!(effects_tbl_count, 3_427);

    // 2. Representative type mapping for LocationGroupModifier
    let mut rep_type_for_group = HashMap::<u32, TypeId>::new();
    for (&tid, tinfo) in &sde.types {
        rep_type_for_group.entry(tinfo.group_id).or_insert(tid);
    }

    // 3. Sweep all 3,427 Dogma effects across sde.effects
    let mut total_effects = 0usize;
    let mut total_modifiers = 0usize;
    let mut effects_with_modifiers = 0usize;
    let mut effects_without_modifiers = 0usize;
    let mut total_evaluations = 0usize;

    let mut domain_counts = HashMap::<DogmaDomain, usize>::new();
    let mut func_counts = HashMap::<DogmaModifierFunc, usize>::new();
    let mut op_counts = BTreeMap::<ModifierOperator, usize>::new();
    let mut attested_effect_rows = Vec::<String>::with_capacity(3_427);

    let mut sorted_eids: Vec<eve_sde::EffectId> = sde.effects.keys().copied().collect();
    sorted_eids.sort_unstable();

    for eid in sorted_eids {
        total_effects += 1;
        let effect = &sde.effects[&eid];
        assert_eq!(effect.effect_id, eid);
        assert!(!effect.name.is_empty(), "Empty name on effect {eid}");
        assert!(
            effect.effect_category <= 7,
            "Invalid category on effect {eid}"
        );

        if effect.modifiers.is_empty() {
            effects_without_modifiers += 1;
            assert!(compile_effect_to_vm(effect, false).is_none());
            assert!(compile_effect_to_vm(effect, true).is_none());
            continue;
        }

        effects_with_modifiers += 1;
        total_modifiers += effect.modifiers.len();
        attested_effect_rows.push(format!("effect:{eid}"));

        // Compile combined effect to VM AST (both add and remove)
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

        // Evaluate every individual modifier spec without unhandled opcode panics
        for (idx, spec) in effect.modifiers.iter().enumerate() {
            total_evaluations += 1;

            let domain = DogmaDomain::from_sde_str(&spec.domain).unwrap_or_else(|| {
                panic!("Effect {eid} modifier {idx} unknown domain {}", spec.domain)
            });
            let func = DogmaModifierFunc::from_sde_str(&spec.func).unwrap_or_else(|| {
                panic!("Effect {eid} modifier {idx} unknown func {}", spec.func)
            });

            *domain_counts.entry(domain).or_insert(0) += 1;
            *func_counts.entry(func).or_insert(0) += 1;

            let single_vm_add = compile_modifier_spec_to_vm(spec, false)
                .unwrap_or_else(|| panic!("Effect {eid} modifier {idx} add VM compile failure"));
            let single_vm_rm = compile_modifier_spec_to_vm(spec, true)
                .unwrap_or_else(|| panic!("Effect {eid} modifier {idx} rm VM compile failure"));

            assert!(
                single_vm_add.opcode != DogmaVmOpcode::UserError,
                "Opcode compilation error on effect {eid} modifier {idx}"
            );
            assert!(
                single_vm_rm.opcode != DogmaVmOpcode::UserError,
                "Opcode compilation error on effect {eid} modifier {idx}"
            );

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
                assert_eq!(single_vm_add.opcode, DogmaVmOpcode::Efct);
                let stopped_eid = spec.effect_id.expect("EffectStopper missing effect_id");
                assert!(
                    sde.effects.contains_key(&stopped_eid),
                    "EffectStopper references unknown effect_id {stopped_eid}"
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
            *op_counts.entry(op).or_insert(0) += 1;

            assert!(
                sde.attributes.contains_key(&spec.modified_attribute_id),
                "Effect {eid} modifier {idx} unknown modified_attribute_id {}",
                spec.modified_attribute_id
            );
            assert!(
                sde.attributes.contains_key(&spec.modifying_attribute_id),
                "Effect {eid} modifier {idx} unknown modifying_attribute_id {}",
                spec.modifying_attribute_id
            );

            let base_val = 100.0;
            let (seed_src_val, expected_val) = expected_single_modifier_value(base_val, op);

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

            ctx.item
                .base_attributes
                .insert(spec.modifying_attribute_id, seed_src_val);

            let effective_base_for_item =
                if spec.modified_attribute_id == spec.modifying_attribute_id {
                    seed_src_val
                } else {
                    base_val
                };
            let (_, expected_item_val) =
                expected_single_modifier_value(effective_base_for_item, op);

            match func {
                DogmaModifierFunc::ItemModifier | DogmaModifierFunc::ItemModifierWithFalloff => {
                    let applied =
                        ctx.apply_modifier_spec_with_effect(source_item_id, Some(eid), spec, &sde);
                    assert_eq!(
                        applied, 1,
                        "Effect {eid} mod {idx} ItemModifier applied != 1"
                    );
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

                    let applied =
                        ctx.apply_modifier_spec_with_effect(source_item_id, Some(eid), spec, &sde);
                    assert_eq!(applied, 1);
                    let actual = ctx.located_items_for_domain(domain)[0]
                        .get_attribute(spec.modified_attribute_id, &sde);
                    assert!(
                        (actual - expected_val).abs() < 1e-6,
                        "Effect {eid} mod {idx} LocationModifier: got {actual}, expected {expected_val}"
                    );
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

                    let applied =
                        ctx.apply_modifier_spec_with_effect(source_item_id, Some(eid), spec, &sde);
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

                    let applied =
                        ctx.apply_modifier_spec_with_effect(source_item_id, Some(eid), spec, &sde);
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

                    let applied =
                        ctx.apply_modifier_spec_with_effect(source_item_id, Some(eid), spec, &sde);
                    assert_eq!(applied, 1);
                    let actual = ctx.owned_items[0].get_attribute(spec.modified_attribute_id, &sde);
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
        }
    }

    assert_eq!(total_effects, 3_427);
    assert_eq!(effects_with_modifiers, 3_210);
    assert_eq!(effects_without_modifiers, 217);
    assert_eq!(total_modifiers, 5_178);
    assert!(
        total_evaluations >= 4_000,
        "Expected at least 4,000 effect/modifier evaluations, got {total_evaluations}"
    );
    assert_eq!(total_evaluations, 5_178);

    // Verify all 6 domains and all modifier functions were covered
    assert_eq!(domain_counts.len(), 6);
    assert!(domain_counts[&DogmaDomain::Ship] > 3_000);
    assert!(domain_counts[&DogmaDomain::Char] > 1_000);
    assert_eq!(func_counts.len(), 6);
    assert_eq!(func_counts[&DogmaModifierFunc::ItemModifier], 1_800);
    assert_eq!(func_counts[&DogmaModifierFunc::LocationModifier], 29);
    assert_eq!(func_counts[&DogmaModifierFunc::LocationGroupModifier], 805);
    assert_eq!(
        func_counts[&DogmaModifierFunc::LocationRequiredSkillModifier],
        1_611
    );
    assert_eq!(
        func_counts[&DogmaModifierFunc::OwnerRequiredSkillModifier],
        923
    );
    assert_eq!(func_counts[&DogmaModifierFunc::EffectStopper], 10);

    assert_eq!(attested_effect_rows.len(), 3_210);
}
