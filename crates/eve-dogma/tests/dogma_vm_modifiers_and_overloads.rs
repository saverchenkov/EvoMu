//! Integration tests for `eve-dogma` real SDE fitting cascades, dbuff operators,
//! VM opcodes, range falloff cutoffs, and wormhole phenomena (`DG-1`..`DG-4`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;

use eve_dogma::{
    DogmaExpressionNode, DogmaVmOpcode, ModuleState, ShipFittingEngine,
};
use eve_sde::{
    attr_ids, InvType, SdeRegistry,
};

#[cfg(test)]
#[path = "dogma_vm_modifiers_and_overloads/dbuff_and_vm_opcodes_tests.rs"]
mod dbuff_and_vm_opcodes_tests;

pub(crate) fn load_sde() -> SdeRegistry {
    let p = std::env::var("EVOMU_SDE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Path::new("/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite").to_path_buf());
    SdeRegistry::open(p).expect("SDE sqlite")
}

pub(crate) fn find_type(sde: &SdeRegistry, name: &str) -> u32 {
    sde.types
        .values()
        .find(|t| t.name == name)
        .map(|t| t.type_id)
        .unwrap_or_else(|| panic!("missing SDE type {name}"))
}

pub(crate) fn make_type(
    base: &InvType,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
    effect_ids: Vec<u16>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        published: true,
        default_attributes,
        effect_ids,
        ..base.clone()
    }
}

pub(crate) fn const_node(v: f64) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode: DogmaVmOpcode::DefFloat,
        value_f64: Some(v),
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    }
}

#[test]
fn test_real_sde_combat_ship_fit_overloads_scripts_and_drones() {
    let sde = load_sde();
    let hull_id = find_type(&sde, "Hurricane");
    let mut engine = ShipFittingEngine::from_sde(100, hull_id, &sde);

    // Equip full level-V skills so all T2 modules & drones qualify cleanly
    let mut skills = HashMap::new();
    for tid in sde
        .types
        .values()
        .filter(|t| t.category_id == 16 && t.published)
        .map(|t| t.type_id)
    {
        skills.insert(tid, 5u8);
    }
    engine.set_character_skills(skills);
    engine.recalculate_stats(&sde);

    let base_vel = engine.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let base_sig = engine.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde);

    // High slot: 425mm AutoCannon II with Hail M loaded & active + overloaded
    let ac_id = find_type(&sde, "425mm AutoCannon II");
    let hail_m = find_type(&sde, "Hail M");
    engine.fit_module_with_state(1001, ac_id, 27, true, &sde).unwrap();
    engine.load_charge(1001, hail_m, 40, &sde).unwrap();
    engine.activate_module(1001, true, &sde).unwrap();

    // Med slots: 50MN Y-T8 Compact Microwarpdrive (active), Multispectrum Shield Hardener II (active + overloaded),
    // Tracking Computer II with Optimal Range Script
    let mwd_id = find_type(&sde, "50MN Y-T8 Compact Microwarpdrive");
    let hardener_id = find_type(&sde, "Multispectrum Shield Hardener II");
    let tc_id = find_type(&sde, "Tracking Computer II");
    let opt_script = find_type(&sde, "Optimal Range Script");
    engine.fit_module_with_state(1002, mwd_id, 19, true, &sde).unwrap();
    engine.activate_module(1002, false, &sde).unwrap();
    engine.fit_module_with_state(1003, hardener_id, 20, true, &sde).unwrap();
    engine.activate_module(1003, true, &sde).unwrap();
    engine.fit_module_with_state(1004, tc_id, 21, true, &sde).unwrap();
    engine.load_charge(1004, opt_script, 1, &sde).unwrap();
    engine.activate_module(1004, false, &sde).unwrap();

    // Low slots: Damage Control II, Gyrostabilizer II, Nanofiber Internal Structure II
    let dcu_id = find_type(&sde, "Damage Control II");
    let gyro_id = find_type(&sde, "Gyrostabilizer II");
    let nano_id = find_type(&sde, "Nanofiber Internal Structure II");
    engine.fit_module_with_state(1005, dcu_id, 11, true, &sde).unwrap();
    engine.fit_module_with_state(1006, gyro_id, 12, true, &sde).unwrap();
    engine.fit_module_with_state(1007, nano_id, 13, true, &sde).unwrap();

    // Launch Warrior II drone & inspect effective drone attributes
    let warrior_id = find_type(&sde, "Warrior II");
    engine.drones.push((5001, warrior_id));
    engine.recalculate_stats(&sde);

    let eff_vel = engine.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let eff_sig = engine.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde);
    let eff_dmg_mult = engine.get_module_effective_attribute(1001, attr_ids::DAMAGE_MULTIPLIER, &sde);
    let base_dmg_mult = sde
        .types
        .get(&ac_id)
        .and_then(|t| t.default_attributes.get(&attr_ids::DAMAGE_MULTIPLIER).copied())
        .unwrap_or(1.0);
    assert!(eff_vel > base_vel * 2.5, "MWD should strongly boost velocity");
    assert!(eff_sig > base_sig * 2.0, "MWD active effect blooms signature radius");
    assert!(eff_dmg_mult > base_dmg_mult, "Gyro + overload should boost turret damage multiplier");

    let base_drone_dmg = sde
        .types
        .get(&warrior_id)
        .and_then(|t| t.default_attributes.get(&attr_ids::DAMAGE_MULTIPLIER).copied())
        .unwrap_or(1.0);
    let eff_drone_dmg = engine.get_drone_effective_attribute(5001, attr_ids::DAMAGE_MULTIPLIER, &sde);
    assert!(eff_drone_dmg >= base_drone_dmg);
    assert_eq!(engine.get_drone_effective_attribute(999_999_999, 64, &sde), 0.0);

    // Offline active/overloaded module automatically deactivates & de-overloads it
    engine.take_module_offline(1001, &sde).unwrap();
    assert_eq!(engine.modules.get(&1001).unwrap().state, ModuleState::Offline);
    engine.set_module_online(1001, &sde).unwrap();
    assert_eq!(engine.modules.get(&1001).unwrap().state, ModuleState::Online);
}

#[test]
fn test_fitting_calibration_overflow_max_group_online_and_offline_cascade() {
    let mut sde = load_sde();
    let base = sde.types.get(&587).expect("Rifter").clone();

    let hull_id = 990_001u32;
    sde.types.insert(
        hull_id,
        make_type(
            &base,
            hull_id,
            25,
            6,
            "Test Frigate",
            HashMap::from([
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
                (attr_ids::UPGRADE_CAPACITY, 100.0),
                (attr_ids::HI_SLOTS, 4.0),
                (attr_ids::MED_SLOTS, 4.0),
                (attr_ids::LOW_SLOTS, 4.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (1547, 1.0),
            ]),
            vec![],
        ),
    );

    let rig_id = 990_002u32;
    sde.types.insert(
        rig_id,
        make_type(
            &base,
            rig_id,
            774,
            7,
            "Test Cal Rig",
            HashMap::from([(attr_ids::UPGRADE_COST, 75.0), (1547, 1.0)]),
            vec![2663],
        ),
    );

    let burst_id = 990_003u32;
    sde.types.insert(
        burst_id,
        make_type(
            &base,
            burst_id,
            1770,
            7,
            "Test Command Burst",
            HashMap::from([(978, 1.0), (attr_ids::CPU, 10.0)]),
            vec![16, 12],
        ),
    );

    let burst_rig_id = 990_004u32;
    sde.types.insert(
        burst_rig_id,
        make_type(
            &base,
            burst_rig_id,
            774,
            7,
            "Test Burst Rig",
            HashMap::from([(2542, 1.0), (attr_ids::UPGRADE_COST, 10.0), (1547, 1.0)]),
            vec![2663, 6766],
        ),
    );

    let mut st = ShipFittingEngine::from_sde(500, hull_id, &sde);
    st.fit_module_with_state(2001, rig_id, 92, true, &sde).unwrap();
    // Second 75-cal rig overflows 75 + 75 > 100 in fit_module_with_state
    assert!(st.fit_module_with_state(2002, rig_id, 93, true, &sde).is_err());
    // Fit dynamic-attribute 75-cal rig and verify rollback on Insufficient Calibration
    assert!(st
        .fit_module_with_dynamic_attributes(
            2002,
            burst_rig_id,
            93,
            true,
            HashMap::from([(attr_ids::UPGRADE_COST, 75.0)]),
            &sde,
        )
        .is_err());
    assert!(!st.modules.contains_key(&2002));

    // Command burst maxGroupOnline: 1 allowed without rig, 2 with burst rig, 3rd rejected
    st.fit_module_with_state(2010, burst_id, 27, true, &sde).unwrap();
    st.fit_module_with_state(2011, burst_id, 28, false, &sde).unwrap();
    assert!(st.set_module_online(2011, &sde).is_err());
    st.fit_module_with_state(2012, burst_rig_id, 93, true, &sde).unwrap();
    st.set_module_online(2011, &sde).unwrap();
    assert!(st.fit_module_with_state(2013, burst_id, 29, true, &sde).is_err());
    st.unfit_module(2010, &sde).unwrap();
    st.unfit_module(2011, &sde).unwrap();

    // Multi-rack offline cascade (enforce_post_unfit_or_offline_constraints)
    let mod_id = 990_005u32;
    sde.types.insert(
        mod_id,
        make_type(
            &base,
            mod_id,
            53,
            7,
            "Heavy CPU Module",
            HashMap::from([(attr_ids::CPU, 60.0), (attr_ids::POWER, 60.0)]),
            vec![16, 12, 13, 11],
        ),
    );
    st.fit_module_with_state(2020, mod_id, 30, false, &sde).unwrap(); // high slot
    st.fit_module_with_state(2021, mod_id, 19, false, &sde).unwrap(); // med slot
    st.fit_module_with_state(2022, mod_id, 11, false, &sde).unwrap(); // low slot
    for id in [2020, 2021, 2022] {
        st.modules.get_mut(&id).unwrap().state = ModuleState::Online;
    }
    st.recalculate_stats(&sde);
    let forced = st.enforce_post_unfit_or_offline_constraints(&sde);
    assert!(forced.len() >= 2);
    assert_eq!(forced[0], 2020);
    assert_eq!(forced[1], 2021);
}
