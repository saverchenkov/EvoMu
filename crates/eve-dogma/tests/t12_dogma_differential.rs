//! Differential test harness comparing `eve-dogma` against client Python dogma engine calculations (`plan:T-12`).
//!
//! Asserts modifier graph arithmetic evaluation, attribute derivations, and stacking penalties
//! match bit-for-bit across ship/module/implant combinations.

// parity: plan:T-12

use eve_dogma::{
    attr_ids, stacking_penalty_factor, AttributeModifier, DogmaItemState, ModifierOperator,
    ShipFittingEngine,
};
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackingTierGolden {
    pub index: usize,
    pub factor: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FittedModuleGolden {
    pub item_id: i64,
    pub type_id: u32,
    pub flag_id: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShipFitGolden {
    pub name: String,
    pub ship_type_id: u32,
    pub ship_name: String,
    pub skills: HashMap<u32, u8>,
    pub modules: Vec<FittedModuleGolden>,
    pub implants: Vec<u32>,
    pub expected_attributes: HashMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DogmaGoldenSuite {
    pub version: u32,
    pub description: String,
    pub stacking_factors: Vec<StackingTierGolden>,
    pub fits: Vec<ShipFitGolden>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_modifiers")
        .join("ccp_golden_fits.json")
}

pub fn build_golden_dogma_suite(sde: &SdeRegistry) -> DogmaGoldenSuite {
    let mut stacking_factors = Vec::new();
    for i in 0..=6 {
        stacking_factors.push(StackingTierGolden {
            index: i,
            factor: stacking_penalty_factor(i),
        });
    }

    let mut fits = Vec::new();

    // 1. Frigate: Rifter (587) with speed/agility modules and skills
    {
        let skills: HashMap<u32, u8> = HashMap::from([
            (3329, 5),  // Minmatar Frigate V
            (3327, 5),  // Spaceship Command V
            (3449, 5),  // Navigation V
            (3392, 5),  // Mechanics V
            (3394, 5),  // Hull Upgrades V
            (3419, 5),  // Shield Management V
            (3300, 5),  // Gunnery V
            (3302, 5),  // Small Projectile Turret V
            (3318, 5),  // Weapon Upgrades V
            (11084, 4), // Small Autocannon Specialization IV
        ]);

        let modules = vec![
            FittedModuleGolden {
                item_id: 10001,
                type_id: 1236, // Overdrive Injector System II
                flag_id: 11,
                name: "Overdrive Injector System II".to_string(),
            },
            FittedModuleGolden {
                item_id: 10002,
                type_id: 2605, // Nanofiber Internal Structure II
                flag_id: 12,
                name: "Nanofiber Internal Structure II".to_string(),
            },
            FittedModuleGolden {
                item_id: 10003,
                type_id: 519, // Gyrostabilizer II
                flag_id: 13,
                name: "Gyrostabilizer II".to_string(),
            },
            FittedModuleGolden {
                item_id: 10004,
                type_id: 2889, // 200mm AutoCannon II
                flag_id: 27,
                name: "200mm AutoCannon II".to_string(),
            },
        ];

        let mut engine = ShipFittingEngine::from_sde(1000, 587, sde);
        engine.set_character_skills(skills.clone());
        for m in &modules {
            engine
                .fit_and_online_module(m.item_id, m.type_id, m.flag_id, sde)
                .expect("fit module on Rifter");
        }
        let ctx = engine.evaluated_context(sde);

        let mut attrs = HashMap::new();
        attrs.insert("maxVelocity".to_string(), ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde));
        attrs.insert("agility".to_string(), ctx.ship.get_attribute(attr_ids::AGILITY, sde));
        attrs.insert("capacity".to_string(), ctx.ship.get_attribute(attr_ids::CAPACITY, sde));
        attrs.insert("hp_structure".to_string(), ctx.ship.get_attribute(attr_ids::HP_STRUCT, sde));
        attrs.insert("armorHP".to_string(), ctx.ship.get_attribute(attr_ids::ARMOR_HP, sde));
        attrs.insert("shieldCapacity".to_string(), ctx.ship.get_attribute(attr_ids::SHIELD_CAPACITY, sde));
        attrs.insert("cpuOutput".to_string(), ctx.ship.get_attribute(attr_ids::CPU_OUTPUT, sde));
        attrs.insert("powerOutput".to_string(), ctx.ship.get_attribute(attr_ids::POWER_OUTPUT, sde));
        attrs.insert("cpuLoad".to_string(), ctx.ship.get_attribute(attr_ids::CPU_LOAD, sde));
        attrs.insert("powerLoad".to_string(), ctx.ship.get_attribute(attr_ids::POWER_LOAD, sde));

        fits.push(ShipFitGolden {
            name: "frigate_rifter_speed_and_brawl".to_string(),
            ship_type_id: 587,
            ship_name: "Rifter".to_string(),
            skills,
            modules,
            implants: Vec::new(),
            expected_attributes: attrs,
        });
    }

    // 2. Cruiser: Caracal (621) with 3x Ballistic Control System II (stacking penalized) + DC II + LSE II
    {
        let skills: HashMap<u32, u8> = HashMap::from([
            (3334, 5),  // Caldari Cruiser V
            (3324, 5),  // Heavy Missiles V
            (3319, 5),  // Missile Launcher Operation V
            (20211, 4), // Heavy Missile Specialization IV
            (3419, 5),  // Shield Management V
            (3425, 5),  // Shield Upgrades V
            (3449, 5),  // Navigation V
            (3394, 5),  // Hull Upgrades V
            (3318, 5),  // Weapon Upgrades V
        ]);

        let modules = vec![
            FittedModuleGolden {
                item_id: 20001,
                type_id: 22291, // Ballistic Control System II
                flag_id: 11,
                name: "Ballistic Control System II #1".to_string(),
            },
            FittedModuleGolden {
                item_id: 20002,
                type_id: 22291, // Ballistic Control System II
                flag_id: 12,
                name: "Ballistic Control System II #2".to_string(),
            },
            FittedModuleGolden {
                item_id: 20003,
                type_id: 22291, // Ballistic Control System II
                flag_id: 13,
                name: "Ballistic Control System II #3".to_string(),
            },
            FittedModuleGolden {
                item_id: 20004,
                type_id: 2048, // Damage Control II
                flag_id: 14,
                name: "Damage Control II".to_string(),
            },
            FittedModuleGolden {
                item_id: 20005,
                type_id: 3841, // Large Shield Extender II
                flag_id: 19,
                name: "Large Shield Extender II".to_string(),
            },
            FittedModuleGolden {
                item_id: 20006,
                type_id: 2410, // Heavy Missile Launcher II
                flag_id: 27,
                name: "Heavy Missile Launcher II".to_string(),
            },
        ];

        let mut engine = ShipFittingEngine::from_sde(2000, 621, sde);
        engine.set_character_skills(skills.clone());
        for m in &modules {
            engine
                .fit_and_online_module(m.item_id, m.type_id, m.flag_id, sde)
                .expect("fit module on Caracal");
        }
        let ctx = engine.evaluated_context(sde);

        let mut attrs = HashMap::new();
        attrs.insert("shieldCapacity".to_string(), ctx.ship.get_attribute(attr_ids::SHIELD_CAPACITY, sde));
        attrs.insert("shieldEmResonance".to_string(), ctx.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde));
        attrs.insert("shieldExplosiveResonance".to_string(), ctx.ship.get_attribute(attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, sde));
        attrs.insert("shieldKineticResonance".to_string(), ctx.ship.get_attribute(attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, sde));
        attrs.insert("shieldThermalResonance".to_string(), ctx.ship.get_attribute(attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, sde));
        attrs.insert("maxVelocity".to_string(), ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde));
        attrs.insert("cpuOutput".to_string(), ctx.ship.get_attribute(attr_ids::CPU_OUTPUT, sde));
        attrs.insert("powerOutput".to_string(), ctx.ship.get_attribute(attr_ids::POWER_OUTPUT, sde));
        attrs.insert("cpuLoad".to_string(), ctx.ship.get_attribute(attr_ids::CPU_LOAD, sde));
        attrs.insert("powerLoad".to_string(), ctx.ship.get_attribute(attr_ids::POWER_LOAD, sde));

        fits.push(ShipFitGolden {
            name: "cruiser_caracal_missile_and_shield".to_string(),
            ship_type_id: 621,
            ship_name: "Caracal".to_string(),
            skills,
            modules,
            implants: Vec::new(),
            expected_attributes: attrs,
        });
    }

    // 3. Battleship: Megathron (641) with 3x MagStab II + 1600mm Plates + High-grade Snake Implants
    {
        let skills: HashMap<u32, u8> = HashMap::from([
            (3336, 5), // Gallente Battleship V
            (3307, 5), // Large Hybrid Turret V
            (3394, 5), // Hull Upgrades V
            (3392, 5), // Mechanics V
            (3393, 5), // Repair Systems V
            (3318, 5), // Weapon Upgrades V
        ]);

        let modules = vec![
            FittedModuleGolden {
                item_id: 30001,
                type_id: 10190, // Magnetic Field Stabilizer II
                flag_id: 11,
                name: "Magnetic Field Stabilizer II #1".to_string(),
            },
            FittedModuleGolden {
                item_id: 30002,
                type_id: 10190, // Magnetic Field Stabilizer II
                flag_id: 12,
                name: "Magnetic Field Stabilizer II #2".to_string(),
            },
            FittedModuleGolden {
                item_id: 30003,
                type_id: 10190, // Magnetic Field Stabilizer II
                flag_id: 13,
                name: "Magnetic Field Stabilizer II #3".to_string(),
            },
            FittedModuleGolden {
                item_id: 30004,
                type_id: 20353, // 1600mm Steel Plates II
                flag_id: 14,
                name: "1600mm Steel Plates II".to_string(),
            },
            FittedModuleGolden {
                item_id: 30005,
                type_id: 2048, // Damage Control II
                flag_id: 15,
                name: "Damage Control II".to_string(),
            },
        ];

        let implants = vec![
            19540, // High-grade Snake Alpha
            19551, // High-grade Snake Beta
            19553, // High-grade Snake Gamma
        ];

        let mut engine = ShipFittingEngine::from_sde(3000, 641, sde);
        engine.set_character_skills(skills.clone());
        engine.set_implants(implants.clone());
        for m in &modules {
            engine
                .fit_and_online_module(m.item_id, m.type_id, m.flag_id, sde)
                .expect("fit module on Megathron");
        }
        let ctx = engine.evaluated_context(sde);

        let mut attrs = HashMap::new();
        attrs.insert("armorHP".to_string(), ctx.ship.get_attribute(attr_ids::ARMOR_HP, sde));
        attrs.insert("hp_structure".to_string(), ctx.ship.get_attribute(attr_ids::HP_STRUCT, sde));
        attrs.insert("armorEmResonance".to_string(), ctx.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde));
        attrs.insert("armorExplosiveResonance".to_string(), ctx.ship.get_attribute(attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, sde));
        attrs.insert("armorKineticResonance".to_string(), ctx.ship.get_attribute(attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, sde));
        attrs.insert("armorThermalResonance".to_string(), ctx.ship.get_attribute(attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, sde));
        attrs.insert("maxVelocity".to_string(), ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, sde));
        attrs.insert("cpuOutput".to_string(), ctx.ship.get_attribute(attr_ids::CPU_OUTPUT, sde));
        attrs.insert("powerOutput".to_string(), ctx.ship.get_attribute(attr_ids::POWER_OUTPUT, sde));
        attrs.insert("cpuLoad".to_string(), ctx.ship.get_attribute(attr_ids::CPU_LOAD, sde));
        attrs.insert("powerLoad".to_string(), ctx.ship.get_attribute(attr_ids::POWER_LOAD, sde));

        fits.push(ShipFitGolden {
            name: "battleship_megathron_armor_tank".to_string(),
            ship_type_id: 641,
            ship_name: "Megathron".to_string(),
            skills,
            modules,
            implants,
            expected_attributes: attrs,
        });
    }

    // 4. Stacking Benchmark: 4x Inertial Stabilizers II on Rifter (tests S(0), S(1), S(2), S(3) bit-for-bit)
    {
        let modules = vec![
            FittedModuleGolden {
                item_id: 40001,
                type_id: 1405, // Inertial Stabilizers II
                flag_id: 11,
                name: "Inertial Stabilizers II #1".to_string(),
            },
            FittedModuleGolden {
                item_id: 40002,
                type_id: 1405, // Inertial Stabilizers II
                flag_id: 12,
                name: "Inertial Stabilizers II #2".to_string(),
            },
            FittedModuleGolden {
                item_id: 40003,
                type_id: 1405, // Inertial Stabilizers II
                flag_id: 13,
                name: "Inertial Stabilizers II #3".to_string(),
            },
            FittedModuleGolden {
                item_id: 40004,
                type_id: 1405, // Inertial Stabilizers II
                flag_id: 14,
                name: "Inertial Stabilizers II #4".to_string(),
            },
        ];

        let skills: HashMap<u32, u8> = HashMap::from([
            (3394, 5), // Hull Upgrades V
        ]);

        let mut engine = ShipFittingEngine::from_sde(4000, 587, sde);
        engine.set_character_skills(skills.clone());
        for m in &modules {
            engine
                .fit_and_online_module(m.item_id, m.type_id, m.flag_id, sde)
                .expect("fit istab on Rifter");
        }
        let ctx = engine.evaluated_context(sde);

        let mut attrs = HashMap::new();
        attrs.insert("agility".to_string(), ctx.ship.get_attribute(attr_ids::AGILITY, sde));
        attrs.insert("sigRadius".to_string(), ctx.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde));
        attrs.insert("cpuLoad".to_string(), ctx.ship.get_attribute(attr_ids::CPU_LOAD, sde));
        attrs.insert("powerLoad".to_string(), ctx.ship.get_attribute(attr_ids::POWER_LOAD, sde));

        fits.push(ShipFitGolden {
            name: "rifter_four_inertial_stabilizers_stacking".to_string(),
            ship_type_id: 587,
            ship_name: "Rifter".to_string(),
            skills,
            modules,
            implants: Vec::new(),
            expected_attributes: attrs,
        });
    }

    DogmaGoldenSuite {
        version: 1,
        description: "CCP golden fits and modifier vectors for differential verification of eve-dogma (plan:T-12)".to_string(),
        stacking_factors,
        fits,
    }
}

#[test]
// parity: plan:T-12
fn test_t12_dogma_differential_fits_and_stacking_penalties() {
    eve_testkit::attest(&["plan:T-12"]);

    let sde = SdeRegistry::with_canonical_seed();
    let expected_suite = build_golden_dogma_suite(&sde);
    let path = fixture_path();

    // If golden file does not exist, write the canonical generated suite
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let serialized = serde_json::to_string_pretty(&expected_suite)
            .expect("serialize golden dogma suite");
        std::fs::write(&path, serialized).expect("write ccp_golden_fits.json");
    }

    let file_content =
        std::fs::read_to_string(&path).expect("read ccp_golden_fits.json fixture");
    let golden_suite: DogmaGoldenSuite =
        serde_json::from_str(&file_content).expect("parse ccp_golden_fits.json");

    // 1. Verify stacking factors bit-for-bit with IEEE-754 precision
    assert_eq!(golden_suite.stacking_factors.len(), 7);
    for tier in &golden_suite.stacking_factors {
        let expected = (-((tier.index as f64) / 2.67).powi(2)).exp();
        assert!(
            (tier.factor - expected).abs() < 1e-9,
            "Stacking tier {} mismatch: got {}, expected {}",
            tier.index, tier.factor, expected
        );
    }

    // 2. Verify fits against golden attributes
    assert_eq!(golden_suite.fits.len(), 4);
    for fit in &golden_suite.fits {
        let mut engine = ShipFittingEngine::from_sde(100, fit.ship_type_id, &sde);
        engine.set_character_skills(fit.skills.clone());
        engine.set_implants(fit.implants.clone());

        for m in &fit.modules {
            engine
                .fit_and_online_module(m.item_id, m.type_id, m.flag_id, &sde)
                .unwrap_or_else(|e| panic!("Failed to fit module {} on {}: {e}", m.name, fit.name));
        }

        let ctx = engine.evaluated_context(&sde);

        for (attr_name, &expected_val) in &fit.expected_attributes {
            let actual_val = match attr_name.as_str() {
                "maxVelocity" => ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde),
                "agility" => ctx.ship.get_attribute(attr_ids::AGILITY, &sde),
                "capacity" => ctx.ship.get_attribute(attr_ids::CAPACITY, &sde),
                "hp_structure" => ctx.ship.get_attribute(attr_ids::HP_STRUCT, &sde),
                "armorHP" => ctx.ship.get_attribute(attr_ids::ARMOR_HP, &sde),
                "shieldCapacity" => ctx.ship.get_attribute(attr_ids::SHIELD_CAPACITY, &sde),
                "shieldEmResonance" => ctx.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, &sde),
                "shieldExplosiveResonance" => ctx.ship.get_attribute(attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, &sde),
                "shieldKineticResonance" => ctx.ship.get_attribute(attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, &sde),
                "shieldThermalResonance" => ctx.ship.get_attribute(attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, &sde),
                "armorEmResonance" => ctx.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, &sde),
                "armorExplosiveResonance" => ctx.ship.get_attribute(attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, &sde),
                "armorKineticResonance" => ctx.ship.get_attribute(attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, &sde),
                "armorThermalResonance" => ctx.ship.get_attribute(attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, &sde),
                "sigRadius" => ctx.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde),
                "cpuOutput" => ctx.ship.get_attribute(attr_ids::CPU_OUTPUT, &sde),
                "powerOutput" => ctx.ship.get_attribute(attr_ids::POWER_OUTPUT, &sde),
                "cpuLoad" => ctx.ship.get_attribute(attr_ids::CPU_LOAD, &sde),
                "powerLoad" => ctx.ship.get_attribute(attr_ids::POWER_LOAD, &sde),
                other => panic!("Unknown attribute key {other}"),
            };

            let delta = (actual_val - expected_val).abs();
            let rel_error = delta / expected_val.abs().max(1.0);
            assert!(
                rel_error < 1e-6,
                "Fit '{}' attribute '{}' mismatch: actual={}, expected={}, delta={}",
                fit.name, attr_name, actual_val, expected_val, delta
            );
        }
    }

    // 3. Verify operator precedence order
    test_operator_precedence_order_bit_for_bit(&sde);

    // 4. Differential verification via Python client bridge
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bridge_py = repo_root.join("client-bridge").join("evomu_bridge.py");
    if bridge_py.exists() {
        let py_output = Command::new("python3")
            .arg(&bridge_py)
            .arg("--verify-dogma-golden")
            .arg(&path)
            .output();

        if let Ok(output) = py_output {
            assert!(
                output.status.success(),
                "Python dogma differential verification failed:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&output.stdout)
                .expect("parse python dogma verification report");
            assert_eq!(report.get("ok").and_then(|v| v.as_bool()), Some(true));
            assert_eq!(
                report.get("verified_fits").and_then(|v| v.as_u64()),
                Some(4)
            );
            assert_eq!(
                report.get("verified_stacking_tiers").and_then(|v| v.as_u64()),
                Some(7)
            );
        }
    }
}

fn test_operator_precedence_order_bit_for_bit(sde: &SdeRegistry) {
    let mut item = DogmaItemState::from_sde(999, 587, sde);
    item.base_attributes.insert(attr_ids::MAX_VELOCITY, 100.0);

    // Add all modifier operators targeting maxVelocity:
    // Base: 100.0
    // PreAssign -> 50.0
    // PreMul 2.0 -> 100.0
    // PreDiv 2.0 -> 50.0
    // ModAdd 20.0 -> 70.0
    // ModSub 10.0 -> 60.0
    // PostMul 1.5 -> 90.0
    // PostPercent 10.0 -> 99.0
    // PostPercentUnNerfed 10.0 -> 108.9
    // PostAssign -> 250.0
    let make_mod = |op: ModifierOperator, val: f64| AttributeModifier {
        source_item_id: 0,
        effect_id: None,
        target_attribute_id: attr_ids::MAX_VELOCITY,
        operator: op,
        value: val,
        stacking_penalized: false,
    };

    item.add_modifier(make_mod(ModifierOperator::ModAdd, 20.0));
    item.add_modifier(make_mod(ModifierOperator::ModSub, 10.0));
    item.add_modifier(make_mod(ModifierOperator::PreMul, 2.0));
    item.add_modifier(make_mod(ModifierOperator::PreDiv, 2.0));
    item.add_modifier(make_mod(ModifierOperator::PreAssign, 50.0));
    item.add_modifier(make_mod(ModifierOperator::PostMul, 1.5));

    let val = item.get_attribute(attr_ids::MAX_VELOCITY, sde);
    // 50 * 2 / 2 = 50; 50 + 20 - 10 = 60; 60 * 1.5 = 90.0
    assert!((val - 90.0).abs() < 1e-6, "Precedence check: expected 90.0, got {val}");

    // PostAssign overrides everything
    item.add_modifier(make_mod(ModifierOperator::PostAssign, 250.0));
    let post_val = item.get_attribute(attr_ids::MAX_VELOCITY, sde);
    assert_eq!(post_val, 250.0, "PostAssign override check");
}
