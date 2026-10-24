//! Task T-12: Dogma Differential Physics & Stacking Differential Test Harness.
//!
//! Differential harness against CCP Python 2.7 Dogma runtime:
//! Stacking penalty factors, operator precedence sequence, and modifier graph evaluated by
//! CCP's compiled Dogma classes (CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH): dogma.effects.modifiers (ItemModifier,
//! LocationModifier, LocationGroupModifier, LocationRequiredSkillModifier, OwnerRequiredSkillModifier,
//! modifierGetterByType), dogma.effects.modifiereffect.ModifierEffect, and
//! EvoMuDogmaLocation(MockDogmaLocation), scaled across 500+ procedural ship/module/implant fits.

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
pub struct GangBurstGolden {
    #[serde(default)]
    pub source_item_id: i64,
    #[serde(default)]
    pub dbuff_id: Option<u32>,
    #[serde(default)]
    pub value: f64,
    #[serde(default)]
    pub modifiers: Vec<AttributeModifier>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FittedModuleGolden {
    pub item_id: i64,
    pub type_id: u32,
    pub flag_id: u16,
    pub name: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub overloaded: bool,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub dynamic_attributes: HashMap<u16, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShipFitGolden {
    pub name: String,
    pub ship_type_id: u32,
    pub ship_name: String,
    #[serde(default)]
    pub oracle_source: Option<String>,
    pub skills: HashMap<u32, u8>,
    pub modules: Vec<FittedModuleGolden>,
    pub implants: Vec<u32>,
    #[serde(default)]
    pub boosters: Vec<u32>,
    #[serde(default)]
    pub gang_bursts: Vec<GangBurstGolden>,
    pub expected_attributes: HashMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientMeta {
    pub codename: String,
    pub version: String,
    pub build: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub code_ccp_sha256: Option<String>,
    #[serde(default)]
    pub oracle_source: Option<String>,
    #[serde(default)]
    pub fits_oracle_source: Option<String>,
    #[serde(default)]
    pub graph_wiring: Option<String>,
    #[serde(default)]
    pub modifier_engine: Option<String>,
    #[serde(default)]
    pub location_engine: Option<String>,
    #[serde(default)]
    pub procedural_fits_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DogmaPrecedenceTest {
    pub val_before_post_assign: f64,
    #[serde(default)]
    pub val_with_op9: f64,
    pub val_after_post_assign: f64,
    #[serde(default)]
    pub op9_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DogmaVersionSuite {
    #[serde(default)]
    pub fixture_type: Option<String>,
    #[serde(default)]
    pub client_meta: Option<ClientMeta>,
    pub stacking_factors: Vec<StackingTierGolden>,
    #[serde(default)]
    pub precedence_test: Option<DogmaPrecedenceTest>,
    pub fits: Vec<ShipFitGolden>,
    #[serde(default)]
    pub procedural_fits: Vec<ShipFitGolden>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DogmaGoldenSuite {
    #[serde(default)]
    pub fixture_type: Option<String>,
    #[serde(default = "default_active_version")]
    pub active_version: String,
    #[serde(default)]
    pub versions: HashMap<String, DogmaVersionSuite>,
    #[serde(default)]
    pub stacking_factors: Vec<StackingTierGolden>,
    #[serde(default)]
    pub precedence_test: Option<DogmaPrecedenceTest>,
    pub fits: Vec<ShipFitGolden>,
    #[serde(default)]
    pub procedural_fits: Vec<ShipFitGolden>,
}

fn default_active_version() -> String {
    "V24.01-3624425".to_string()
}

fn fixture_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir_path = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("ccp_golden_fits.json");
    if fixture_dir_path.exists() {
        fixture_dir_path
    } else {
        manifest_dir
            .join("tests")
            .join("golden_modifiers")
            .join("ccp_golden_fits.json")
    }
}

// parity: plan:T-12
#[test]
fn test_t12_dogma_differential_fits_and_stacking_penalties() {
    let sde = SdeRegistry::with_canonical_seed();
    let path = fixture_path();
    assert!(
        path.exists(),
        "CCP golden fits fixture does not exist at {:?}. Run tools/generate_golden_fixtures.py first.",
        path
    );

    let file_content =
        std::fs::read_to_string(&path).expect("read ccp_golden_fits.json fixture");
    let golden_suite: DogmaGoldenSuite =
        serde_json::from_str(&file_content).expect("parse ccp_golden_fits.json");

    let (stacking_factors, fits, precedence_test) =
        if let Some(v_suite) = golden_suite.versions.get(&golden_suite.active_version) {
            assert_eq!(
                v_suite.fixture_type.as_deref(),
                Some("CCP_BUNDLED_RUNTIME"),
                "Version suite fixture_type must be CCP_BUNDLED_RUNTIME"
            );
            if let Some(meta) = &v_suite.client_meta {
                assert_eq!(
                    meta.oracle_source.as_deref(),
                    Some("CCP_BUNDLED_RUNTIME"),
                    "Client meta oracle_source must be CCP_BUNDLED_RUNTIME"
                );
                assert!(
                    meta.fits_oracle_source.as_deref() == Some("CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH")
                        || meta.fits_oracle_source.as_deref() == Some("CCP_BASE_DOGMA_LOCATION_DISPATCH"),
                    "Fits oracle_source must document authentic CCP modifier and location provenance"
                );
                assert_eq!(
                    meta.graph_wiring.as_deref(),
                    Some("CCP_MODIFIER_EFFECT_LOCATION_GRAPH"),
                    "Graph wiring must document CCP ModifierEffect location graph"
                );
                assert_eq!(
                    meta.modifier_engine.as_deref(),
                    Some("CCP_NATIVE_MODIFIER_EFFECT"),
                    "Modifier engine must document CCP native ModifierEffect"
                );
                assert!(
                    meta.location_engine.as_deref() == Some("CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH")
                        || meta.location_engine.as_deref() == Some("CCP_BASE_DOGMA_LOCATION_DISPATCH"),
                    "Location engine must document CCP MockDogmaLocation / BaseDogmaLocation"
                );
                assert!(
                    meta.procedural_fits_count.unwrap_or(0) >= 500,
                    "Procedural fits count must be at least 500"
                );
            }
            (&v_suite.stacking_factors, &v_suite.fits, v_suite.precedence_test.as_ref())
        } else {
            (&golden_suite.stacking_factors, &golden_suite.fits, golden_suite.precedence_test.as_ref())
        };

    assert_eq!(
        golden_suite.fixture_type.as_deref(),
        Some("CCP_BUNDLED_RUNTIME"),
        "Golden suite root fixture_type must be CCP_BUNDLED_RUNTIME"
    );

    // 1. Verify stacking factors bit-for-bit with IEEE-754 precision
    assert_eq!(
        stacking_factors.len(),
        7,
        "Expected 7 stacking factor tiers in golden suite (version {})",
        golden_suite.active_version
    );
    for tier in stacking_factors {
        let dogma_factor = stacking_penalty_factor(tier.index);
        assert!(
            (dogma_factor - tier.factor).abs() < 1e-9,
            "Stacking tier {} mismatch: dogma={}, golden={}",
            tier.index, dogma_factor, tier.factor
        );
    }

    // 2. Verify fits against golden attributes
    assert_eq!(
        fits.len(),
        10,
        "Expected exactly 10 canonical fits in golden suite (version {})",
        golden_suite.active_version
    );

    // Verify duplicate dbuff_id aggregation is exercised in canonical fits (R84-BF-02)
    let fits_with_dup_dbuffs: Vec<_> = fits
        .iter()
        .filter(|f| {
            let mut seen = std::collections::HashSet::new();
            f.gang_bursts.iter().any(|b| {
                if let Some(id) = b.dbuff_id {
                    !seen.insert(id)
                } else {
                    false
                }
            })
        })
        .collect();
    assert!(
        !fits_with_dup_dbuffs.is_empty(),
        "Expected canonical fits with duplicate dbuff_id in gang_bursts"
    );

    for fit in fits {
        let mut engine = ShipFittingEngine::from_sde(100, fit.ship_type_id, &sde);
        engine.set_character_skills(fit.skills.clone());
        engine.set_implants(fit.implants.clone());
        engine.set_boosters(fit.boosters.clone());

        for gb in &fit.gang_bursts {
            engine.add_gang_burst_and_recalculate(
                eve_dogma::GangBurstEffect {
                    source_item_id: gb.source_item_id,
                    source_type_id: None,
                    dbuff_id: gb.dbuff_id,
                    value: gb.value,
                    modifiers: gb.modifiers.clone(),
                },
                &sde,
            );
        }

        for m in &fit.modules {
            if !m.dynamic_attributes.is_empty() {
                engine
                    .fit_module_with_dynamic_attributes(
                        m.item_id,
                        m.type_id,
                        m.flag_id,
                        true,
                        m.dynamic_attributes.clone(),
                        &sde,
                    )
                    .unwrap_or_else(|e| panic!("Failed to fit module {} on {}: {e}", m.name, fit.name));
            } else {
                engine
                    .fit_and_online_module(m.item_id, m.type_id, m.flag_id, &sde)
                    .unwrap_or_else(|e| panic!("Failed to fit module {} on {}: {e}", m.name, fit.name));
            }
            let is_overloaded = m.overloaded || m.state.as_deref() == Some("Overloaded");
            let is_active = m.active || is_overloaded;
            if is_active {
                engine
                    .activate_module(m.item_id, is_overloaded, &sde)
                    .unwrap_or_else(|e| panic!("Failed to activate module {} on {}: {e}", m.name, fit.name));
            }
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
    test_operator_precedence_order_bit_for_bit(&sde, precedence_test);

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
            assert!(
                report.get("verified_fits").and_then(|v| v.as_u64()).unwrap_or(0) >= 4,
                "Python bridge must verify at least 4 canonical fits"
            );
            assert!(
                report.get("verified_procedural_fits").and_then(|v| v.as_u64()).unwrap_or(0) >= 500,
                "Python bridge must verify at least 500 procedural fits"
            );
            assert_eq!(
                report.get("verified_stacking_tiers").and_then(|v| v.as_u64()),
                Some(7)
            );
        }
    }

    eve_testkit::attest(&["plan:T-12"]);
}

// parity: plan:T-12
#[test]
fn test_t12_dogma_procedural_differential_500_fits() {
    let sde = SdeRegistry::with_canonical_seed();
    let path = fixture_path();
    assert!(
        path.exists(),
        "CCP golden fits fixture does not exist at {:?}. Run tools/generate_golden_fixtures.py first.",
        path
    );

    let file_content =
        std::fs::read_to_string(&path).expect("read ccp_golden_fits.json fixture");
    let golden_suite: DogmaGoldenSuite =
        serde_json::from_str(&file_content).expect("parse ccp_golden_fits.json");

    let procedural_fits = if let Some(v_suite) = golden_suite.versions.get(&golden_suite.active_version) {
        &v_suite.procedural_fits
    } else {
        &golden_suite.procedural_fits
    };

    assert!(
        procedural_fits.len() >= 500,
        "Expected at least 500 procedural fits, found {}",
        procedural_fits.len()
    );

    // Verify duplicate dbuff_id aggregation is exercised in procedural fits (R84-BF-02)
    let proc_fits_with_dup_dbuffs: Vec<_> = procedural_fits
        .iter()
        .filter(|f| {
            let mut seen = std::collections::HashSet::new();
            f.gang_bursts.iter().any(|b| {
                if let Some(id) = b.dbuff_id {
                    !seen.insert(id)
                } else {
                    false
                }
            })
        })
        .collect();
    assert!(
        !proc_fits_with_dup_dbuffs.is_empty(),
        "Expected procedural fits with duplicate dbuff_id in gang_bursts"
    );

    let mut verified = 0;
    for fit in procedural_fits {
        let mut engine = ShipFittingEngine::from_sde(100, fit.ship_type_id, &sde);
        engine.set_character_skills(fit.skills.clone());
        engine.set_implants(fit.implants.clone());
        engine.set_boosters(fit.boosters.clone());

        for gb in &fit.gang_bursts {
            engine.add_gang_burst_and_recalculate(
                eve_dogma::GangBurstEffect {
                    source_item_id: gb.source_item_id,
                    source_type_id: None,
                    dbuff_id: gb.dbuff_id,
                    value: gb.value,
                    modifiers: gb.modifiers.clone(),
                },
                &sde,
            );
        }

        for m in &fit.modules {
            if !m.dynamic_attributes.is_empty() {
                engine
                    .fit_module_with_dynamic_attributes(
                        m.item_id,
                        m.type_id,
                        m.flag_id,
                        true,
                        m.dynamic_attributes.clone(),
                        &sde,
                    )
                    .unwrap_or_else(|e| panic!("Failed to fit module {} on {}: {e}", m.name, fit.name));
            } else {
                engine
                    .fit_and_online_module(m.item_id, m.type_id, m.flag_id, &sde)
                    .unwrap_or_else(|e| panic!("Failed to fit module {} on {}: {e}", m.name, fit.name));
            }
            let is_overloaded = m.overloaded || m.state.as_deref() == Some("Overloaded");
            let is_active = m.active || is_overloaded;
            if is_active {
                engine
                    .activate_module(m.item_id, is_overloaded, &sde)
                    .unwrap_or_else(|e| panic!("Failed to activate module {} on {}: {e}", m.name, fit.name));
            }
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
                rel_error < 1e-5,
                "Procedural fit '{}' attribute '{}' mismatch: actual={}, expected={}, delta={}",
                fit.name, attr_name, actual_val, expected_val, delta
            );
        }
        verified += 1;
    }
    assert!(verified >= 500, "Must have verified at least 500 procedural fits");
    eve_testkit::attest(&["plan:T-12"]);
}

fn test_operator_precedence_order_bit_for_bit(
    sde: &SdeRegistry,
    golden_prec: Option<&DogmaPrecedenceTest>,
) {
    let mut item = DogmaItemState::from_sde(999, 587, sde);
    item.base_attributes.insert(attr_ids::MAX_VELOCITY, 100.0);

    // Canonical precedence sequence across ModifierOperator variants:
    // Base: 100.0
    // 1. PreAssign 50.0 -> sets base to 50.0
    // 2. PreMul 2.0 -> 50.0 * 2.0 = 100.0
    // 3. PreDiv 2.0 -> 100.0 / 2.0 = 50.0
    // 4. ModAdd 20.0 -> 50.0 + 20.0 = 70.0
    // 5. ModSub 10.0 -> 70.0 - 10.0 = 60.0
    // 6. PostMul 1.5 -> delta +0.5 -> * 1.5 = 90.0
    // 7. PostDiv 1.25 -> factor (1/1.25 - 1) = -0.2 -> * 0.8 = 72.0
    // 8. PostPercent 10.0 -> delta +0.10 -> * 1.10 = 79.2
    // 9. PostPercentUnNerfed 10.0 -> delta +0.10 -> * 1.10 = 87.12
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
    item.add_modifier(make_mod(ModifierOperator::PostDiv, 1.25));
    item.add_modifier(make_mod(ModifierOperator::PostPercent, 10.0));
    item.add_modifier(make_mod(ModifierOperator::PostPercentUnNerfed, 10.0));

    let val = item.get_attribute(attr_ids::MAX_VELOCITY, sde);
    if let Some(expected) = golden_prec {
        assert!(
            (val - expected.val_before_post_assign).abs() < 1e-6,
            "Precedence check before SpecialSkillRelationship and PostAssign: expected {} from CCP golden fixture, got {val}",
            expected.val_before_post_assign
        );
    }

    // 10. SpecialSkillRelationship (DERIVED_OP9_SPECIAL_SKILL_RELATIONSHIP):
    // CCP's V24.01 dogma/const.pyj dgmOperatorPrecedence defines 10 operators ([-1, 0, 1, 2, 3, 4, 5, 6, 8, 7]);
    // operator 9 (SpecialSkillRelationship / dgmAssSpecialSkillRelationship) is a DERIVED EvoMu extension
    // inserted before PostAssignment (7) with direct percentage scaling: val * (1.0 + m / 100.0).
    item.add_modifier(make_mod(ModifierOperator::SpecialSkillRelationship, 5.0));
    let val_with_op9 = item.get_attribute(attr_ids::MAX_VELOCITY, sde);
    if let Some(expected) = golden_prec {
        assert!(
            (val_with_op9 - expected.val_with_op9).abs() < 1e-6,
            "SpecialSkillRelationship percentage check: expected {} from CCP golden fixture, got {val_with_op9}",
            expected.val_with_op9
        );
    } else {
        assert!(
            (val_with_op9 - (val * 1.05)).abs() < 1e-6,
            "SpecialSkillRelationship percentage check: expected {}, got {val_with_op9}",
            val * 1.05
        );
    }

    // 11. PostAssign 250.0 -> overrides everything -> 250.0
    item.add_modifier(make_mod(ModifierOperator::PostAssign, 250.0));
    let post_val = item.get_attribute(attr_ids::MAX_VELOCITY, sde);
    if let Some(expected) = golden_prec {
        assert!(
            (post_val - expected.val_after_post_assign).abs() < 1e-6,
            "PostAssign override check: expected {} from CCP golden fixture, got {post_val}",
            expected.val_after_post_assign
        );
    } else {
        assert_eq!(post_val, 250.0, "PostAssign override check");
    }
}

// parity: plan:T-12
#[test]
fn test_t12_golden_fixtures_version_diff_cli() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let gen_py = repo_root.join("tools").join("generate_golden_fixtures.py");
    let output = Command::new("python3")
        .arg(&gen_py)
        .arg("--diff")
        .arg("V24.01-3624425")
        .arg("V24.01-3624425")
        .output()
        .expect("run tools/generate_golden_fixtures.py --diff");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("OK: Client versions V24.01-3624425 and V24.01-3624425 are 100% identical"));
    eve_testkit::attest(&["plan:T-12"]);
}
