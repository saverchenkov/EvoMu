//! `crates/eve-server/tests/dogma_modifier_engine.rs`
//!
//! Track 1: Phase 4 Dogma Engine (`DG-1`)
//! Comprehensive integration and specification tests for:
//! - All domain modifier functions over real V24.01 SDE `modifierInfo`:
//!   `ItemModifier`, `LocationGroupModifier`, `LocationRequiredSkillModifier`,
//!   `OwnerRequiredSkillModifier`, `ItemModifierWithFalloff`, `LocationModifier`.
//! - Stacking penalty curve $S(n) = e^{-(n-1)^2 / 7.1289}$ and exemption rules.
//! - Module family effects:
//!   - Weapon upgrades (Gyrostabilizer, Magnetic Field Stabilizer, Heat Sink, Ballistic Control System, Tracking Enhancer)
//!   - Propulsion modules (Afterburner, Microwarpdrive, MWD sig bloom & scrambler suppression)
//!   - Energy neutralizers and nosferatu (range falloff attenuation & vampire cap % rule)
//!   - Electronic warfare (ECM jam chance, sensor dampeners, target painters, tracking disruptors, stasis webifiers, warp disruptors/scramblers)
//!   - Command bursts & dbuff aggregation
//!   - Combat booster drugs & neurotoxin skills
//!   - Character implants & stacking exemption
//!
//! // parity: plan:DG-1
//! // parity: feat:dogma.modifier-engine

use eve_dogma::{
    aggregate_dbuff_values, compute_ecm_jam_chance, eval_command_burst, eval_energy_neutralizer,
    eval_energy_nosferatu, eval_propulsion_module, eval_sensor_dampener_stacking,
    eval_stasis_webifier_stacking, eval_target_painter_stacking, eval_tracking_disruptor_stacking,
    eval_warp_scrambler_effects, evaluate_booster_side_effect, stacking_penalty_factor,
    DogmaEffectContext, DogmaItemState, EffectModifierSpec, ShipFittingEngine,
};
use eve_sde::{attr_ids, SdeRegistry};

fn setup_sde() -> SdeRegistry {
    SdeRegistry::with_canonical_seed()
}

#[tokio::test]
async fn test_dg1_all_domain_modifier_functions_and_isolation() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    let sde = setup_sde();

    // 1. ItemModifier: Modifies target entity directly (e.g. shipID maxVelocity)
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(100, 1236, &sde), // Overdrive II
            DogmaItemState::from_sde(1000, 587, &sde), // Rifter
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let spec_item = EffectModifierSpec {
            domain: "shipID".into(),
            func: "ItemModifier".into(),
            modified_attribute_id: attr_ids::MAX_VELOCITY,
            modifying_attribute_id: 20, // speedFactor
            operator: 6,                 // PostPercent (+12.5%)
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        ctx.item.base_attributes.insert(20, 12.5);
        assert_eq!(ctx.apply_modifier_spec(100, &spec_item, &sde), 1);
        let vel = ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        assert!((vel - 365.0 * 1.125).abs() < 1e-6);
        // Character and target are isolated
        assert_eq!(ctx.character.get_attribute(attr_ids::MAX_VELOCITY, &sde), 0.0);
    }

    // 2. LocationGroupModifier: Modifies items located in domain with matching group_id
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(101, 519, &sde), // Gyro II
            DogmaItemState::from_sde(1000, 587, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let ac2 = DogmaItemState::from_sde(201, 2881, &sde); // group 55 (Projectile Weapon)
        let lml2 = DogmaItemState::from_sde(202, 2410, &sde); // group 509 (Missile Launcher)
        let base_ac_dmg = ac2.get_attribute(64, &sde);
        let base_lml_dmg = lml2.get_attribute(64, &sde);

        ctx.ship_located_items.push(ac2);
        ctx.ship_located_items.push(lml2);

        let spec_grp = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationGroupModifier".into(),
            modified_attribute_id: 64, // damageMultiplier
            modifying_attribute_id: 64,
            operator: 4,               // PostMul (*1.10)
            group_id: Some(55),
            skill_type_id: None,
            effect_id: None,
        };
        ctx.item.base_attributes.insert(64, 1.10);
        assert_eq!(ctx.apply_modifier_spec(101, &spec_grp, &sde), 1);

        // Projectile weapon is modified (+10% damage)
        assert!((ctx.ship_located_items[0].get_attribute(64, &sde) - base_ac_dmg * 1.10).abs() < 1e-6);
        // Missile launcher is untouched
        assert_eq!(ctx.ship_located_items[1].get_attribute(64, &sde), base_lml_dmg);
        // Ship itself is untouched (default damageMultiplier is 1.0)
        assert_eq!(ctx.ship.get_attribute(64, &sde), 1.0);
    }

    // 3. LocationRequiredSkillModifier: Modifies items located in domain requiring a specific skill
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(102, 22291, &sde), // BCS II
            DogmaItemState::from_sde(1000, 587, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let lml2 = DogmaItemState::from_sde(203, 2410, &sde); // requires 3319 Missile Launcher Operation
        let ac2 = DogmaItemState::from_sde(204, 2881, &sde); // does not require 3319
        let base_lml_speed = lml2.get_attribute(51, &sde);
        let base_ac_speed = ac2.get_attribute(51, &sde);

        ctx.ship_located_items.push(lml2);
        ctx.ship_located_items.push(ac2);

        let spec_srq = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationRequiredSkillModifier".into(),
            modified_attribute_id: 51, // speed (RoF)
            modifying_attribute_id: 51,
            operator: 4,               // PostMul (*0.895)
            group_id: None,
            skill_type_id: Some(3319),
            effect_id: None,
        };
        ctx.item.base_attributes.insert(51, 0.895);
        assert_eq!(ctx.apply_modifier_spec(102, &spec_srq, &sde), 1);

        assert!((ctx.ship_located_items[0].get_attribute(51, &sde) - base_lml_speed * 0.895).abs() < 1e-6);
        assert_eq!(ctx.ship_located_items[1].get_attribute(51, &sde), base_ac_speed);
    }

    // 4. OwnerRequiredSkillModifier: Modifies items owned by charID requiring specific skill
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(103, 10037, &sde),
            DogmaItemState::from_sde(1000, 587, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let mut drone1 = DogmaItemState::from_sde(501, 2454, &sde); // Hobgoblin I (requires 3436 Drones)
        drone1.base_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3436.0);
        drone1.base_attributes.insert(64, 1.0);
        ctx.owned_items.push(drone1);

        let spec_own = EffectModifierSpec {
            domain: "charID".into(),
            func: "OwnerRequiredSkillModifier".into(),
            modified_attribute_id: 64,
            modifying_attribute_id: 64,
            operator: 6, // PostPercent (+10%)
            group_id: None,
            skill_type_id: Some(3436),
            effect_id: None,
        };
        ctx.item.base_attributes.insert(64, 10.0);
        assert_eq!(ctx.apply_modifier_spec(103, &spec_own, &sde), 1);
        assert!((ctx.owned_items[0].get_attribute(64, &sde) - 1.10).abs() < 1e-6);
    }

    // 5. ItemModifierWithFalloff: Attenuates based on distance, optimal, and falloff
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(104, 527, &sde), // Web II
            DogmaItemState::from_sde(1000, 587, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let spec_falloff = EffectModifierSpec {
            domain: "targetID".into(),
            func: "ItemModifierWithFalloff".into(),
            modified_attribute_id: attr_ids::MAX_VELOCITY,
            modifying_attribute_id: 20, // speedFactor (-60%)
            operator: 6,                // PostPercent
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        ctx.item.base_attributes.insert(20, -60.0);

        // Case A: Within optimal (10,000m <= 10,000m) -> 100% effectiveness (-60%)
        ctx.apply_modifier_spec_with_falloff(104, Some(9001), &spec_falloff, 10_000.0, 10_000.0, 3_000.0, &sde);
        let vel_opt = ctx.target.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        assert!((vel_opt - 365.0 * 0.40).abs() < 1e-6, "At optimal, speed must be reduced by 60%: got {vel_opt}");

        // Case B: At optimal + falloff (13,000m) -> 50% effectiveness (-30%)
        ctx.target.remove_modifiers_from_source(104);
        ctx.apply_modifier_spec_with_falloff(104, Some(9001), &spec_falloff, 13_000.0, 10_000.0, 3_000.0, &sde);
        let vel_fall = ctx.target.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        assert!((vel_fall - 365.0 * 0.70).abs() < 1e-6, "At optimal + falloff, speed must be reduced by 30%: got {vel_fall}");

        // Case C: At optimal + 2x falloff (16,000m) -> 0.5^4 = 6.25% effectiveness (-3.75%)
        ctx.target.remove_modifiers_from_source(104);
        ctx.apply_modifier_spec_with_falloff(104, Some(9001), &spec_falloff, 16_000.0, 10_000.0, 3_000.0, &sde);
        let vel_2fall = ctx.target.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        let expected_2fall = 365.0 * (1.0 - 0.60 * 0.0625);
        assert!((vel_2fall - expected_2fall).abs() < 1e-6);
    }

    // 6. LocationModifier: Modifies items located inside domain, never the container entity itself
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(105, 1236, &sde),
            DogmaItemState::from_sde(1000, 587, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let mut mod1 = DogmaItemState::from_sde(205, 519, &sde);
        mod1.base_attributes.insert(37, 100.0);
        ctx.ship_located_items.push(mod1);

        let loc_spec = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationModifier".into(),
            modified_attribute_id: 37,
            modifying_attribute_id: 37,
            operator: 6, // PostPercent (+25%)
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        ctx.item.base_attributes.insert(37, 25.0);
        assert_eq!(ctx.apply_modifier_spec(105, &loc_spec, &sde), 1);
        // Ship itself must NOT be modified
        assert_eq!(ctx.ship.get_attribute(37, &sde), 365.0);
        // Located item inside shipID is modified (+25%)
        assert!((ctx.ship_located_items[0].get_attribute(37, &sde) - 125.0).abs() < 1e-6);
    }

    // 7. EffectStopper: Stops target effects (e.g. Warp Scrambler stopping MWD 6441 / MJD 6442)
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(106, 448, &sde), // Warp Scrambler II
            DogmaItemState::from_sde(1000, 587, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, 587, &sde),
            DogmaItemState::from_sde(4000, 587, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let scram_eff = sde.effects.get(&5934).expect("warpScrambleBlockMWDWithNPCEffect 5934");
        ctx.apply_effect(106, scram_eff, &sde);
        assert_eq!(ctx.stopped_target_effects, vec![6441, 6442]);
        assert_eq!(ctx.target.get_attribute(104, &sde), 2.0); // 2 scramble points
    }
}

#[tokio::test]
async fn test_dg1_stacking_penalty_curve_and_exemptions() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    // 1. Exact mathematical validation of S(n) = e^{-(n-1)^2 / 7.1289}
    let s0 = stacking_penalty_factor(0);
    let s1 = stacking_penalty_factor(1);
    let s2 = stacking_penalty_factor(2);
    let s3 = stacking_penalty_factor(3);
    let s4 = stacking_penalty_factor(4);

    assert_eq!(s0, 1.0);
    assert!((s1 - (-1.0_f64 / 7.1289).exp()).abs() < 1e-9);
    assert!((s1 - 0.869118).abs() < 1e-4);
    assert!((s2 - (-4.0_f64 / 7.1289).exp()).abs() < 1e-9);
    assert!((s2 - 0.570583).abs() < 1e-4);
    assert!((s3 - (-9.0_f64 / 7.1289).exp()).abs() < 1e-9);
    assert!((s3 - 0.282955).abs() < 1e-4);
    assert!((s4 - (-16.0_f64 / 7.1289).exp()).abs() < 1e-9);
    assert!((s4 - 0.105986).abs() < 1e-4);

    let sde = setup_sde();

    // 2. Multi-module stacking test on Rifter (type 587, base maxVelocity = 365 m/s, base cargo = 140 m3)
    //    Overdrive Injector System II (type 1236): +12.5% maxVelocity (non-stackable -> penalized), -20% capacity (stackable -> unpenalized)
    let mut fit = ShipFittingEngine::from_sde(1000, 587, &sde);
    fit.set_character_skills(std::collections::HashMap::from([
        (3394, 5), // Hull Upgrades V (enables fitting Overdrive II without affecting speed)
    ]));
    let v0 = 365.0;
    let c0 = 140.0;

    let v1_exp = v0 * 1.125;
    let c1_exp = c0 * 0.80;

    fit.fit_module(2001, 1236, 11, &sde).unwrap();
    let ctx1 = fit.evaluated_context(&sde);
    assert!((ctx1.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - v1_exp).abs() < 1e-6);
    assert!((ctx1.ship.get_attribute(attr_ids::CAPACITY, &sde) - c1_exp).abs() < 1e-6);

    let v2_exp = v1_exp * (1.0 + 0.125 * s1);
    let c2_exp = c1_exp * 0.80;
    fit.fit_module(2002, 1236, 12, &sde).unwrap();
    let ctx2 = fit.evaluated_context(&sde);
    assert!((ctx2.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - v2_exp).abs() < 1e-6);
    assert!((ctx2.ship.get_attribute(attr_ids::CAPACITY, &sde) - c2_exp).abs() < 1e-6);

    let v3_exp = v2_exp * (1.0 + 0.125 * s2);
    let c3_exp = c2_exp * 0.80;
    fit.fit_module(2003, 1236, 13, &sde).unwrap();
    let ctx3 = fit.evaluated_context(&sde);
    assert!((ctx3.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - v3_exp).abs() < 1e-6);
    assert!((ctx3.ship.get_attribute(attr_ids::CAPACITY, &sde) - c3_exp).abs() < 1e-6);

    // 3. Stacking exemptions: skills, implants, boosters
    //    Add Navigation skill level 5 (+25% velocity from skill) -> skill stacks freely with modules!
    let mut skills = std::collections::HashMap::new();
    skills.insert(3394, 5); // Hull Upgrades V
    skills.insert(3449, 5); // Navigation 5: +5%/lvl = +25% unpenalized
    fit.set_character_skills(skills);

    // Add High-grade Snake Alpha (type 19540, +0.5% maxVelocity from implant) -> implant is stacking exempt!
    fit.set_implants(vec![19540]);

    let ctx_exempt = fit.evaluated_context(&sde);
    let expected_vel = v0 * 1.25 * 1.005 * (1.0 + 0.125) * (1.0 + 0.125 * s1) * (1.0 + 0.125 * s2);
    let actual_vel = ctx_exempt.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    assert!(
        (actual_vel - expected_vel).abs() < 1e-6,
        "Skill (+25%) and Implant (+0.5%) must stack freely without penalty: got {actual_vel}, expected {expected_vel}"
    );
}

#[tokio::test]
async fn test_dg1_weapon_upgrades_family() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    let sde = setup_sde();

    // Rifter fitted with:
    // Slot 27: 150mm Light AutoCannon II (2881, group 55)
    // Slot 28: 150mm Light AutoCannon II (2881, group 55)
    // Slot 11: Gyrostabilizer II (519) -> +10% damageMultiplier (64), -10.5% speed/RoF (51)
    // Slot 12: Gyrostabilizer II (519) -> 2nd gyro is stacking penalized!
    // Slot 13: Tracking Enhancer II (1999) -> +9.5% trackingSpeed (160), +10% maxRange (54), +10% falloff (158)
    let mut fit = ShipFittingEngine::from_sde(1000, 587, &sde);
    fit.set_character_skills(std::collections::HashMap::from([
        (3300, 2),  // Gunnery II
        (3302, 5),  // Small Projectile Turret V
        (11084, 1), // Small Autocannon Specialization I
        (3318, 4),  // Weapon Upgrades IV
    ]));
    fit.fit_module(1001, 2881, 27, &sde).unwrap();
    fit.fit_module(1002, 2881, 28, &sde).unwrap();

    let ctx0 = fit.evaluated_context(&sde);
    let base_dmg = ctx0.ship_located_items[0].get_attribute(64, &sde);
    let base_rof = ctx0.ship_located_items[0].get_attribute(51, &sde);
    let base_track = ctx0.ship_located_items[0].get_attribute(160, &sde);

    fit.fit_module(2001, 519, 11, &sde).unwrap();

    let ctx1 = fit.evaluated_context(&sde);
    let gun1 = ctx1.ship_located_items.iter().find(|m| m.item_id == 1001).unwrap();

    // Single Gyro II: exact 1.10x damage, 0.895x RoF
    assert!((gun1.get_attribute(64, &sde) - base_dmg * 1.10).abs() < 1e-4);
    assert!((gun1.get_attribute(51, &sde) - base_rof * 0.895).abs() < 1e-4);

    // Fit 2nd Gyrostabilizer II -> penalized against 1st gyro
    fit.fit_module(2002, 519, 12, &sde).unwrap();
    let s1 = stacking_penalty_factor(1);
    let ctx2 = fit.evaluated_context(&sde);
    let gun1_g2 = ctx2.ship_located_items.iter().find(|m| m.item_id == 1001).unwrap();

    let exp_dmg_2 = base_dmg * 1.10 * (1.0 + 0.10 * s1);
    let exp_rof_2 = base_rof * 0.895 * (1.0 - 0.105 * s1);
    assert!((gun1_g2.get_attribute(64, &sde) - exp_dmg_2).abs() < 1e-4);
    assert!((gun1_g2.get_attribute(51, &sde) - exp_rof_2).abs() < 1e-4);

    // Fit Tracking Enhancer II
    fit.fit_module(2003, 1999, 13, &sde).unwrap();
    let ctx3 = fit.evaluated_context(&sde);
    let gun1_te = ctx3.ship_located_items.iter().find(|m| m.item_id == 1001).unwrap();
    let track = gun1_te.get_attribute(160, &sde);
    assert!(track > base_track, "Tracking speed must be boosted by Tracking Enhancer: {track} > {base_track}");

    // Also test Magnetic Field Stabilizer II (type 10190) and Heat Sink II (type 2364)
    let mut hybrid_fit = ShipFittingEngine::from_sde(2000, 587, &sde);
    hybrid_fit.set_character_skills(std::collections::HashMap::from([(3318, 4)]));
    hybrid_fit.fit_module(3001, 10190, 11, &sde).unwrap(); // Magstab II
    assert_eq!(hybrid_fit.modules.len(), 1);

    let mut laser_fit = ShipFittingEngine::from_sde(3000, 587, &sde);
    laser_fit.set_character_skills(std::collections::HashMap::from([(3318, 4)]));
    laser_fit.fit_module(4001, 2364, 11, &sde).unwrap(); // Heat Sink II
    assert_eq!(laser_fit.modules.len(), 1);

}

#[tokio::test]
async fn test_dg1_propulsion_family_ab_mwd_scrambler() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    let sde = setup_sde();
    let rifter = DogmaItemState::from_sde(1000, 587, &sde);
    let base_vel = rifter.get_attribute(attr_ids::MAX_VELOCITY, &sde); // 365.0
    let base_mass = rifter.get_attribute(attr_ids::MASS, &sde); // 1,067,000 kg
    let base_sig = rifter.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde); // 35.0

    // 1. 1MN Afterburner II (type 438): speedFactor 135%, massAddition 500,000 kg
    let ab2 = DogmaItemState::from_sde(2001, 438, &sde);
    let ab_inactive = eval_propulsion_module(&rifter, &ab2, false, false, false, &sde);
    assert_eq!(ab_inactive.max_velocity, base_vel);
    assert_eq!(ab_inactive.mass, base_mass);

    let ab_active = eval_propulsion_module(&rifter, &ab2, true, false, false, &sde);
    assert!((ab_active.max_velocity - base_vel * 2.35).abs() < 1e-4);
    assert_eq!(ab_active.mass, base_mass + 500_000.0);
    assert_eq!(ab_active.signature_radius, base_sig); // Afterburner does NOT bloom sig radius!

    let ab_ovl = eval_propulsion_module(&rifter, &ab2, true, true, false, &sde);
    assert!(ab_ovl.max_velocity > ab_active.max_velocity);

    // Afterburner can activate even if warp scrambled!
    let ab_scrambled = eval_propulsion_module(&rifter, &ab2, true, false, true, &sde);
    assert!(ab_scrambled.can_activate);
    assert!((ab_scrambled.max_velocity - ab_active.max_velocity).abs() < 1e-4);

    // 2. 5MN Microwarpdrive II (type 440): speedFactor 510%, massAddition 500,000 kg, sigRadiusBonus +475%
    let mwd2 = DogmaItemState::from_sde(2002, 440, &sde);
    let mwd_active = eval_propulsion_module(&rifter, &mwd2, true, false, false, &sde);
    assert!((mwd_active.max_velocity - base_vel * 6.10).abs() < 1e-4);
    assert_eq!(mwd_active.mass, base_mass + 500_000.0);
    assert!((mwd_active.signature_radius - base_sig * 5.75).abs() < 1e-4); // 475% bloom!

    // MWD CANNOT activate when warp scrambled!
    let mwd_scrambled = eval_propulsion_module(&rifter, &mwd2, true, false, true, &sde);
    assert!(!mwd_scrambled.can_activate);
    assert_eq!(mwd_scrambled.max_velocity, base_vel);
    assert_eq!(mwd_scrambled.signature_radius, base_sig);
}

#[tokio::test]
async fn test_dg1_energy_neutralizers_and_nosferatu() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    let sde = setup_sde();

    // 1. Small Energy Neutralizer II (type 13003, amount = 55.0 GJ, opt = 5,000m, fall = 2,500m)
    let neut2 = DogmaItemState::from_sde(3001, 13003, &sde);

    // Within optimal (4,000m <= 5,000m): full 55.0 GJ drain
    let r_opt = eval_energy_neutralizer(&neut2, 4_000.0, 100.0, &sde);
    assert_eq!(r_opt.falloff_factor, 1.0);
    assert_eq!(r_opt.actual_drain, 55.0);
    assert_eq!(r_opt.target_remaining_cap, 45.0);

    // At optimal + falloff (7,500m): 50% drain (27.5 GJ)
    let r_fall = eval_energy_neutralizer(&neut2, 7_500.0, 100.0, &sde);
    assert!((r_fall.falloff_factor - 0.50).abs() < 1e-6);
    assert!((r_fall.actual_drain - 27.5).abs() < 1e-6);
    assert!((r_fall.target_remaining_cap - 72.5).abs() < 1e-6);

    // Over-drain protection: target only has 20.0 GJ -> drains exactly 20.0 GJ, leaves 0.0 GJ
    let r_empty = eval_energy_neutralizer(&neut2, 5_000.0, 20.0, &sde);
    assert_eq!(r_empty.actual_drain, 20.0);
    assert_eq!(r_empty.target_remaining_cap, 0.0);

    // 2. Small Energy Nosferatu II (type 13001, amount = 10.0 GJ, opt = 5,000m, fall = 2,500m)
    let nos2 = DogmaItemState::from_sde(3002, 13001, &sde);

    // Vampire rule: source at 20% cap (20/100), target at 80% cap (80/100) -> transfers 10.0 GJ!
    let r_nos = eval_energy_nosferatu(&nos2, 4_000.0, 20.0, 100.0, 80.0, 100.0, &sde);
    assert_eq!(r_nos.actual_transferred, 10.0);
    assert_eq!(r_nos.source_new_cap, 30.0);
    assert_eq!(r_nos.target_new_cap, 70.0);

    // Vampire rule: source at 90% cap (90/100), target at 50% cap (50/100) -> NO transfer!
    let r_nos_blocked = eval_energy_nosferatu(&nos2, 4_000.0, 90.0, 100.0, 50.0, 100.0, &sde);
    assert_eq!(r_nos_blocked.actual_transferred, 0.0);
    assert_eq!(r_nos_blocked.source_new_cap, 90.0);
    assert_eq!(r_nos_blocked.target_new_cap, 50.0);

    // Range attenuation on Nosferatu at optimal + falloff (7,500m)
    let r_nos_fall = eval_energy_nosferatu(&nos2, 7_500.0, 10.0, 100.0, 90.0, 100.0, &sde);
    assert!((r_nos_fall.actual_transferred - 5.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_dg1_electronic_warfare_family() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    let sde = setup_sde();

    // 1. ECM Jam Chance calculation: jam strength vs target sensor strength
    // Jammer: strength 6.0, opt 30,000m, fall 20,000m. Target: sensor strength 12.0
    let p_opt = compute_ecm_jam_chance(6.0, 12.0, 25_000.0, 30_000.0, 20_000.0);
    assert_eq!(p_opt, 0.50, "At optimal, jam chance is exactly 6.0 / 12.0 = 0.50");

    let p_fall = compute_ecm_jam_chance(6.0, 12.0, 50_000.0, 30_000.0, 20_000.0);
    assert!((p_fall - 0.25).abs() < 1e-6, "At optimal + falloff, jam chance is 0.50 * 0.50 = 0.25");

    // 2. Stasis Webifier II (type 527, speedFactor = -60.0%):
    let web2 = DogmaItemState::from_sde(4001, 527, &sde);
    let base_speed = 365.0;

    // Single web: 365 * 0.40 = 146.0 m/s
    let v_web1 = eval_stasis_webifier_stacking(&[&web2], &[8_000.0], base_speed, &sde);
    assert!((v_web1 - 146.0).abs() < 1e-4);

    // Dual web: 2nd web is stacking penalized S(1)!
    let s1 = stacking_penalty_factor(1);
    let v_web2 = eval_stasis_webifier_stacking(&[&web2, &web2], &[8_000.0, 8_000.0], base_speed, &sde);
    let expected_v2 = base_speed * 0.40 * (1.0 - 0.60 * s1);
    assert!((v_web2 - expected_v2).abs() < 1e-4);

    // 3. Target Painter II (type 19806, sigBonus = +30.0%):
    let tp2 = DogmaItemState::from_sde(4002, 19806, &sde);
    let base_sig = 40.0;
    let sig_tp1 = eval_target_painter_stacking(&[&tp2], &[20_000.0], base_sig, &sde);
    assert!((sig_tp1 - 40.0 * 1.30).abs() < 1e-4);

    let sig_tp2 = eval_target_painter_stacking(&[&tp2, &tp2], &[20_000.0, 20_000.0], base_sig, &sde);
    let expected_sig2 = base_sig * 1.30 * (1.0 + 0.30 * s1);
    assert!((sig_tp2 - expected_sig2).abs() < 1e-4);

    // 4. Remote Sensor Dampener II (type 1969):
    let damp2 = DogmaItemState::from_sde(4003, 1969, &sde);
    let (damp_res, damp_rng) = eval_sensor_dampener_stacking(&[&damp2], &[20_000.0], 500.0, 100_000.0, &sde);
    assert!(damp_res < 500.0);
    assert!(damp_rng < 100_000.0);

    // 5. Tracking Disruptor II (type 2109):
    let td2 = DogmaItemState::from_sde(4004, 2109, &sde);
    let (dis_track, dis_opt, dis_fall) = eval_tracking_disruptor_stacking(&[&td2], &[30_000.0], 362.0, 1_080.0, 4_730.0, &sde);
    assert!(dis_track < 362.0);
    assert!(dis_opt < 1_080.0);
    assert!(dis_fall < 4_730.0);

    // 6. Warp Scrambler II vs Warp Disruptor II:
    let scram2 = DogmaItemState::from_sde(4005, 448, &sde); // Scrambler II (2 pts, stops MWD)
    let disrup2 = DogmaItemState::from_sde(4006, 3244, &sde); // Disruptor II (1 pt, does not stop MWD)

    let state_dis = eval_warp_scrambler_effects(&[&disrup2], &[15_000.0], &sde);
    assert_eq!(state_dis.scramble_strength, 1);
    assert!(!state_dis.blocks_mwd);

    let state_scram = eval_warp_scrambler_effects(&[&scram2], &[8_000.0], &sde);
    assert_eq!(state_scram.scramble_strength, 2);
    assert!(state_scram.blocks_mwd);
}

#[tokio::test]
async fn test_dg1_command_bursts_boosters_and_implants() {
    // parity: plan:DG-1
    // parity: feat:dogma.modifier-engine
    eve_testkit::attest(&["feat:dogma.modifier-engine"]);

    let sde = setup_sde();

    // 1. Command Bursts: Shield Command Burst I with Shield Harmonizing Charge
    let burst = DogmaItemState::from_sde(5001, 42529, &sde); // Shield Command Burst I
    let charge = DogmaItemState::from_sde(5002, 42695, &sde); // Shield Harmonizing Charge
    let fleet_distances = [5_000.0, 15_000.0, 30_000.0, 50_000.0]; // 3 within 35km radius, 1 outside
    let burst_res = eval_command_burst(&burst, Some(&charge), &fleet_distances, &sde);
    assert_eq!(burst_res.affected_fleet_count, 3);
    assert!(burst_res.aggregated_bonus > 0.0);

    // Test dbuff aggregation modes: Maximum, Minimum, Additive
    assert_eq!(aggregate_dbuff_values("Maximum", &[10.0, 15.0, 12.0]), 15.0);
    assert_eq!(aggregate_dbuff_values("Minimum", &[10.0, 15.0, 12.0]), 10.0);
    assert_eq!(aggregate_dbuff_values("Additive", &[10.0, 15.0, 12.0]), 37.0);

    // 2. Combat Booster Drugs: Standard Exile Booster (type 15479)
    // Neurotoxin Control reduces side effect chance by 5%/lvl
    // Neurotoxin Recovery reduces penalty severity by 5%/lvl
    let side_fx_none = evaluate_booster_side_effect(0.20, 20.0, 5, 0, 0.18); // 0.20 * 0.75 = 0.15; roll 0.18 >= 0.15 -> None
    assert!(side_fx_none.is_none());

    let side_fx_hit = evaluate_booster_side_effect(0.20, 20.0, 0, 4, 0.10); // roll 0.10 < 0.20 -> Hit with 20% * 0.80 = 16% penalty
    assert_eq!(side_fx_hit, Some(16.0));

    // Evaluate booster fitted in ShipFittingEngine
    let booster_fit = ShipFittingEngine::from_sde(1000, 587, &sde)
        .with_boosters(vec![15479]); // Standard Exile Booster
    let ctx_booster = booster_fit.evaluated_context(&sde);
    // Standard Exile Booster has -20% armorHP penalty (attr 265)
    let base_armor = sde.types[&587].default_attributes[&265];
    assert!((ctx_booster.ship.get_attribute(265, &sde) - base_armor * 0.80).abs() < 1e-4);

    // 3. Implants: High-grade Snake Alpha (type 19540)
    let implant_fit = ShipFittingEngine::from_sde(1000, 587, &sde)
        .with_implants(vec![19540]);
    let ctx_imp = implant_fit.evaluated_context(&sde);
    // Snake Alpha has +0.5% maxVelocity (effect 394, attr 315)
    assert!((ctx_imp.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 365.0 * 1.005).abs() < 1e-4);
    // Character perception attribute boosted by +4 (attr 167)
    assert_eq!(ctx_imp.character.get_attribute(167, &sde), 4.0);
}
