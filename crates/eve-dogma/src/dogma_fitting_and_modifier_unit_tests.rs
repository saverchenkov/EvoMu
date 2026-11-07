//! Fitting, wormhole phenomenon, and modifier unit tests for `eve-dogma`.

use super::*;
use eve_sde::{DogmaEffectType, EffectModifierSpec, SdeRegistry};
use std::collections::HashMap;

fn base_seed() -> (SdeRegistry, DogmaEffectType) {
    let sde = SdeRegistry::with_canonical_seed();
    let eff = sde
        .effects
        .values()
        .find(|e| e.effect_category == 0)
        .or_else(|| sde.effects.values().next())
        .expect("canonical seed has at least one effect")
        .clone();
    (sde, eff)
}

#[test]
fn test_implants_boosters_drones_and_effective_module_state() {
    let (mut sde, mut eff_tpl) = base_seed();
    eff_tpl.effect_category = 0;
    let inv_tpl = sde.types.values().next().unwrap().clone();

    let ship_tid = 90_100;
    let imp_tid = 90_101;
    let booster_tid = 90_102;
    let neuro_skill_tid = 25_538;
    let mod_tid = 90_103;
    let charge_tid = 90_104;
    let drone_tid = 90_105;

    let mut ship = inv_tpl.clone();
    ship.type_id = ship_tid;
    ship.group_id = 25;
    ship.category_id = 6;
    ship.default_attributes = HashMap::from([
        (attr_ids::MAX_VELOCITY, 200.0),
        (attr_ids::HI_SLOTS, 2.0),
        (attr_ids::CPU_OUTPUT, 200.0),
        (attr_ids::POWER_OUTPUT, 200.0),
    ]);
    sde.types.insert(ship_tid, ship);

    // Implant with passive itemID + shipID modifier
    let imp_eff = 8201;
    let mut e1 = eff_tpl.clone();
    e1.effect_id = imp_eff;
    e1.modifiers = vec![EffectModifierSpec {
        domain: "shipID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: 312,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    }];
    sde.effects.insert(imp_eff, e1);
    let mut imp = inv_tpl.clone();
    imp.type_id = imp_tid;
    imp.group_id = 300;
    imp.category_id = 20;
    imp.effect_ids = vec![imp_eff];
    imp.default_attributes = HashMap::from([(312, 5.0)]);
    sde.types.insert(imp_tid, imp);

    // Neurotoxin Control skill (25538): charID LocationGroupModifier on group 303
    let neuro_eff = 8202;
    let mut e2 = eff_tpl.clone();
    e2.effect_id = neuro_eff;
    e2.modifiers = vec![EffectModifierSpec {
        domain: "charID".into(),
        func: "LocationGroupModifier".into(),
        modified_attribute_id: 1126,
        modifying_attribute_id: 459,
        operator: 6,
        group_id: Some(303),
        skill_type_id: None,
        effect_id: None,
    }];
    sde.effects.insert(neuro_eff, e2);
    let mut neuro = inv_tpl.clone();
    neuro.type_id = neuro_skill_tid;
    neuro.group_id = 258;
    neuro.category_id = 16;
    neuro.effect_ids = vec![neuro_eff];
    neuro.default_attributes = HashMap::from([(459, -5.0)]);
    sde.types.insert(neuro_skill_tid, neuro);

    // Booster with both primary beneficial effect and optional penalty side-effect
    let b_good_eff = 8203;
    let b_bad_eff = 8204;
    let mut e3 = eff_tpl.clone();
    e3.effect_id = b_good_eff;
    e3.modifiers = vec![EffectModifierSpec {
        domain: "shipID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: 1125,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    }];
    sde.effects.insert(b_good_eff, e3);
    let mut e4 = eff_tpl.clone();
    e4.effect_id = b_bad_eff;
    e4.modifiers = vec![EffectModifierSpec {
        domain: "shipID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::CPU_OUTPUT,
        modifying_attribute_id: 1126,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    }];
    sde.effects.insert(b_bad_eff, e4);
    let mut booster = inv_tpl.clone();
    booster.type_id = booster_tid;
    booster.group_id = 303;
    booster.category_id = 14;
    booster.effect_ids = vec![b_good_eff, b_bad_eff];
    booster.default_attributes = HashMap::from([(1125, 10.0), (1126, -20.0)]);
    sde.types.insert(booster_tid, booster);

    // Module + charge with `otherID` script modifier
    let charge_eff = 8205;
    let mut e5 = eff_tpl;
    e5.effect_id = charge_eff;
    e5.modifiers = vec![EffectModifierSpec {
        domain: "otherID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::FALLOFF,
        modifying_attribute_id: 796,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    }];
    sde.effects.insert(charge_eff, e5);
    let mut charge = inv_tpl.clone();
    charge.type_id = charge_tid;
    charge.group_id = 907;
    charge.category_id = 8;
    charge.effect_ids = vec![charge_eff];
    charge.default_attributes = HashMap::from([(796, 100.0), (attr_ids::MAX_VELOCITY, 1500.0)]);
    sde.types.insert(charge_tid, charge);

    let mut m_type = inv_tpl.clone();
    m_type.type_id = mod_tid;
    m_type.group_id = 53;
    m_type.category_id = 7;
    m_type.effect_ids = vec![12, 16];
    m_type.default_attributes = HashMap::from([
        (attr_ids::FALLOFF, 10_000.0),
        (attr_ids::CPU, 10.0),
        (attr_ids::POWER, 10.0),
    ]);
    sde.types.insert(mod_tid, m_type);

    let mut drone = inv_tpl;
    drone.type_id = drone_tid;
    drone.group_id = 100;
    drone.category_id = 18;
    drone.default_attributes = HashMap::from([(attr_ids::MAX_VELOCITY, 2500.0)]);
    sde.types.insert(drone_tid, drone);

    let mut eng = ShipFittingEngine::from_sde(1, ship_tid, &sde)
        .with_character_skills(HashMap::from([(neuro_skill_tid, 4)]))
        .with_implants(vec![imp_tid, imp_tid])
        .with_boosters(vec![booster_tid]);
    eng.drones.push((999, drone_tid));
    eng.booster_side_effects.insert(booster_tid, vec![]);
    eng.recalculate_stats(&sde);
    assert!((eng.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 242.55).abs() < 1e-6);
    assert!((eng.ship.get_attribute(attr_ids::CPU_OUTPUT, &sde) - 200.0).abs() < 1e-6);

    // Include side-effect b_bad_eff -> CPU output reduced
    eng.booster_side_effects
        .insert(booster_tid, vec![u32::from(b_bad_eff)]);
    eng.recalculate_stats(&sde);
    assert!(eng.ship.get_attribute(attr_ids::CPU_OUTPUT, &sde) < 200.0);

    // Fit module, load charge, and test build_effective_module_state both fitted & hypothetical
    eng.fit_module(50, mod_tid, 27, &sde).unwrap();
    if let Some(fm) = eng.modules.get_mut(&50) {
        fm.charge_type_id = Some(charge_tid);
        fm.charge_quantity = 1;
    }
    let eff_state = eng.build_effective_module_state(50, mod_tid, true, &sde);
    assert!((eff_state.get_attribute(attr_ids::FALLOFF, &sde) - 20_000.0).abs() < 1e-6);
    assert!((eng.get_charge_effective_attribute(50, attr_ids::MAX_VELOCITY, &sde) - 1500.0).abs() < 1e-6);

    let hyp_state = eng.build_effective_module_state(99_999, mod_tid, false, &sde);
    assert!((hyp_state.get_attribute(attr_ids::FALLOFF, &sde) - 10_000.0).abs() < 1e-6);
}

#[test]
fn test_wormhole_phenomena_and_t3c_subsystems() {
    let mut sde = SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE");

    // Non-capsuleer and Sleepers are never affected by wormhole phenomena
    let mut sleeper_attrs = HashMap::from([(attr_ids::SHIELD_CAPACITY, 1000.0)]);
    assert!(!apply_wormhole_phenomenon_to_attributes(
        &sde,
        30844,
        11,
        true,
        &mut sleeper_attrs
    ));
    assert_eq!(sleeper_attrs[&attr_ids::SHIELD_CAPACITY], 1000.0);

    // Capsuleer ship receives Class 1 Pulsar beacon 30844 shield bonus
    let mut cap_attrs = HashMap::from([(attr_ids::SHIELD_CAPACITY, 1000.0)]);
    assert!(apply_wormhole_phenomenon_to_attributes(
        &sde,
        30844,
        6,
        false,
        &mut cap_attrs
    ));
    assert!(cap_attrs[&attr_ids::SHIELD_CAPACITY] > 1000.0);

    // Non-beacon type rejected
    assert!(!apply_wormhole_phenomenon_to_attributes(
        &sde,
        587,
        6,
        false,
        &mut cap_attrs
    ));

    // Shattered C13 system maps to Wolf-Rayet effective class 6
    let mut c13_sys = sde.solar_systems.values().next().unwrap().clone();
    c13_sys.solar_system_id = 31_002_999;
    c13_sys.wormhole_class_id = Some(13);
    sde.solar_systems.insert(31_002_999, c13_sys);
    let resolved = resolve_system_wormhole_phenomenon(&sde, 31_002_999, Some("Wolf-Rayet")).unwrap();
    assert_eq!(resolved.0, 13);
    assert_eq!(resolved.1, 6);
    assert_eq!(resolved.2, "Wolf-Rayet");

    // Drifter C14 maps to effective class 2
    let mut c14_sys = sde.solar_systems.values().next().unwrap().clone();
    c14_sys.solar_system_id = 31_003_001;
    c14_sys.wormhole_class_id = Some(14);
    sde.solar_systems.insert(31_003_001, c14_sys);
    let resolved14 = resolve_system_wormhole_phenomenon(&sde, 31_003_001, Some("Pulsar")).unwrap();
    assert_eq!(resolved14.0, 14);
    assert_eq!(resolved14.1, 2);

    // Thera C12 returns None
    let mut c12_sys = sde.solar_systems.values().next().unwrap().clone();
    c12_sys.solar_system_id = 31_000_005;
    c12_sys.wormhole_class_id = Some(12);
    sde.solar_systems.insert(31_000_005, c12_sys);
    assert!(resolve_system_wormhole_phenomenon(&sde, 31_000_005, Some("Pulsar")).is_none());

    // T3C subsystem slot modifiers dynamically populate hull hi/med/low/turret/launcher slots
    let t3c_tid = 29_984;
    let sub_tid = 90_200;
    let mut sub = sde.types.values().next().unwrap().clone();
    sub.type_id = sub_tid;
    sub.group_id = 954;
    sub.category_id = 32;
    sub.effect_ids = vec![3772];
    sub.default_attributes = HashMap::from([
        (1366, 125.0),
        (1374, 3.0),
        (1375, 2.0),
        (1376, 1.0),
        (1368, 2.0),
        (1369, 1.0),
    ]);
    sde.types.insert(sub_tid, sub);
    let mut t3c = ShipFittingEngine::from_sde(10, t3c_tid, &sde);
    t3c.fit_module(500, sub_tid, 125, &sde)
        .unwrap();
    assert_eq!(t3c.ship.get_attribute(attr_ids::HI_SLOTS, &sde), 3.0);
    assert_eq!(t3c.ship.get_attribute(attr_ids::MED_SLOTS, &sde), 2.0);
    assert_eq!(t3c.ship.get_attribute(attr_ids::LOW_SLOTS, &sde), 1.0);

    // Real W-space system with secondary sun resolves phenomenon automatically when override is None
    if let Ok(suns) = sde.load_secondary_suns() {
        for (&wh_sys_id, _) in suns.iter().take(20) {
            if let Some((raw_c, eff_c, name, beacon_id)) =
                resolve_system_wormhole_phenomenon(&sde, wh_sys_id, None)
            {
                assert!(raw_c >= 1 && eff_c >= 1 && !name.is_empty() && beacon_id >= 30844);
                break;
            }
        }
    }
}

#[test]
fn test_cpu_pg_online_rollbacks_low_rack_heat_and_self_modifiers() {
    let (mut sde, mut eff_tpl) = base_seed();
    eff_tpl.effect_category = 0;
    let inv_tpl = sde.types.values().next().unwrap().clone();

    let ship_tid = 90_300;
    let imp_tid = 90_301;
    let booster_tid = 90_302;
    let skill_tid = 90_303;
    let mod_tid = 90_304;
    let charge_tid = 90_305;

    let mut ship = inv_tpl.clone();
    ship.type_id = ship_tid;
    ship.group_id = 25;
    ship.category_id = 6;
    ship.default_attributes = HashMap::from([
        (attr_ids::HI_SLOTS, 4.0),
        (attr_ids::LOW_SLOTS, 4.0),
        (attr_ids::CPU_OUTPUT, 50.0),
        (attr_ids::POWER_OUTPUT, 50.0),
        (attr_ids::HEAT_DISSIPATION_RATE_HI, 0.01),
    ]);
    sde.types.insert(ship_tid, ship);

    // Self-modifying itemID effects on implant, booster, and skill
    let mut e_self = eff_tpl.clone();
    e_self.effect_id = 8301;
    e_self.modifiers = vec![EffectModifierSpec {
        domain: "itemID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: 317,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    }];
    sde.effects.insert(8301, e_self);

    for (tid, gid, cid, effs, attrs) in [
        (imp_tid, 300_u32, 20_u32, vec![8301], HashMap::from([(317, 10.0), (attr_ids::MAX_VELOCITY, 100.0)])),
        (booster_tid, 303, 20, vec![8301], HashMap::from([(317, 15.0), (attr_ids::MAX_VELOCITY, 100.0)])),
        (skill_tid, 258, 16, vec![8301], HashMap::from([(317, 20.0), (attr_ids::MAX_VELOCITY, 100.0)])),
    ] {
        let mut t = inv_tpl.clone();
        t.type_id = tid;
        t.group_id = gid;
        t.category_id = cid;
        t.effect_ids = effs;
        t.default_attributes = attrs;
        sde.types.insert(tid, t);
    }

    let mut ch = inv_tpl.clone();
    ch.type_id = charge_tid;
    ch.group_id = 907;
    ch.category_id = 8;
    sde.types.insert(charge_tid, ch);

    let mut m_t = inv_tpl;
    m_t.type_id = mod_tid;
    m_t.group_id = 53;
    m_t.category_id = 7;
    m_t.effect_ids = vec![11]; // lowSlot
    m_t.default_attributes = HashMap::from([(attr_ids::CPU, 30.0), (attr_ids::POWER, 30.0)]);
    sde.types.insert(mod_tid, m_t);

    let mut eng = ShipFittingEngine::from_sde(1, ship_tid, &sde)
        .with_character_skills(HashMap::from([(skill_tid, 5), (90_399, 0), (999_999, 3)]))
        .with_implants(vec![imp_tid])
        .with_boosters(vec![booster_tid]);
    eng.recalculate_stats(&sde);

    // Fit two 30-CPU / 30-PG modules (total 60 > 50 hull output) -> second online fails Insufficient CPU, then PG
    eng.fit_module(1, mod_tid, 11, &sde).unwrap();
    eng.fit_module_with_state(2, mod_tid, 12, false, &sde).unwrap();
    let cpu_err = eng.set_module_online(2, &sde).unwrap_err();
    assert!(cpu_err.contains("Insufficient CPU"));

    // Give enough CPU so PG check trips next
    if let Some(m) = eng.modules.get_mut(&2) {
        m.dynamic_attribute_overrides.insert(attr_ids::CPU, 10.0);
    }
    let pg_err = eng.set_module_online(2, &sde).unwrap_err();
    assert!(pg_err.contains("Insufficient Powergrid"));

    // Uncached module with loaded charge + overloaded = true exercises build_effective_module_state fallbacks
    eng.fit_module_with_state(3, mod_tid, 13, false, &sde).unwrap();
    if let Some(m) = eng.modules.get_mut(&3) {
        m.charge_type_id = Some(charge_tid);
    }
    let eff = eng.build_effective_module_state(3, mod_tid, true, &sde);
    assert_eq!(eff.type_id, mod_tid);

    // Low-rack heat cycle with brittle adjacent module burns out adjacent module cleanly
    if let Some(am) = eng.modules.get_mut(&2) {
        am.state = ModuleState::Online;
        am.max_hp = 0.01;
    }
    eng.rack_heat[2] = 99.0;
    let report = eng
        .apply_heat_cycle(3, 1.0, 0.0, 1.0, 0.0, &sde)
        .unwrap();
    assert!(!report.adjacent_damaged_modules.is_empty());
    assert!(eng.modules[&2].damage_hp > 0.0);
}

#[test]
fn test_kill_lib_and_combat_eval_mutants() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_item = DogmaItemState::from_sde(1, 587, &sde);
    let node = |opcode, value_f64, attribute_id, operator, arg1, arg2| DogmaExpressionNode {
        opcode,
        value_f64,
        attribute_id,
        operator,
        arg1,
        arg2,
    };
    let c = |v: f64| Box::new(node(DogmaVmOpcode::DefFloat, Some(v), None, None, None, None));
    let bin = |op, a, b| node(op, None, None, None, Some(c(a)), Some(c(b)));
    assert_eq!(bin(DogmaVmOpcode::Or, 0.0, 0.0).evaluate(&base_item, &sde), 0.0);
    assert_eq!(bin(DogmaVmOpcode::Or, 0.0, 2.0).evaluate(&base_item, &sde), 1.0);
    assert_eq!(bin(DogmaVmOpcode::Gt, 5.0, 5.0).evaluate(&base_item, &sde), 0.0);
    assert_eq!(bin(DogmaVmOpcode::Gt, 6.0, 5.0).evaluate(&base_item, &sde), 1.0);
    assert_eq!(bin(DogmaVmOpcode::Gte, 5.0, 5.0).evaluate(&base_item, &sde), 1.0);
    assert_eq!(bin(DogmaVmOpcode::Gte, 4.9, 5.0).evaluate(&base_item, &sde), 0.0);
    assert_eq!(bin(DogmaVmOpcode::Eq, 5.0, 5.0).evaluate(&base_item, &sde), 1.0);
    assert_eq!(bin(DogmaVmOpcode::Eq, 5.0, 5.0 + 1e-9).evaluate(&base_item, &sde), 0.0);

    let mut zero_req_item = DogmaItemState::from_sde(2, 999_991, &sde);
    zero_req_item.base_attributes.insert(182, 0.0);
    assert!(!item_has_any_required_skill(&zero_req_item, &sde));
    assert!(!item_requires_skill(&zero_req_item, 0, &sde));

    let mut sde_req_type = sde.types[&587].clone();
    sde_req_type.type_id = 999_992;
    sde_req_type.default_attributes.insert(182, 0.0);
    sde.types.insert(999_992, sde_req_type);
    let zero_sde_item = DogmaItemState::from_sde(3, 999_992, &sde);
    assert!(!item_has_any_required_skill(&zero_sde_item, &sde));
    assert!(!item_requires_skill(&zero_sde_item, 0, &sde));

    let mut target = DogmaItemState::from_sde(4, 587, &sde);
    target.base_attributes.insert(182, 0.0);
    let req_attr_node = node(DogmaVmOpcode::Att, None, Some(182), None, None, None);
    let filter = node(DogmaVmOpcode::DefType, None, None, None, Some(Box::new(req_attr_node)), None);
    let req_attr_filter = node(
        DogmaVmOpcode::AddLocSrqMod,
        Some(25.0),
        Some(attr_ids::MAX_VELOCITY),
        Some(ModifierOperator::PostPercent),
        Some(Box::new(filter)),
        None,
    );
    req_attr_filter.execute_on_target_with_effect_and_scale(1, None, &base_item, &mut target, 1.0, &sde);
    assert!(target.modifiers.is_empty());

    let pol = |eid: Option<u16>, op: ModifierOperator, aid: u16, raw: f64| -> f64 {
        let mut t = DogmaItemState::from_sde(5, 587, &sde);
        let n = node(DogmaVmOpcode::AddItmMod, Some(raw), Some(aid), Some(op), None, None);
        n.execute_on_target_with_effect_and_scale(1, eid, &base_item, &mut t, 1.0, &sde);
        t.modifiers.last().map(|m| m.value).unwrap_or(-1.0)
    };
    assert_eq!(pol(Some(5995), ModifierOperator::PostAssign, 267, 100.0), 1.0);
    assert_eq!(pol(Some(5994), ModifierOperator::PostAssign, 267, 100.0), 100.0);
    assert_eq!(pol(Some(5995), ModifierOperator::PreAssign, 267, 100.0), 100.0);
    assert_eq!(pol(Some(5995), ModifierOperator::PostAssign, 109, 100.0), 100.0);
    assert_eq!(pol(Some(5995), ModifierOperator::PostAssign, 267, 100.0 + 2e-6), 100.0 + 2e-6);

    let div_scale = |sc: f64| -> f64 {
        let mut t = DogmaItemState::from_sde(6, 587, &sde);
        let n = node(
            DogmaVmOpcode::AddItmMod,
            Some(49.0),
            Some(attr_ids::MAX_VELOCITY),
            Some(ModifierOperator::PostDiv),
            None,
            None,
        );
        n.execute_on_target_with_effect_and_scale(1, None, &base_item, &mut t, sc, &sde);
        t.modifiers.last().map(|m| m.value).unwrap()
    };
    assert_eq!(div_scale(1.0).to_bits(), 49.0_f64.to_bits());
    assert_eq!(div_scale(1.0 + 5e-10).to_bits(), 49.0_f64.to_bits());
    assert_ne!(div_scale(1.0 + 2e-9).to_bits(), 49.0_f64.to_bits());
    assert_ne!(div_scale(-1.0), 49.0);
    for (name, ids) in [
        ("Pulsar", [30844, 30865, 30866, 30867, 30868, 30869]),
        ("Black Hole", [30845, 30850, 30851, 30852, 30853, 30854]),
        ("Cataclysmic Variable", [30846, 30880, 30881, 30884, 30883, 30882]),
        ("Magnetar", [30847, 30860, 30861, 30862, 30863, 30864]),
        ("Red Giant", [30848, 30870, 30871, 30872, 30873, 30874]),
        ("Wolf-Rayet", [30849, 30875, 30876, 30877, 30878, 30879]),
    ] {
        for (i, &exp) in ids.iter().enumerate() {
            assert_eq!(wormhole_phenomenon_type_for_class_and_name((i + 1) as u8, name), Some(exp));
        }
    }
    let mut wh_beacon = sde.types[&587].clone();
    wh_beacon.type_id = 30844;
    wh_beacon.group_id = 920;
    wh_beacon.effect_ids = vec![48_001, 48_002];
    wh_beacon.default_attributes = HashMap::from([(2001, 50.0), (2002, 20.0)]);
    sde.types.insert(30844, wh_beacon);
    let base_eff = sde.effects.values().next().unwrap().clone();
    let base_mod = sde.effects.values().find_map(|e| e.modifiers.first()).unwrap().clone();
    let wh_eff = |eid: u16, func: &str, aid: u16| {
        let mut e = base_eff.clone();
        e.effect_id = eid;
        let mut m = base_mod.clone();
        m.domain = "shipID".into();
        m.func = String::from(func);
        m.modified_attribute_id = attr_ids::MAX_VELOCITY;
        m.modifying_attribute_id = aid;
        m.operator = 6;
        e.modifiers = vec![m];
        e
    };
    let e1 = wh_eff(48_001, "LocationRequiredSkillModifier", 2001);
    let e2 = wh_eff(48_002, "ItemModifier", 2002);
    sde.effects.insert(48_001, e1);
    sde.effects.insert(48_002, e2);
    let mut wh_attrs: HashMap<u16, f64> = HashMap::from([(attr_ids::MAX_VELOCITY, 100.0)]);
    assert!(apply_wormhole_phenomenon_to_attributes(&sde, 30844, 6, false, &mut wh_attrs));
    assert!((wh_attrs[&attr_ids::MAX_VELOCITY] - 120.0).abs() < 1e-9);
    let mut neut = DogmaItemState::from_sde(10, 587, &sde);
    neut.base_attributes.extend([(97, 80.0), (attr_ids::MAX_RANGE, 10_000.0), (attr_ids::FALLOFF, 5_000.0)]);
    assert!((eval_energy_neutralizer(&neut, 15_000.0, 500.0, &sde).actual_drain - 40.0).abs() < 1e-9);
    let mut nos = DogmaItemState::from_sde(11, 587, &sde);
    nos.base_attributes.extend([(90, 50.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    assert_eq!(eval_energy_nosferatu_with_hull(&nos, 0.0, 300.0, 400.0, 200.0, false, &sde).actual_transferred, 0.0);
    assert!((eval_energy_nosferatu_with_hull(&nos, 0.0, 100.0, 400.0, 200.0, false, &sde).actual_transferred - 50.0).abs() < 1e-9);
}

