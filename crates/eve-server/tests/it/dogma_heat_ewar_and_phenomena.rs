//! Integration tests for Dogma EWAR resistance attributes, heat rack thermodynamics, and wormhole phenomena.

use std::collections::HashMap;

use eve_dogma::{
    apply_wormhole_phenomenon_to_attributes, attr_ids, resolve_system_wormhole_phenomenon,
    thermodynamics, wormhole_phenomenon_type_for_class_and_name, DogmaItemState,
    ProjectedEwarEffect, ShipFittingEngine,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;

fn sde() -> &'static SdeRegistry {
    static SDE: std::sync::OnceLock<SdeRegistry> = std::sync::OnceLock::new();
    SDE.get_or_init(SdeRegistry::with_canonical_seed)
}

fn grant_all_required_skills(skills: &mut HashMap<u32, u8>, type_id: u32, sde: &SdeRegistry) {
    let Some(t) = sde.types.get(&type_id) else { return };
    for (s_attr, l_attr) in [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
        (1285, 1286),
        (1289, 1287),
        (1290, 1288),
    ] {
        if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
            let sid = sid_f as u32;
            if sid > 0 {
                let lvl = t.default_attributes.get(&l_attr).copied().unwrap_or(5.0).max(5.0) as u8;
                skills.insert(sid, lvl);
                grant_all_required_skills(skills, sid, sde);
            }
        }
    }
}

// parity: feat:dogma.ewar-resistance-attributes
#[test]
fn test_dogma_ewar_resistance_attributes_and_bastion_immunity() {
    eve_testkit::attest(&["feat:dogma.ewar-resistance-attributes"]);
    let sde = sde();

    // 1. Setup target ship (Omen 2006) with base attributes
    let mut target = ShipFittingEngine::from_sde(24_001, 2006, sde);
    target.set_character_skills(HashMap::from([
        (3300, 5),  // Gunnery V
        (3303, 5),  // Medium Energy Turret V
        (3426, 5),  // CPU Management V
        (3418, 5),  // Power Grid Management V
    ]));
    target.fit_and_online_module(34_001, 454, 27, sde).unwrap(); // Small Focused Beam Laser I

    let base_vel = target.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    let base_sig = target.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    let base_scan = target.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde);
    let base_opt = target.get_module_effective_attribute(34_001, attr_ids::MAX_RANGE, sde);

    // Verify all 7 resistance and impedance attributes default to 1.0 (no reduction)
    for attr_id in [2045_u16, 2112, 2113, 2114, 2115, 2116, 2135] {
        assert_eq!(
            target.ship.get_attribute(attr_id, sde),
            1.0,
            "Attribute {attr_id} must default to 1.0"
        );
    }

    // Set 50% resistance (0.5 multiplier) across all 7 attributes on target
    for attr_id in [2045_u16, 2112, 2113, 2114, 2115, 2116, 2135] {
        target.ship.base_attributes.insert(attr_id, 0.5);
    }

    // Project hostile EWAR at 1,000 m (well within optimal range):
    // - Tracking Disruptor I (2108): weapon disruption resistance (attr 2113)
    // - Remote Sensor Dampener II (1969): sensor dampener resistance (attr 2112)
    // - Target Painter II (19806): target painter resistance (attr 2114)
    // - Stasis Webifier I (526): stasis webifier resistance (attr 2115)
    for (src_id, mod_tid) in [
        (80_001_i64, 2108_u32),
        (80_002_i64, 1969_u32),
        (80_003_i64, 19806_u32),
        (80_004_i64, 526_u32),
    ] {
        target.upsert_projected_ewar(
            ProjectedEwarEffect {
                source_module_id: src_id,
                source_module_type_id: mod_tid,
                charge_type_id: None,
                distance: 1_000.0,
                overloaded: false,
                effective_module: DogmaItemState::from_sde(src_id, mod_tid, sde),
            },
            sde,
        );
    }

    // Target's 50% resistance scales incoming effect strength:
    // Stasis Webifier (526, speedFactor 20 = -50.0%): scaled by 0.5 -> -25.0%
    let web_bonus = sde.types[&526].default_attributes[&20];
    let expected_vel = base_vel * (1.0 + (web_bonus * 0.5) / 100.0);
    assert!(
        (target.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) - expected_vel).abs() < 1e-4,
        "Stasis webifier must scale with stasisWebifierResistance (2115)"
    );

    // Target Painter (19806, signatureRadiusBonus 554 = +30.0%): scaled by 0.5 -> +15.0%
    let tp_bonus = sde.types[&19806].default_attributes[&554];
    let expected_sig = base_sig * (1.0 + (tp_bonus * 0.5) / 100.0);
    assert!(
        (target.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde) - expected_sig).abs() < 1e-4,
        "Target painter must scale with targetPainterResistance (2114)"
    );

    // Remote Sensor Dampener (1969, scanResolutionBonus 566 = -15.3%): scaled by 0.5 -> -7.65%
    let damp_scan = sde.types[&1969].default_attributes[&566];
    let expected_scan = base_scan * (1.0 + (damp_scan * 0.5) / 100.0);
    assert!(
        (target.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde) - expected_scan).abs() < 1e-4,
        "Sensor dampener must scale with sensorDampenerResistance (2112)"
    );

    // Tracking Disruptor (2108, maxRangeBonus 351): scaled by 0.5
    let td_opt = sde.types[&2108].default_attributes[&351];
    let expected_opt = base_opt * (1.0 + (td_opt * 0.5) / 100.0);
    assert!(
        (target.get_module_effective_attribute(34_001, attr_ids::MAX_RANGE, sde) - expected_opt).abs() < 1e-4,
        "Weapon disruption must scale with weaponDisruptionResistance (2113)"
    );

    // Energy Warfare resistance (2045 = 0.5)
    let heavy_neut = DogmaItemState::from_sde(80_010, 533, sde); // Heavy Energy Neutralizer I (97 = 600 GJ)
    let neut_amt = heavy_neut.get_attribute(97, sde);
    assert!(
        (target.evaluate_projected_energy_warfare(&heavy_neut, 1_000.0, sde) - neut_amt * 0.5).abs() < 1e-5,
        "Energy warfare drain must scale with energyWarfareResistance (2045)"
    );

    // Remote Repair impedance (2116 = 0.5)
    let remote_armor = DogmaItemState::from_sde(80_011, 1609, sde); // Small Remote Armor Repairer I (84 = 200 HP)
    let rep_amt = remote_armor.get_attribute(84, sde);
    assert!(
        (target.evaluate_projected_remote_repair(&remote_armor, 1_000.0, sde) - rep_amt * 0.5).abs() < 1e-5,
        "Remote repair must scale with remoteRepairImpedance (2116)"
    );

    // Remote Assistance impedance (2135 = 0.5)
    let remote_tc = DogmaItemState::from_sde(80_012, 1982, sde); // Remote Tracking Computer I (351 = 15.0%)
    let assist_amt = remote_tc.get_attribute(351, sde);
    assert!(
        (target.evaluate_projected_remote_assistance(&remote_tc, 351, 1_000.0, sde) - assist_amt * 0.5).abs() < 1e-5,
        "Remote assistance must scale with remoteAssistanceImpedance (2135)"
    );

    // 2. Active Bastion Module I (33400, effect 6658) on Paladin Marauder (28659)
    let mut paladin = ShipFittingEngine::from_sde(24_002, 28659, sde);
    paladin.set_character_skills(HashMap::from([
        (33407, 5), // Marauders V
        (3318, 5),  // Weapon Upgrades V
        (11207, 5), // Advanced Weapon Upgrades V
        (3424, 5),  // Energy Grid Upgrades V
        (3426, 5),  // CPU Management V
        (3418, 5),  // Power Grid Management V
    ]));
    paladin.fit_and_online_module(34_100, 33400, 27, sde).unwrap(); // Bastion Module I
    paladin.activate_module(34_100, false, sde).unwrap();

    // Verify Bastion zeroes all 7 resistance and impedance attributes (immunity)
    for immune_attr in [2045_u16, 2112, 2113, 2114, 2115, 2116, 2135] {
        assert_eq!(
            paladin.ship.get_attribute(immune_attr, sde),
            0.0,
            "Active Bastion Module I must set resistance/impedance attribute {immune_attr} to 0.0"
        );
    }

    let pal_base_vel = paladin.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    paladin.upsert_projected_ewar(
        ProjectedEwarEffect {
            source_module_id: 80_099,
            source_module_type_id: 526,
            charge_type_id: None,
            distance: 1_000.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(80_099, 526, sde),
        },
        sde,
    );
    assert_eq!(
        paladin.ship.get_attribute(attr_ids::MAX_VELOCITY, sde),
        pal_base_vel,
        "Active Bastion must be 100% immune to Stasis Webifier"
    );
    assert_eq!(
        paladin.evaluate_projected_energy_warfare(&heavy_neut, 1_000.0, sde),
        0.0,
        "Active Bastion must be 100% immune to Energy Warfare (2045)"
    );
    assert_eq!(
        paladin.evaluate_projected_remote_repair(&remote_armor, 1_000.0, sde),
        0.0,
        "Active Bastion must block Remote Repairs (2116)"
    );
    assert_eq!(
        paladin.evaluate_projected_remote_assistance(&remote_tc, 351, 1_000.0, sde),
        0.0,
        "Active Bastion must block Remote Assistance (2135)"
    );

    // 3. Active Siege Module I (20280, effect 6582) on Revelation Dreadnought (19720)
    let mut dread = ShipFittingEngine::from_sde(24_003, 19720, sde);
    let mut dread_skills = HashMap::from([
        (3424, 5),  // Energy Grid Upgrades V
        (3426, 5),  // CPU Management V
        (3418, 5),  // Power Grid Management V
    ]);
    grant_all_required_skills(&mut dread_skills, 19720, sde);
    grant_all_required_skills(&mut dread_skills, 20280, sde);
    dread.set_character_skills(dread_skills);
    dread.fit_and_online_module(34_101, 20280, 27, sde).unwrap(); // Siege Module I
    dread.activate_module(34_101, false, sde).unwrap();

    for immune_attr in [2045_u16, 2112, 2113, 2114, 2115, 2116, 2135] {
        assert_eq!(
            dread.ship.get_attribute(immune_attr, sde),
            0.0,
            "Active Siege Module I must set resistance/impedance attribute {immune_attr} to 0.0"
        );
    }
    assert_eq!(dread.evaluate_projected_energy_warfare(&heavy_neut, 1_000.0, sde), 0.0);
    assert_eq!(dread.evaluate_projected_remote_repair(&remote_armor, 1_000.0, sde), 0.0);
    assert_eq!(dread.evaluate_projected_remote_assistance(&remote_tc, 351, 1_000.0, sde), 0.0);
}

// parity: feat:dogma.heat-rack-thermodynamics
#[test]
fn test_dogma_heat_rack_thermodynamics_and_skills() {
    eve_testkit::attest(&["feat:dogma.heat-rack-thermodynamics"]);
    let sde = sde();

    // 1. Fit Megathron (641) with modules across High (27, 28, 30), Med (19), Low (11) racks
    let mut fit = ShipFittingEngine::from_sde(25_001, 641, sde);
    let mut skills = HashMap::from([
        (3426, 5),  // CPU Management V
        (3418, 5),  // Power Grid Management V
        (28164, 0), // Thermodynamics level 0 initially
    ]);
    grant_all_required_skills(&mut skills, 641, sde);
    grant_all_required_skills(&mut skills, 454, sde);
    grant_all_required_skills(&mut skills, 526, sde);
    grant_all_required_skills(&mut skills, 2048, sde);
    fit.set_character_skills(skills);

    // High rack (rack 0): slots 27 (HiSlot0), 28 (HiSlot1), 30 (HiSlot3)
    let mod_hi0 = 50_001i64;
    let mod_hi1 = 50_002i64;
    let mod_hi3 = 50_003i64;
    fit.fit_module_with_state(mod_hi0, 454, 27, false, sde).unwrap();
    fit.fit_module_with_state(mod_hi1, 454, 28, false, sde).unwrap();
    fit.fit_module_with_state(mod_hi3, 454, 30, false, sde).unwrap();

    // Med rack (rack 1): slot 19 (MedSlot0)
    let mod_med = 50_004i64;
    fit.fit_module_with_state(mod_med, 526, 19, false, sde).unwrap();

    // Low rack (rack 2): slot 11 (LoSlot0)
    let mod_lo = 50_005i64;
    fit.fit_module_with_state(mod_lo, 2048, 11, false, sde).unwrap();

    // Initial rack heat must be 0 for all racks
    assert_eq!(fit.rack_heat, [0.0, 0.0, 0.0]);

    // Overloaded cycle on High slot 27 (rack 0) accumulates heat in rack 0 only
    let rep_hi = fit.apply_heat_cycle(mod_hi0, 0.12, 0.02, 5.0, 0.99, sde).unwrap();
    assert!(rep_hi.rack_heat > 0.0, "Rack 0 heat must accumulate during overload cycle");
    assert_eq!(fit.rack_heat[0], rep_hi.rack_heat);
    assert_eq!(fit.rack_heat[1], 0.0, "Rack 1 must remain cold");
    assert_eq!(fit.rack_heat[2], 0.0, "Rack 2 must remain cold");

    // Overloaded cycle on Med slot 19 (rack 1) accumulates heat in rack 1 only
    let rep_med = fit.apply_heat_cycle(mod_med, 0.10, 0.02, 5.0, 0.99, sde).unwrap();
    assert!(rep_med.rack_heat > 0.0);
    assert_eq!(fit.rack_heat[1], rep_med.rack_heat);
    assert_eq!(fit.rack_heat[2], 0.0, "Rack 2 must still remain cold");

    // Overloaded cycle on Low slot 11 (rack 2) accumulates heat in rack 2
    let rep_lo = fit.apply_heat_cycle(mod_lo, 0.08, 0.02, 5.0, 0.99, sde).unwrap();
    assert!(rep_lo.rack_heat > 0.0);
    assert_eq!(fit.rack_heat[2], rep_lo.rack_heat);

    // 2. Exponential heat dissipation when no modules overloaded
    let init_heat = 0.80_f64;
    let dissipation_rate = 0.05_f64;
    for dt in [5.0_f64, 15.0, 30.0, 60.0] {
        let expected = init_heat * (-dissipation_rate * dt).exp();
        let stepped = thermodynamics::step_rack_heat(init_heat, 0.0, dissipation_rate, dt);
        assert!(
            (stepped - expected).abs() < 1e-6,
            "Heat dissipation must be exponential: stepped={stepped}, expected={expected}"
        );
    }
    // After sufficient dissipation time, heat returns to 0.0
    let cooled = thermodynamics::step_rack_heat(init_heat, 0.0, dissipation_rate, 300.0);
    assert!(cooled < 1e-4, "Heat must dissipate to near zero");

    // 3. Slot-adjacency distance and current rack heat probability scaling
    let att_factor = 0.5;
    let p_dist0 = thermodynamics::heat_damage_probability(0.8, 0, att_factor);
    let p_dist1 = thermodynamics::heat_damage_probability(0.8, 1, att_factor);
    let p_dist2 = thermodynamics::heat_damage_probability(0.8, 2, att_factor);
    let p_dist3 = thermodynamics::heat_damage_probability(0.8, 3, att_factor);

    assert_eq!(p_dist0, 0.8);
    assert!((p_dist1 - 0.8 * (-0.5_f64).exp()).abs() < 1e-6);
    assert!((p_dist2 - 0.8 * (-1.0_f64).exp()).abs() < 1e-6);
    assert!((p_dist3 - 0.8 * (-1.5_f64).exp()).abs() < 1e-6);
    assert!(p_dist0 > p_dist1 && p_dist1 > p_dist2 && p_dist2 > p_dist3);

    // Heat damage probability scales linearly with rack heat percentage
    let p_half_heat = thermodynamics::heat_damage_probability(0.4, 1, att_factor);
    assert!((p_dist1 - 2.0 * p_half_heat).abs() < 1e-6);

    // In apply_heat_cycle: roll_u = 0.35 damages distance 1 (prob ≈ 0.485 > 0.35) but not distance 3 (prob ≈ 0.178 < 0.35)
    fit.rack_heat[0] = 0.80;
    let roll_rep = fit.apply_heat_cycle(mod_hi0, 0.0, 0.0, 0.0, 0.35, sde).unwrap();
    let adj_damaged_ids: Vec<i64> = roll_rep
        .adjacent_damaged_modules
        .iter()
        .map(|(id, _, _)| *id)
        .collect();
    assert!(
        adj_damaged_ids.contains(&mod_hi1),
        "Adjacent module at slot distance 1 must take heat damage"
    );
    assert!(
        !adj_damaged_ids.contains(&mod_hi3),
        "Module at slot distance 3 must not take heat damage when roll_u > prob"
    );

    // 4. Thermodynamics skill (type 28164): -5% heat damage per level
    let base_dmg = 20.0;
    let expected_reductions = [
        (0u8, 20.0), // 1.00x
        (1u8, 19.0), // 0.95x (-5%)
        (2u8, 18.0), // 0.90x (-10%)
        (3u8, 17.0), // 0.85x (-15%)
        (4u8, 16.0), // 0.80x (-20%)
        (5u8, 15.0), // 0.75x (-25%)
    ];
    for (lvl, expected) in expected_reductions {
        let eff = thermodynamics::effective_heat_damage(base_dmg, lvl);
        assert_eq!(eff, expected, "Thermodynamics level {lvl} must yield {expected} damage");
    }

    // Verify Thermodynamics skill reduction applied in ShipFittingEngine
    fit.character_skills.insert(28164, 0);
    let dmg_lvl0 = fit.apply_heat_cycle(mod_hi0, 0.0, 0.0, 0.0, 0.99, sde).unwrap().module_damage;
    fit.character_skills.insert(28164, 5);
    let dmg_lvl5 = fit.apply_heat_cycle(mod_hi0, 0.0, 0.0, 0.0, 0.99, sde).unwrap().module_damage;
    assert_eq!(dmg_lvl0, 1.25, "Base heat damage for Small Focused Beam Laser I is 1.25 HP");
    assert_eq!(dmg_lvl5, 1.25 * 0.75, "Level 5 Thermodynamics must reduce module heat damage by 25% (0.9375 HP)");
}

// parity: feat:dogma.wormhole-phenomena-effects
#[tokio::test]
async fn test_dogma_wormhole_phenomena_effects_and_system_classes() {
    eve_testkit::attest(&["feat:dogma.wormhole-phenomena-effects"]);
    let ctx = ServerContext::new();

    let expected_pos = [1.30, 1.44, 1.58, 1.72, 1.86, 2.00];
    let expected_neg = [0.85, 0.78, 0.71, 0.64, 0.57, 0.50];
    let expected_half_pos = [1.15, 1.22, 1.29, 1.36, 1.43, 1.50];
    let expected_wr_small = [1.60, 1.88, 2.16, 2.44, 2.72, 3.00];

    let rifter_base = ctx.sde.types.get(&587).unwrap().default_attributes.clone();

    // 1. Verify all 6 wormhole phenomena across class strength 1..=6
    for (idx, class) in (1u8..=6).enumerate() {
        let base_attrs = || -> HashMap<u16, f64> {
            let mut m = rifter_base.clone();
            for (aid, v) in [
                (64, 10.0),
                (84, 100.0),
                (160, 1.0),
                (653, 100.0),
                (654, 100.0),
                (20, 100.0),
                (554, 100.0),
                (1211, 10.0),
                (1210, 10.0),
                (116, 100.0),
            ] {
                m.insert(aid, v);
            }
            m
        };

        // Pulsar: Shield HP (+30..+100%), Signature (+30..+100%), Cap recharge time (-15..-50%), Armor EM resonance (+15..+50%)
        let mut p_attrs = base_attrs();
        let p_tid = wormhole_phenomenon_type_for_class_and_name(class, "Pulsar").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, p_tid, 6, false, &mut p_attrs));
        assert!((p_attrs[&263] - rifter_base[&263] * expected_pos[idx]).abs() < 1e-4);
        assert!((p_attrs[&552] - rifter_base[&552] * expected_pos[idx]).abs() < 1e-4);
        assert!((p_attrs[&55] - rifter_base[&55] * expected_neg[idx]).abs() < 1e-4);
        assert!((p_attrs[&267] - rifter_base[&267] * expected_half_pos[idx]).abs() < 1e-4);

        // Black Hole: Velocity (+30..+100%), Targeting range (+30..+100%), Explosion vel (+30..+100%), Inertia (+15..+50%), Web (-15..-50%)
        let mut bh_attrs = base_attrs();
        let bh_tid = wormhole_phenomenon_type_for_class_and_name(class, "Black Hole").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, bh_tid, 6, false, &mut bh_attrs));
        assert!((bh_attrs[&37] - rifter_base[&37] * expected_pos[idx]).abs() < 1e-4);
        assert!((bh_attrs[&76] - rifter_base[&76] * expected_pos[idx]).abs() < 1e-4);
        assert!((bh_attrs[&653] - 100.0 * expected_pos[idx]).abs() < 1e-4);
        assert!((bh_attrs[&70] - rifter_base[&70] * expected_half_pos[idx]).abs() < 1e-4);
        assert!((bh_attrs[&20] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        // Cataclysmic Variable: Cap capacity (+30..+100%), Cap recharge (+15..+50%), Local rep (-15..-50%)
        let mut cv_attrs = base_attrs();
        let cv_tid = wormhole_phenomenon_type_for_class_and_name(class, "Cataclysmic Variable").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, cv_tid, 6, false, &mut cv_attrs));
        assert!((cv_attrs[&482] - rifter_base[&482] * expected_pos[idx]).abs() < 1e-4);
        assert!((cv_attrs[&55] - rifter_base[&55] * expected_half_pos[idx]).abs() < 1e-4);
        assert!((cv_attrs[&84] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        // Magnetar: Damage (+30..+100%), Explosion radius (+30..+100%), Tracking (-15..-50%), Target range (-15..-50%), Painter (-15..-50%)
        let mut mg_attrs = base_attrs();
        let mg_tid = wormhole_phenomenon_type_for_class_and_name(class, "Magnetar").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, mg_tid, 6, false, &mut mg_attrs));
        assert!((mg_attrs[&64] - 10.0 * expected_pos[idx]).abs() < 1e-4);
        assert!((mg_attrs[&654] - 100.0 * expected_pos[idx]).abs() < 1e-4);
        assert!((mg_attrs[&160] - 1.0 * expected_neg[idx]).abs() < 1e-4);
        assert!((mg_attrs[&76] - rifter_base[&76] * expected_neg[idx]).abs() < 1e-4);
        assert!((mg_attrs[&554] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        // Red Giant: Heat damage (+15..+50%), Overload (+30..+100%), Smartbomb (+30..+100%)
        let mut rg_attrs = base_attrs();
        let rg_tid = wormhole_phenomenon_type_for_class_and_name(class, "Red Giant").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, rg_tid, 6, false, &mut rg_attrs));
        assert!((rg_attrs[&1211] - 10.0 * expected_half_pos[idx]).abs() < 1e-4);
        assert!((rg_attrs[&1210] - 10.0 * expected_pos[idx]).abs() < 1e-4);
        assert!((rg_attrs[&116] - 100.0 * expected_pos[idx]).abs() < 1e-4);

        // Wolf-Rayet: Armor HP (+30..+100%), Signature (-15..-50%), Small weapon (+60..+200%), Shield EM resonance (+15..+50%)
        let mut wr_attrs = base_attrs();
        let wr_tid = wormhole_phenomenon_type_for_class_and_name(class, "Wolf-Rayet").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, false, &mut wr_attrs));
        assert!((wr_attrs[&265] - rifter_base[&265] * expected_pos[idx]).abs() < 1e-4);
        assert!((wr_attrs[&552] - rifter_base[&552] * expected_neg[idx]).abs() < 1e-4);
        assert!((wr_attrs[&64] - 10.0 * expected_wr_small[idx]).abs() < 1e-4);
        assert!((wr_attrs[&271] - rifter_base[&271] * expected_half_pos[idx]).abs() < 1e-4);

        // 2. Immunity: Sleepers (category 11 or is_npc_or_sleeper) and Structures (category 65) must never receive effects
        let mut sleeper_attrs = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, true, &mut sleeper_attrs));
        assert_eq!(sleeper_attrs[&265], rifter_base[&265]);

        let mut sleeper_cat11 = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 11, false, &mut sleeper_cat11));
        assert_eq!(sleeper_cat11[&265], rifter_base[&265]);

        let mut struct_attrs = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 65, false, &mut struct_attrs));
        assert_eq!(struct_attrs[&265], rifter_base[&265]);
    }

    // 3. Verify C13 systems resolve to C6 Wolf-Rayet values (beacon 30879)
    let c13_sys = 31_002_585u32;
    let (raw_cls, eff_cls, name, beacon_tid) =
        resolve_system_wormhole_phenomenon(&ctx.sde, c13_sys, None).expect("C13 shattered");
    assert_eq!((raw_cls, eff_cls, name, beacon_tid), (13, 6, "Wolf-Rayet", 30879));

    // 4. Verify Drifter systems resolve to C2 strength (beacon 30875)
    let drifter_sys = 31_000_006u32;
    let (raw_cls_d, eff_cls_d, name_d, beacon_tid_d) =
        resolve_system_wormhole_phenomenon(&ctx.sde, drifter_sys, None).expect("Drifter system");
    assert_eq!((raw_cls_d, eff_cls_d, name_d, beacon_tid_d), (18, 2, "Wolf-Rayet", 30875));

    // 5. System-wide effects HUD RPC verification
    let mut sess = SessionState::new(4005);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let hud_c13 = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetSystemWideEffectsInfo",
            &[PyRep::Integer(c13_sys as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSystemWideEffectsInfo C13");

    assert_eq!(hud_c13.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hud_c13.dict_get("hudVisible").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hud_c13.dict_get("appliesToCapsuleerShipsOnly").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hud_c13.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));
    assert_eq!(hud_c13.dict_get("wormholeClass").and_then(|v| v.as_i64()), Some(13));
    assert_eq!(hud_c13.dict_get("effectiveClassStrength").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(hud_c13.dict_get("effectTypeID").and_then(|v| v.as_i64()), Some(30879));

    let hud_drifter = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetSystemWideEffectsInfo",
            &[PyRep::Integer(drifter_sys as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSystemWideEffectsInfo Drifter");

    assert_eq!(hud_drifter.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hud_drifter.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));
    assert_eq!(hud_drifter.dict_get("wormholeClass").and_then(|v| v.as_i64()), Some(18));
    assert_eq!(hud_drifter.dict_get("effectiveClassStrength").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(hud_drifter.dict_get("effectTypeID").and_then(|v| v.as_i64()), Some(30875));
}
