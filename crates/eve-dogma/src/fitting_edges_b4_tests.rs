//! Edge-case behavioral tests for `fitting.rs`.

use super::*;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;

fn make_type(
    type_id: TypeId,
    group_id: u32,
    category_id: u32,
    attrs: &[(AttributeId, f64)],
    effects: &[u16],
) -> InvType {
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type{type_id}"),
        mass: 100.0,
        volume: 10.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 0.0,
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
        default_effect_id: None,
        description: String::new(),
        effect_ids: effects.to_vec(),
        default_attributes,
    }
}

fn standard_frigate_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        587,
        make_type(
            587,
            25,
            6,
            &[
                (attr_ids::LOW_SLOTS, 3.0),
                (attr_ids::MED_SLOTS, 3.0),
                (attr_ids::HI_SLOTS, 3.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (attr_ids::TURRET_SLOTS_LEFT, 2.0),
                (attr_ids::LAUNCHER_SLOTS_LEFT, 2.0),
                (attr_ids::RIG_SIZE, 1.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
            ],
            &[],
        ),
    );
    sde
}

#[test]
fn subsystem_slots_refit_burnout_and_unknown_hull_edges() {
    let mut sde = standard_frigate_sde();
    // T3 Cruiser hull (group 963)
    sde.types.insert(
        29984,
        make_type(
            29984,
            963,
            6,
            &[
                (attr_ids::LOW_SLOTS, 2.0),
                (attr_ids::MED_SLOTS, 2.0),
                (attr_ids::HI_SLOTS, 2.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (attr_ids::CPU_OUTPUT, 300.0),
                (attr_ids::POWER_OUTPUT, 300.0),
            ],
            &[],
        ),
    );
    // Subsystem on valid slot 125
    sde.types
        .insert(201, make_type(201, 954, 32, &[(1366, 125.0)], &[3772]));
    // Subsystem with out-of-range slot 129 (slot_idx == 4 >= 4)
    sde.types
        .insert(202, make_type(202, 954, 32, &[(1366, 129.0)], &[3772]));
    // Basic hi-slot module
    sde.types.insert(
        203,
        make_type(
            203,
            53,
            7,
            &[(attr_ids::CPU, 10.0), (attr_ids::POWER, 10.0), (9, 40.0)],
            &[12],
        ),
    );

    // Unknown hull type error
    let mut bad_eng = ShipFittingEngine::from_sde(1, 999_999, &sde);
    assert!(bad_eng
        .fit_module_with_state(10, 203, 27, true, &sde)
        .unwrap_err()
        .contains("Unknown ship hull type"));

    // Subsystem slot 125 succeeds on T3C; slot 129 fails range check (>= 4)
    let mut t3c = ShipFittingEngine::from_sde(2, 29984, &sde);
    assert!(t3c.fit_module_with_state(20, 201, 125, true, &sde).is_ok());
    let err = t3c.fit_module_with_state(21, 202, 129, true, &sde).unwrap_err();
    assert!(err.contains("exceeds hull slot capacity (4)"), "got {err}");

    // Refitting an already-fitted item preserves damage_hp & max_hp; burned-out module can refit offline only
    let mut eng = ShipFittingEngine::from_sde(3, 587, &sde);
    eng.fit_module_with_state(30, 203, 27, true, &sde).unwrap();
    eng.modules.get_mut(&30).unwrap().damage_hp = 40.0;
    eng.modules.get_mut(&30).unwrap().max_hp = 40.0;
    let err = eng.fit_module_with_state(30, 203, 27, true, &sde).unwrap_err();
    assert!(err.contains("ModuleBurnedOut"), "got {err}");
    assert!(eng.fit_module_with_state(30, 203, 27, false, &sde).is_ok());
    assert_eq!(eng.modules[&30].state, ModuleState::Offline);
    assert_eq!(eng.modules[&30].damage_hp, 40.0);
}

#[test]
fn group_type_rig_online_limits_ship_type_fit_and_skill_tiers_2_to_6() {
    let mut sde = standard_frigate_sde();
    // Module group 60 with only attr 1544 (maxGroupFitted = 1) and NO attr 1548
    sde.types
        .insert(301, make_type(301, 60, 7, &[(1544, 1.0)], &[11]));
    // Module group 61 with maxTypeFitted = 1 (attr 2431), allowing a second distinct type in same group
    sde.types
        .insert(302, make_type(302, 61, 7, &[(2431, 1.0)], &[11]));
    sde.types
        .insert(303, make_type(303, 61, 7, &[(2431, 1.0)], &[11]));
    // Propulsion group 46 WITHOUT attr 978 (defaults to 1.0 maxGroupOnline)
    sde.types.insert(304, make_type(304, 46, 7, &[], &[13]));
    // Module in group 1770 with attr 978 = 1.0 + rig with effect 6766 & attr 2542 = 1.0
    sde.types
        .insert(305, make_type(305, 1770, 7, &[(978, 1.0)], &[13]));
    sde.types.insert(
        306,
        make_type(
            306,
            775,
            7,
            &[(attr_ids::RIG_SIZE, 1.0), (attr_ids::UPGRADE_COST, 50.0), (2542, 1.0)],
            &[2663, 6766],
        ),
    );
    // Hull-specific module restricted via canFitShipType1 (1302 = 587)
    sde.types
        .insert(307, make_type(307, 62, 7, &[(1298, 999.0), (1302, 587.0)], &[11]));
    // Skill slots 2..=6 check (attrs 183/278, 184/279, 1285/1286, 1289/1287, 1290/1288)
    let skill_pairs = [
        (183, 278, 3302_u32),
        (184, 279, 3303_u32),
        (1285, 1286, 3304_u32),
        (1289, 1287, 3305_u32),
        (1290, 1288, 3306_u32),
    ];
    for (idx, &(req_attr, lvl_attr, skill_id)) in skill_pairs.iter().enumerate() {
        sde.types.insert(
            310 + idx as u32,
            make_type(
                310 + idx as u32,
                63,
                7,
                &[(req_attr, skill_id as f64), (lvl_attr, 2.0)],
                &[11],
            ),
        );
    }

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);

    // maxGroupFitted via 1544 alone
    assert!(eng.fit_module_with_state(1, 301, 11, true, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(2, 301, 12, true, &sde)
        .unwrap_err()
        .contains("MaxGroupFittedExceeded"));

    // maxTypeFitted via 2431 allows distinct type in same group, blocks duplicate type
    assert!(eng.fit_module_with_state(3, 302, 13, true, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(4, 302, 12, true, &sde)
        .unwrap_err()
        .contains("MaxTypeFittedExceeded"));
    assert!(eng.fit_module_with_state(5, 303, 12, true, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(3, 302, 13, true, &sde)
        .is_ok()); // refit same item in same slot ok

    // Clean engine for propmod fallback (46) & group 1770 + effect 6766 rig bonus tests
    let mut eng = ShipFittingEngine::from_sde(2, 587, &sde);
    assert!(eng.fit_module_with_state(10, 304, 19, true, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(11, 304, 20, true, &sde)
        .unwrap_err()
        .contains("MaxGroupOnlineExceeded"));

    // Group 1770 online limit increases by rig effect 6766 (+attr 2542) only when rig is online
    assert!(eng.fit_module_with_state(12, 305, 20, true, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(13, 305, 21, true, &sde)
        .unwrap_err()
        .contains("MaxGroupOnlineExceeded"));
    assert!(eng.fit_module_with_state(13, 305, 21, false, &sde).is_ok());
    assert!(eng
        .set_module_online(13, &sde)
        .unwrap_err()
        .contains("MaxGroupOnlineExceeded"));

    // Fit rig 306 OFFLINE -> still blocks; online rig 306 -> allows 2nd group 1770 online
    assert!(eng.fit_module_with_state(14, 306, 92, false, &sde).is_ok());
    assert!(eng.set_module_online(13, &sde).is_err());
    assert!(eng.set_module_online(14, &sde).is_ok());
    assert!(eng.set_module_online(13, &sde).is_ok());

    // canFitShipType1 (1302) overrides non-matching group restriction (1298)
    assert!(eng.fit_module_with_state(20, 307, 11, true, &sde).is_ok());

    // Missing any of REQUIRED_SKILL_2..=6 rejects, training to 2 allows each
    let mut learned = HashMap::new();
    for (idx, &(_, _, skill_id)) in skill_pairs.iter().enumerate() {
        let tid = 310 + idx as u32;
        assert!(eng
            .fit_module_with_state(30, tid, 12, true, &sde)
            .unwrap_err()
            .contains(&format!("skill {skill_id} level 2")));
        learned.insert(skill_id, 2_u8);
        eng.set_character_skills(learned.clone());
        assert!(eng.fit_module_with_state(30, tid, 12, true, &sde).is_ok());
        eng.unfit_module(30, &sde).unwrap();
    }
}

#[test]
fn rollback_and_priority_auto_offline_order_and_subsystem_unfit_guard() {
    let mut sde = standard_frigate_sde();
    // Rig costing 250 calibration (2nd exceeds 400 even with online=false)
    sde.types.insert(
        401,
        make_type(
            401,
            700,
            7,
            &[(attr_ids::RIG_SIZE, 1.0), (attr_ids::UPGRADE_COST, 250.0)],
            &[2663],
        ),
    );
    // High-cost CPU module (40 CPU, 10 PG), low-cost CPU module (30 CPU, 5 PG), med module (30 CPU, 5 PG), low module (40 CPU, 40 PG)
    sde.types.insert(
        402,
        make_type(402, 53, 7, &[(attr_ids::CPU, 40.0), (attr_ids::POWER, 10.0)], &[12]),
    );
    sde.types.insert(
        403,
        make_type(403, 53, 7, &[(attr_ids::CPU, 30.0), (attr_ids::POWER, 5.0)], &[12]),
    );
    sde.types.insert(
        404,
        make_type(404, 65, 7, &[(attr_ids::CPU, 30.0), (attr_ids::POWER, 5.0)], &[13]),
    );
    sde.types.insert(
        405,
        make_type(405, 62, 7, &[(attr_ids::CPU, 40.0), (attr_ids::POWER, 40.0)], &[11]),
    );
    // Heavy CPU/PG module to trigger rollback (needs 200 CPU / 200 PG)
    sde.types.insert(
        406,
        make_type(406, 53, 7, &[(attr_ids::CPU, 200.0), (attr_ids::POWER, 5.0)], &[12]),
    );
    sde.types.insert(
        407,
        make_type(407, 53, 7, &[(attr_ids::CPU, 5.0), (attr_ids::POWER, 200.0)], &[12]),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    assert!(eng.fit_module_with_state(1, 401, 92, false, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(2, 401, 93, false, &sde)
        .unwrap_err()
        .contains("Insufficient Calibration"));

    // CPU & PG rollback when refitting existing module preserves prior entry
    assert!(eng.fit_module_with_state(10, 402, 27, true, &sde).is_ok());
    assert!(eng
        .fit_module_with_state(10, 406, 27, true, &sde)
        .unwrap_err()
        .contains("Insufficient CPU"));
    assert_eq!(eng.modules[&10].type_id, 402);
    assert_eq!(eng.modules[&10].state, ModuleState::Online);

    assert!(eng
        .fit_module_with_state(10, 407, 27, true, &sde)
        .unwrap_err()
        .contains("Insufficient Powergrid"));
    assert_eq!(eng.modules[&10].type_id, 402);

    // set_module_online rollback restores Offline state on insufficient CPU/PG
    assert!(eng.fit_module_with_state(11, 406, 28, false, &sde).is_ok());
    assert!(eng.set_module_online(11, &sde).is_err());
    assert_eq!(eng.modules[&11].state, ModuleState::Offline);
    eng.unfit_module(11, &sde).unwrap();

    // Priority auto-offline ordering: High before Med before Low, higher cpu+pg first
    // Fit two High (402: 50 load on id=10, 403: 35 load on id=12 -> total 85) + Med (404: 35 on id=13 -> total 115? wait ship has 100 CPU)
    // Bump ship CPU temporarily to fit all, then reduce ship CPU dynamically via hull attribute override / skill change or offline rig!
    // Even simpler: use ship skill / T3C or adjust hull CPU output in sde before calling enforce_post_unfit_or_offline_constraints!
    eng.unfit_module(10, &sde).unwrap();
    eng.unfit_module(1, &sde).unwrap();
    eng.ship.base_attributes.insert(attr_ids::CPU_OUTPUT, 200.0);
    eng.ship.base_attributes.insert(attr_ids::POWER_OUTPUT, 200.0);
    eng.recalculate_stats(&sde);
    eng.fit_module_with_state(100, 403, 27, true, &sde).unwrap(); // Hi, 30+5=35
    eng.fit_module_with_state(101, 402, 28, true, &sde).unwrap(); // Hi, 40+10=50 (higher load)
    eng.fit_module_with_state(102, 404, 19, true, &sde).unwrap(); // Med, 30+5=35
    eng.fit_module_with_state(103, 405, 11, true, &sde).unwrap(); // Low, 40+40=80
    eng.fit_module_with_state(104, 401, 92, true, &sde).unwrap(); // Rig (never auto-offlined)

    // Total CPU = 30+40+30+40 = 140. Reduce CPU_OUTPUT to 75 -> must offline Hi#101 (40), then Hi#100 (30) -> 70 remaining <= 75!
    eng.ship.base_attributes.insert(attr_ids::CPU_OUTPUT, 75.0);
    eng.recalculate_stats(&sde);
    let offlined = eng.enforce_post_unfit_or_offline_constraints(&sde);
    assert_eq!(offlined, vec![101, 100]);
    assert_eq!(eng.modules[&102].state, ModuleState::Online);
    assert_eq!(eng.modules[&103].state, ModuleState::Online);
    assert_eq!(eng.modules[&104].state, ModuleState::Online);

    // Further reduce CPU_OUTPUT to 10 -> Med#102 offlined next, then Low#103, Rig#104 stays Online and loop terminates cleanly
    eng.ship.base_attributes.insert(attr_ids::CPU_OUTPUT, 10.0);
    eng.recalculate_stats(&sde);
    let offlined2 = eng.enforce_post_unfit_or_offline_constraints(&sde);
    assert_eq!(offlined2, vec![102, 103]);
    assert_eq!(eng.modules[&104].state, ModuleState::Online);

    // Subsystem unfit guard on Med and High slot overflow, plus clean unfit when slots are free
    sde.types.insert(
        29984,
        make_type(
            29984,
            963,
            6,
            &[(attr_ids::LOW_SLOTS, 2.0), (attr_ids::MED_SLOTS, 2.0), (attr_ids::HI_SLOTS, 2.0)],
            &[],
        ),
    );
    // Subsystem adding +1 hi slot and +1 med slot (via modifiers? Actually default hull has 2+0 = 2, let's fit at slot index >= hull_base_slots)
    // Wait: subsystem without modifier yields hull slot count = 2, fitting into slot 29 (hi idx 2) only works if subsystem modifier raises hiSlots.
    // Test direct check: place a med module at flag 21 (idx 2 >= base med_slots 2) and unfit a subsystem -> rejects!
    sde.types.insert(501, make_type(501, 954, 32, &[(1366, 125.0)], &[3772]));
    let mut t3 = ShipFittingEngine::from_sde(10, 29984, &sde);
    t3.fit_module_with_state(200, 501, 125, true, &sde).unwrap();
    // Simulate a fitted med module at flag 21 (med slot index 2 >= 2)
    t3.modules.insert(
        201,
        FittedModule {
            item_id: 201,
            type_id: 404,
            flag_id: 21,
            state: ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    assert!(t3
        .unfit_module(200, &sde)
        .unwrap_err()
        .contains("SubsystemHasFittedModules"));
    t3.modules.remove(&201);
    // Simulate a fitted high module at flag 29 (hi slot index 2 >= 2)
    t3.modules.insert(
        202,
        FittedModule {
            item_id: 202,
            type_id: 403,
            flag_id: 29,
            state: ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    assert!(t3
        .unfit_module(200, &sde)
        .unwrap_err()
        .contains("SubsystemHasFittedModules"));
    t3.modules.remove(&202);
    assert!(t3.unfit_module(200, &sde).is_ok());
}

#[test]
fn heat_racks_abyssal_roll_and_t3d_stance_switch_edges() {
    let mut sde = standard_frigate_sde();
    // Overloaded module in Med rack (19) with NO heatDamage attr (defaults 5.0) + adjacent module near burnout
    sde.types
        .insert(601, make_type(601, 65, 7, &[(9, 40.0)], &[13]));
    sde.types
        .insert(602, make_type(602, 62, 7, &[(9, 40.0)], &[11]));
    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.fit_module_with_state(1, 601, 19, true, &sde).unwrap();
    eng.fit_module_with_state(2, 601, 20, true, &sde).unwrap();
    eng.modules.get_mut(&1).unwrap().state = ModuleState::Overloaded;
    eng.modules.get_mut(&2).unwrap().damage_hp = 38.0; // within 2 HP of max_hp=40
    eng.fit_module_with_state(3, 602, 11, true, &sde).unwrap();
    eng.modules.get_mut(&3).unwrap().state = ModuleState::Overloaded;

    // roll_u = 0.0 ensures adjacent roll hits; heat_gen_rate=0.5, dissipation=0.0, duration=1.0
    let report = eng.apply_heat_cycle(1, 0.5, 0.0, 1.0, 0.0, &sde).unwrap();
    assert!(report.rack_heat > 0.0);
    assert!((report.module_damage - 5.0).abs() < 1e-6); // default 5.0 without attr 1211
    assert!(!report.module_burned);
    assert!(eng.rack_heat[1] > 0.0);
    assert_eq!(eng.rack_heat[2], 0.0); // low rack untouched by med cycle
    assert_eq!(eng.modules[&2].damage_hp, 40.0);
    assert_eq!(eng.modules[&2].state, ModuleState::Offline);
    assert!(report.adjacent_damaged_modules.iter().any(|&(id, _, burned)| id == 2 && burned));

    // Low rack heat step
    let report2 = eng.apply_heat_cycle(3, 0.4, 0.0, 1.0, 0.0, &sde).unwrap();
    assert!(report2.rack_heat > 0.0);
    assert!(eng.rack_heat[2] > 0.0);

    // Abyssal rolls clamp out-of-range u values and leave un-ranged attributes untouched
    let base = HashMap::from([(10_u16, 100.0), (20_u16, 50.0)]);
    let ranges = HashMap::from([(10_u16, [0.8_f64, 1.2_f64]), (99_u16, [0.5, 1.5])]);
    let rolls = HashMap::from([(10_u16, 2.5_f64)]); // clamps to 1.0 -> 120.0
    let rolled = roll_abyssal_attributes(&base, &ranges, &rolls);
    assert!((rolled[&10] - 120.0).abs() < 1e-9);
    assert_eq!(rolled[&20], 50.0);
    assert!(!rolled.contains_key(&99));

    let rolls_neg = HashMap::from([(10_u16, -1.0_f64)]); // clamps to 0.0 -> 80.0
    assert!((roll_abyssal_attributes(&base, &ranges, &rolls_neg)[&10] - 80.0).abs() < 1e-9);

    // T3D stance switching validation, same-stance cooldown bypass, and exact cooldown boundary
    sde.types.insert(34317, make_type(34317, 1305, 6, &[(1985, 10_000.0)], &[])); // Confessor
    sde.types.insert(34319, make_type(34319, 1306, 6, &[], &[])); // Defense
    sde.types.insert(34321, make_type(34321, 1306, 6, &[], &[])); // Sniper
    sde.types.insert(34564, make_type(34564, 1306, 6, &[], &[])); // Svipul stance (wrong hull)
    sde.types.insert(99000, make_type(99000, 999, 6, &[], &[])); // Wrong group

    // Non-T3D fails
    assert!(eng.switch_ship_stance(34319, 1_000, &sde).unwrap_err().contains("not a Tactical Destroyer"));

    let mut conf = ShipFittingEngine::from_sde(10, 34317, &sde);
    assert!(conf.switch_ship_stance(888_888, 1_000, &sde).unwrap_err().contains("Unknown stance"));
    assert!(conf.switch_ship_stance(99000, 1_000, &sde).unwrap_err().contains("not a Tactical Destroyer stance"));
    assert!(conf.switch_ship_stance(34564, 1_000, &sde).unwrap_err().contains("does not belong to hull"));

    assert_eq!(conf.switch_ship_stance(34319, 1_000, &sde).unwrap(), 34319);
    // Re-selecting the SAME active stance within cooldown is allowed
    assert_eq!(conf.switch_ship_stance(34319, 2_000, &sde).unwrap(), 34319);
    // Switching to another stance at 9,999 ms elapsed fails with 1 ms remaining
    let cd_err = conf.switch_ship_stance(34321, 11_999, &sde).unwrap_err();
    assert!(cd_err.contains("1ms remaining"), "got {cd_err}");
    // Exact boundary elapsed == 10,000 ms succeeds
    assert_eq!(conf.switch_ship_stance(34321, 12_000, &sde).unwrap(), 34321);
}
