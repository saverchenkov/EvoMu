pub(super) use super::super::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use super::*;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
#[path = "combat_npcs_drones_control_tests.rs"]
mod combat_npcs_drones_control_tests;

#[cfg(test)]
#[path = "combat_npcs_drones_cycle_ops_cov_tests.rs"]
mod combat_npcs_drones_cycle_ops_cov_tests;

pub(super) fn py_dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

pub(super) fn seed_pilot_skill(ctx: &ServerContext, char_id: i64, skill_type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.values().next().cloned())
            .expect("seed character has skills");
        let mut sk = template;
        sk.type_id = skill_type_id;
        sk.level = level;
        snap.skills
            .entry(char_id)
            .or_default()
            .insert(skill_type_id, sk);
    });
}

pub(super) fn make_drone_test_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    // Ship 587 (Rifter/cruiser stand-in): drone bandwidth 1271 = 50, drone bay cap 283 = 100, control range 458 = 20_000
    sde.types.insert(
        587,
        make_fighter_type(
            587,
            25,
            6,
            10_000.0,
            vec![(1271, 50.0), (283, 100.0), (458, 20_000.0)],
        ),
    );
    // Rorqual 28352 with excavator bonus attr 3223 = 1.0 and 125 bandwidth
    sde.types.insert(
        28_352,
        make_fighter_type(
            28_352,
            883,
            6,
            1_000_000.0,
            vec![(1271, 125.0), (283, 500.0), (3223, 1.0)],
        ),
    );
    // Light combat drone 2456 (Hobgoblin II: cat 18, group 100, bw 5, vol 5, vel 2600, opt 2000, falloff 3000, tracking 2.2, therm 24, mult 1.5, cycle 4000ms)
    sde.types.insert(
        2_456,
        make_fighter_type(
            2_456,
            100,
            18,
            5.0,
            vec![
                (1272, 5.0),
                (37, 2_600.0),
                (54, 2_000.0),
                (158, 3_000.0),
                (160, 2.2),
                (64, 1.5),
                (118, 24.0),
                (51, 4_000.0),
            ],
        ),
    );
    // Heavy combat drone 2444 (Ogre II: bw 25, vol 25)
    sde.types.insert(
        2_444,
        make_fighter_type(
            2_444,
            100,
            18,
            25.0,
            vec![(1272, 25.0), (37, 1_100.0), (54, 4_000.0), (118, 60.0)],
        ),
    );
    // Sentry drone 23561 (Garde II: vel 0 -> is_sentry = true, bw 25, opt 45_000, therm 50)
    sde.types.insert(
        23_561,
        make_fighter_type(
            23_561,
            100,
            18,
            25.0,
            vec![
                (1272, 25.0),
                (37, 0.0),
                (54, 45_000.0),
                (158, 15_000.0),
                (118, 50.0),
            ],
        ),
    );
    // Logistic drone 3186 (Maintenance Bot: cat 18, group 640, bw 10, vol 10, shield_rep 68 = 45.0, armor_rep 84 = 35.0)
    sde.types.insert(
        3_186,
        make_fighter_type(
            3_186,
            640,
            18,
            10.0,
            vec![(1272, 10.0), (37, 2_000.0), (54, 10_000.0), (68, 45.0), (84, 35.0)],
        ),
    );
    // EWAR drone 23707 (Hornet EC-300: cat 18, group 639, bw 5, vol 5, ecm 238 = 15.0 for deterministic unit test)
    sde.types.insert(
        23_707,
        make_fighter_type(
            23_707,
            639,
            18,
            5.0,
            vec![(1272, 5.0), (37, 2_400.0), (54, 15_000.0), (238, 15.0)],
        ),
    );
    // Stasis Webifier EWAR drone 23711 (group 641, web -35%, neut 60, tp +25%)
    sde.types.insert(
        23_711,
        make_fighter_type(
            23_711,
            641,
            18,
            5.0,
            vec![
                (1272, 5.0),
                (37, 2_400.0),
                (54, 15_000.0),
                (20, -35.0),
                (97, 60.0),
                (554, 25.0),
            ],
        ),
    );
    // Excavator Mining Drone 41030 (cat 18, group 101, bw 25, vol 750)
    sde.types.insert(
        41_030,
        make_fighter_type(41_030, 101, 18, 750.0, vec![(1272, 25.0), (37, 600.0)]),
    );
    // Salvage Drone 32787 (cat 18, group 1159, bw 5, vol 5)
    sde.types.insert(
        32_787,
        make_fighter_type(32_787, 1159, 18, 5.0, vec![(1272, 5.0), (37, 1_800.0)]),
    );
    // Tractor Beam module (cat 7, group 650, maxRange 54 = 24_000, tractorVel 624 = 1_500, duration 73 = 5000ms)
    sde.types.insert(
        1_222,
        make_fighter_type(
            1_222,
            650,
            7,
            5.0,
            vec![(54, 24_000.0), (624, 1_500.0), (73, 5_000.0)],
        ),
    );
    // Salvager I module (cat 7, group 1122, maxRange 54 = 6_000)
    sde.types.insert(
        25_861,
        make_fighter_type(25_861, 1122, 7, 5.0, vec![(54, 6_000.0)]),
    );
    // Wreck item type 26470 (cat 2, group 186)
    sde.types.insert(
        26_470,
        make_fighter_type(26_470, 186, 2, 100.0, vec![]),
    );

    ctx.sde = Arc::new(sde);
    seed_pilot_skill(&ctx, char_id, 3_436, 5); // Drones V -> max 5 drones
    seed_pilot_skill(&ctx, char_id, 3_437, 4); // Drone Avionics IV (+20km)
    seed_pilot_skill(&ctx, char_id, 23_618, 2); // Advanced Drone Avionics II (+6km)

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 587;
        }
    });
    (ctx, session, char_id, ship_id, sys_id)
}

#[test]
fn drone_control_range_capacity_and_hit_multiplier_math() {
    let (ctx, _, char_id, ship_id, sys_id) = make_drone_test_ctx();
    // 20,000 + 5,000*4 + 3,000*2 = 46,000 m
    let range = compute_drone_control_range(&ctx, char_id, ship_id);
    assert!((range - 46_000.0).abs() < 1e-6);

    // Normal ship capacity without Triage: (5 drones, 50 Mbit/s, 100 m3)
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let (max_d, bw, cap) = pilot_drone_capacity(&ctx, char_id, ship_id, Some(&fit));
    assert_eq!(max_d, 5);
    assert!((bw - 50.0).abs() < 1e-6);
    assert!((cap - 100.0).abs() < 1e-6);

    // Active Triage module (27951) zeroes ship drone bandwidth
    let mut triage_fit = fit.clone();
    triage_fit.modules.insert(
        700_050,
        eve_dogma::FittedModule {
            item_id: 700_050,
            type_id: 27_951,
            flag_id: 27,
            state: eve_dogma::ModuleState::Active,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    let (_, triage_bw, _) = pilot_drone_capacity(&ctx, char_id, ship_id, Some(&triage_fit));
    assert_eq!(triage_bw, 0.0);

    // Drone hit multiplier: point-blank stationary vs fast/far transversal target
    ctx.db.with_mut_snapshot(|snap| {
        let mut d = make_fighter_item(700_060, 2_456, char_id, sys_id as i64, InventoryFlag::None, 1);
        d.dynamic_attributes.insert(160, 2.5);
        d.dynamic_attributes.insert(158, 3_000.0);
        d.dynamic_attributes.insert(51, 3_000.0);
        snap.items.insert(700_060, d);
    });
    let st = DroneSpaceState {
        ball_id: 900_060,
        drone_item_id: 700_060,
        type_id: 2_456,
        group_id: 100,
        owner_char_id: char_id,
        original_owner_char_id: char_id,
        parent_ship_id: ship_id,
        solar_system_id: sys_id,
        target_id: None,
        activity_state: 0,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 2_600.0,
        optimal_range: 2_000.0,
        damage_vector: DamageVector::new(0.0, 36.0, 0.0, 0.0),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };
    assert_eq!(st.cycle_duration_secs(&ctx), 3);
    let b_drone = Ball {
        id: 900_060,
        type_id: 2_456,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 2_600.0,
        inertia: 0.5,
        mass: 5_000.0,
        radius: 10.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let b_close = Ball {
        id: 300_010,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mult_close = st.compute_combat_hit_multiplier(&ctx, &b_drone, &b_close, 40.0);
    assert!((mult_close - 1.0).abs() < 1e-6);

    let b_fast_far = Ball {
        id: 300_011,
        type_id: 587,
        position: DVec3::new(9_000.0, 0.0, 0.0),
        velocity: DVec3::new(0.0, 4_500.0, 0.0),
        max_velocity: 4_500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 25.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let mult_far = st.compute_combat_hit_multiplier(&ctx, &b_drone, &b_fast_far, 25.0);
    assert!(mult_far < 0.25 && mult_far >= 0.05);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_launch_stack_split_sentry_and_validation_errors() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    // Seed: stack of 3 Hobgoblins (700_001), 1 Garde sentry (700_002), 1 Excavator (700_003), 1 Salvager mod (700_004), 2 Ogre IIs (700_005)
    ctx.db.with_mut_snapshot(|snap| {
        let mut stack_item = make_fighter_item(700_001, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 3);
        stack_item.dynamic_attributes.insert(1272, 5.0);
        stack_item.dynamic_attributes.insert(263, 180.0);
        snap.items.insert(700_001, stack_item);
        snap.items.insert(700_002, make_fighter_item(700_002, 23_561, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_003, make_fighter_item(700_003, 41_030, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_004, make_fighter_item(700_004, 25_861, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_005, make_fighter_item(700_005, 2_444, char_id, ship_id, InventoryFlag::DroneBay, 1));
    });

    // 1. Non-drone item -> NotADrone
    let err = handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(700_004)], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NotADrone");

    // 2. Excavator on non-capital ship -> InvalidShipForExcavator
    let err = handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(700_003)], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "InvalidShipForExcavator");

    // 3. Docked rejection -> CantLaunchDronesWhileDocked
    let mut docked = session.clone();
    docked.station_id = Some(60_003_760);
    let err = handle_npcs_drones(&ctx, &mut docked, "ship", "LaunchDrones", &[PyRep::Integer(700_001)], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "CantLaunchDronesWhileDocked");

    // 4. Launch stack of 3 Hobgoblins (pass ID 3x to test dedup + stack split + Garde sentry)
    let res = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(700_001),
            PyRep::Integer(700_001),
            PyRep::Integer(700_002),
        ])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(launched) = res else { panic!("expected list") };
    assert_eq!(launched.len(), 2);
    // Original stack quantity reduced from 3 to 2 in DroneBay
    let rem_qty = ctx.db.with_snapshot(|s| s.items.get(&700_001).map(|i| i.quantity));
    assert_eq!(rem_qty, Some(2));
    // Sentry ball has speed_fraction == 0.0
    let sentry_bid = launched[1].as_i64().unwrap();
    assert_eq!(sys_handle.get_ball_snapshot(sentry_bid).await.unwrap().unwrap().1.speed_fraction, 0.0);

    // 5. Bandwidth exceeded when launching two 25-Mbit Ogre IIs on top of 30 Mbit already out
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(700_006, make_fighter_item(700_006, 2_444, char_id, ship_id, InventoryFlag::DroneBay, 1));
    });
    let err_bw = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(700_005), PyRep::Integer(700_006)])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bw.code, "MaxBandwidthExceeded");

    // 6. Non-drone ship RPC method returns None
    assert!(handle_npcs_drones(&ctx, &mut session, "ship", "GetShipConfiguration", &[], &kwargs).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_link_augmentor_omnidirectional_tracking_and_bonused_tractor_beam_via_dogma() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let mut sde = (*ctx.sde).clone();

    if let Some(hull) = sde.types.get_mut(&587) {
        hull.default_attributes.insert(eve_sde::attr_ids::HI_SLOTS, 8.0);
        hull.default_attributes.insert(eve_sde::attr_ids::MED_SLOTS, 8.0);
        hull.default_attributes.insert(eve_sde::attr_ids::LOW_SLOTS, 8.0);
        hull.default_attributes.insert(eve_sde::attr_ids::RIG_SLOTS, 3.0);
        hull.default_attributes.insert(eve_sde::attr_ids::CPU_OUTPUT, 500.0);
        hull.default_attributes.insert(eve_sde::attr_ids::POWER_OUTPUT, 500.0);
        hull.default_attributes.insert(eve_sde::attr_ids::UPGRADE_CAPACITY, 400.0);
        hull.default_attributes.insert(eve_sde::attr_ids::RIG_SIZE, 1.0);
    }

    // Drone Avionics (3437) and Advanced Drone Avionics (23566) skills (+5km/lvl and +3km/lvl to charID attr 458)
    sde.effects.insert(
        448,
        eve_sde::DogmaEffectType {
            effect_id: 448,
            name: "scoutDroneOperationSkillBoostDroneRangeBonus".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "itemID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 459,
                modifying_attribute_id: 280,
                operator: 0,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        504,
        eve_sde::DogmaEffectType {
            effect_id: 504,
            name: "scoutDroneOperationDroneRangeBonusModAddDroneControlDistanceChar".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "charID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 458,
                modifying_attribute_id: 459,
                operator: 2,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    let mut da_skill = make_fighter_type(3_437, 273, 16, 0.01, vec![(459, 5_000.0)]);
    da_skill.effect_ids = vec![448, 504];
    sde.types.insert(3_437, da_skill);
    let mut ada_skill = make_fighter_type(23_566, 273, 16, 0.01, vec![(459, 3_000.0)]);
    ada_skill.effect_ids = vec![448, 504];
    sde.types.insert(23_566, ada_skill);

    // Drone Link Augmentor I (type 4405, online effect 9201 adding +20,000 m to charID attr 458)
    sde.effects.insert(
        9201,
        eve_sde::DogmaEffectType {
            effect_id: 9201,
            name: "droneLinkAug".into(),
            effect_category: 4,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "charID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 458,
                modifying_attribute_id: 459,
                operator: 2,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    let mut dla = make_fighter_type(4_405, 646, 7, 5.0, vec![(30, 1.0), (50, 15.0), (459, 20_000.0)]);
    dla.effect_ids = vec![12, 16, 9201];
    sde.types.insert(4_405, dla);

    // Omnidirectional Tracking Enhancer (type 23579, passive effect 9202 +25% tracking 160 & falloff 158 on drones requiring 3436)
    sde.effects.insert(
        9202,
        eve_sde::DogmaEffectType {
            effect_id: 9202,
            name: "omniTrackingEnh".into(),
            effect_category: 0,
            modifiers: vec![
                eve_sde::EffectModifierSpec {
                    domain: "charID".into(),
                    func: "OwnerRequiredSkillModifier".into(),
                    modified_attribute_id: 160,
                    modifying_attribute_id: 767,
                    operator: 6,
                    group_id: None,
                    skill_type_id: Some(3436),
                    effect_id: None,
                },
                eve_sde::EffectModifierSpec {
                    domain: "charID".into(),
                    func: "OwnerRequiredSkillModifier".into(),
                    modified_attribute_id: 158,
                    modifying_attribute_id: 349,
                    operator: 6,
                    group_id: None,
                    skill_type_id: Some(3436),
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    let mut omni = make_fighter_type(23_579, 646, 7, 5.0, vec![(30, 1.0), (50, 10.0), (767, 25.0), (349, 25.0)]);
    omni.effect_ids = vec![11, 16, 9202];
    sde.types.insert(23_579, omni);
    if let Some(dt) = sde.types.get_mut(&2_456) {
        dt.default_attributes.insert(160, 2.0);
        dt.default_attributes.insert(158, 4_000.0);
        dt.default_attributes.insert(182, 3436.0);
    }

    // Noctis-style Tractor Beam bonus rig (type 92_650, group 650 tractor range +100% & 1045 tractor vel +100%)
    sde.effects.insert(
        9203,
        eve_sde::DogmaEffectType {
            effect_id: 9203,
            name: "tractorHullBonus".into(),
            effect_category: 0,
            modifiers: vec![
                eve_sde::EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "LocationGroupModifier".into(),
                    modified_attribute_id: 54,
                    modifying_attribute_id: 1670,
                    operator: 6,
                    group_id: Some(650),
                    skill_type_id: None,
                    effect_id: None,
                },
                eve_sde::EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "LocationGroupModifier".into(),
                    modified_attribute_id: 1045,
                    modifying_attribute_id: 1670,
                    operator: 6,
                    group_id: Some(650),
                    skill_type_id: None,
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    let mut rig = make_fighter_type(92_650, 782, 7, 5.0, vec![(1153, 50.0), (1670, 100.0), (1547, 1.0)]);
    rig.effect_ids = vec![2663, 9203];
    sde.types.insert(92_650, rig);

    let mut tractor = make_fighter_type(24_348, 650, 7, 5.0, vec![(30, 5.0), (50, 15.0), (54, 20_000.0), (73, 5_000.0), (1045, 500.0)]);
    tractor.effect_ids = vec![12, 16];
    sde.types.insert(24_348, tractor);
    ctx.sde = Arc::new(sde);

    seed_pilot_skill(&ctx, char_id, 3437, 4); // Drone Avionics IV (+20,000 m)
    seed_pilot_skill(&ctx, char_id, 23566, 2); // Advanced Drone Avionics II (+6,000 m)

    let dla_id = 710_001i64;
    let omni_id = 710_002i64;
    let rig_id = 710_003i64;
    let tractor_id = 710_004i64;
    let wreck_in_id = 710_010i64;
    let wreck_far_id = 710_011i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(eve_sde::attr_ids::UPGRADE_CAPACITY, 400.0);
        }
        snap.items.insert(dla_id, make_fighter_item(dla_id, 4_405, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(tractor_id, make_fighter_item(tractor_id, 24_348, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(omni_id, make_fighter_item(omni_id, 23_579, char_id, ship_id, InventoryFlag::LoSlot0, 1));
        snap.items.insert(rig_id, make_fighter_item(rig_id, 92_650, char_id, ship_id, InventoryFlag::RigSlot0, 1));
        snap.items.insert(wreck_in_id, make_fighter_item(wreck_in_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(wreck_far_id, make_fighter_item(wreck_far_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1));
        for mid in [dla_id, tractor_id, omni_id, rig_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Drone control range includes base 20km + DA IV (20km) + ADA II (6km) + online DLA (20km) = 66,000 m!
    let ctrl_range = compute_drone_control_range(&ctx, char_id, ship_id);
    assert!((ctrl_range - 66_000.0).abs() < 1e-6, "Expected 66,000 m, got {ctrl_range}");

    // Negative check: offlining DLA drops control range by 20,000 m -> 46,000 m
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(dla_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 46_000.0).abs() < 1e-6);

    // 2. DroneSpaceState evaluates tracking (2.0 * 1.25 = 2.5) and falloff (4000 * 1.25 = 5000) via ShipFittingEngine
    let d_state = DroneSpaceState {
        ball_id: 910_001,
        drone_item_id: 910_002,
        type_id: 2_456,
        group_id: 100,
        owner_char_id: char_id,
        original_owner_char_id: char_id,
        parent_ship_id: ship_id,
        solar_system_id: sys_id,
        target_id: None,
        activity_state: 1,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 2_600.0,
        optimal_range: 2_000.0,
        damage_vector: DamageVector::new(0.0, 30.0, 0.0, 0.0),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };
    assert!((d_state.effective_tracking_speed(&ctx) - 2.5).abs() < 1e-9);
    assert!((d_state.effective_falloff_range(&ctx) - 5_000.0).abs() < 1e-9);
    assert!((d_state.effective_optimal_range(&ctx) - 2_000.0).abs() < 1e-9);
    assert_eq!(d_state.cycle_duration_secs(&ctx), 4);

    let mut custom_opt_drone = d_state.clone();
    custom_opt_drone.optimal_range = 12_345.0;
    assert!((custom_opt_drone.effective_optimal_range(&ctx) - 12_345.0).abs() < 1e-9);
    custom_opt_drone.optimal_range = 100.0;
    assert!((custom_opt_drone.effective_optimal_range(&ctx) - 500.0).abs() < 1e-9);

    let mut abandoned_drone = d_state.clone();
    abandoned_drone.is_abandoned = true;
    assert!((abandoned_drone.effective_tracking_speed(&ctx) - 2.0).abs() < 1e-9);
    assert!((abandoned_drone.effective_falloff_range(&ctx) - 4_000.0).abs() < 1e-9);

    ctx.db.with_mut_snapshot(|snap| {
        let mut rec = make_fighter_item(d_state.drone_item_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1);
        rec.dynamic_attributes.insert(51, 6_000.0);
        snap.items.insert(d_state.drone_item_id, rec);
    });
    assert_eq!(d_state.cycle_duration_secs(&ctx), 6);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&d_state.drone_item_id).unwrap().dynamic_attributes.insert(51, 90_000.0);
    });
    assert_eq!(d_state.cycle_duration_secs(&ctx), 60);

    let d_ball = Ball {
        id: d_state.ball_id,
        type_id: 2_456,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 2_600.0,
        inertia: 1.0,
        mass: 5_000.0,
        radius: 5.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let stationary_in_opt = Ball {
        id: 910_099,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        radius: 40.0,
        ..d_ball.clone()
    };
    assert!((d_state.compute_combat_hit_multiplier(&ctx, &d_ball, &stationary_in_opt, 40.0) - 1.0).abs() < 1e-9);
    let out_of_range_ball = Ball {
        position: DVec3::new(17_001.0, 0.0, 0.0),
        ..stationary_in_opt.clone()
    };
    assert_eq!(d_state.compute_combat_hit_multiplier(&ctx, &d_ball, &out_of_range_ball, 40.0), 0.0);
    let fast_transversal_ball = Ball {
        position: DVec3::new(2_000.0, 0.0, 0.0),
        velocity: DVec3::new(0.0, 50_000.0, 0.0),
        radius: 25.0,
        ..stationary_in_opt
    };
    let glancing_mult = d_state.compute_combat_hit_multiplier(&ctx, &d_ball, &fast_transversal_ball, 25.0);
    assert!((0.05..1.0).contains(&glancing_mult), "expected clamped glancing hit mult, got {glancing_mult}");

    // 3. Bonused Tractor Beam reaches wreck at 35 km (base 20km * 2 = 40km) and pulls at 2,000 m/s * 5s = 10,000 m,
    //    while rejecting wreck at 45 km (> 40 km) with TargetOutOfRange!
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let attacker_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(attacker_ball.clone()).await;
    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(35_000.0, 0.0, 0.0), id: wreck_in_id, ..attacker_ball.clone() }).await;
    let _ = sys_handle.spawn_ball(Ball { position: DVec3::new(45_000.0, 0.0, 0.0), id: wreck_far_id, ..attacker_ball.clone() }).await;

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let fm = fit.modules[&tractor_id].clone();
    let tractor_type = ctx.sde.types[&24_348].clone();
    let pull_ok = activate_salvager_or_tractor_module(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        tractor_id,
        &fm,
        &tractor_type,
        &attacker_ball,
        Some(wreck_in_id),
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!((py_dict_get(&pull_ok, "newDistanceMeters").and_then(|v| v.as_f64()).unwrap() - 25_000.0).abs() < 1e-3);

    let pull_err = activate_salvager_or_tractor_module(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        tractor_id,
        &fm,
        &tractor_type,
        &attacker_ball,
        Some(wreck_far_id),
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(pull_err.code, "TargetOutOfRange");

    // Exact range boundary: wreck at 40,000.0 m (== optimal 40km) succeeds, 40,000.1 m fails
    let _ = sys_handle
        .set_ball_position(wreck_far_id, DVec3::new(40_000.0, 0.0, 0.0))
        .await;
    assert!(activate_salvager_or_tractor_module(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        tractor_id,
        &fm,
        &tractor_type,
        &attacker_ball,
        Some(wreck_far_id),
        &HashMap::new(),
    )
    .await
    .unwrap()
    .is_ok());
}

fn set_test_hull_bandwidth(ctx: &mut ServerContext, ship_type_id: u32, bandwidth: f64) {
    let mut sde = (*ctx.sde).clone();
    if let Some(hull) = sde.types.get_mut(&ship_type_id) {
        hull.default_attributes.insert(1271, bandwidth);
    }
    ctx.sde = Arc::new(sde);
}

fn extract_v24_launch_entry(rep: &PyRep, source_item_id: i64) -> &[PyRep] {
    let PyRep::Dict(entries) = rep else {
        panic!("expected PyRep::Dict from V24 LaunchDrones, got {rep:?}");
    };
    entries
        .iter()
        .find_map(|(k, v)| {
            if k.as_i64() == Some(source_item_id) {
                if let PyRep::List(seq) = v {
                    return Some(seq.as_slice());
                }
            }
            None
        })
        .unwrap_or_else(|| panic!("missing source item {source_item_id} in {entries:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v24_launch_drones_partial_stack_respects_active_cap_and_returns_per_item_error_tuple() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let _ = ctx.systems.get_or_spawn(sys_id);
    ctx.db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");
    set_test_hull_bandwidth(&mut ctx, 587, 25.0);
    ctx.invalidate_ship_fitting(ship_id);
    let kwargs = HashMap::new();

    // 8-unit stack of 5 Mbit/s Hobgoblin IIs in drone bay with Drones V (max 5) and 25 Mbit/s bandwidth.
    let stack_id = 710_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_id,
            make_fighter_item(stack_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 8),
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    let res = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(stack_id),
                PyRep::Integer(8),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("partial launch of 5/8 drones must succeed");

    let seq = extract_v24_launch_entry(&res, stack_id);
    assert_eq!(seq.len(), 6, "expected 5 launched IDs + 1 error tuple, got {seq:?}");
    let launched_ids: Vec<i64> = seq[..5].iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(launched_ids.len(), 5, "first 5 elements must be integer drone IDs");
    assert_eq!(
        seq[5],
        PyRep::Tuple(vec![PyRep::String("NoDroneManagementAbilitiesLeft".into())]),
        "remaining unlaunched stack units must emit NoDroneManagementAbilitiesLeft tuple"
    );

    let active_ids: std::collections::HashSet<i64> =
        crate::services::drone_test_helpers::active_drones_for_ship(&ctx, ship_id)
            .into_iter()
            .filter(|d| !d.is_abandoned)
            .map(|d| d.ball_id)
            .collect();
    assert_eq!(active_ids.len(), 5);
    for id in &launched_ids {
        assert!(active_ids.contains(id), "launched drone {id} must be active in space");
    }

    let rem_qty = ctx.db.with_snapshot(|s| s.items.get(&stack_id).map(|i| i.quantity));
    assert_eq!(rem_qty, Some(3), "3 unlaunched drones must remain in the drone bay stack");

    let notifs = ctx.drain_notifications_for_char(char_id);
    let item_change_count = notifs.iter().filter(|n| n.event == "OnItemChange").count();
    let drone_state_count = notifs.iter().filter(|n| n.event == "OnDroneStateChange").count();
    assert_eq!(item_change_count, 6, "expected 1 bay stack decrement + 5 launched item OnItemChanges");
    assert_eq!(drone_state_count, 5, "expected 5 OnDroneStateChange notifications");

    // Subsequent launch when already at 5/5 active cap must fail with top-level NoDroneManagementAbilitiesLeft.
    let err_full = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(stack_id),
                PyRep::Integer(3),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_full.code, "NoDroneManagementAbilitiesLeft");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&stack_id).map(|i| i.quantity)),
        Some(3)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v24_launch_drones_partial_stack_bandwidth_cap_and_zero_capacity_errors() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let _ = ctx.systems.get_or_spawn(sys_id);
    ctx.db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");
    set_test_hull_bandwidth(&mut ctx, 587, 25.0);
    ctx.invalidate_ship_fitting(ship_id);
    let kwargs = HashMap::new();

    // 4-unit stack of 10 Mbit/s drones on 25 Mbit/s hull -> 2 launch (20 Mbit/s), 2 remain, MaxBandwidthExceeded tuple.
    let med_stack_id = 710_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            med_stack_id,
            make_fighter_item(med_stack_id, 3_186, char_id, ship_id, InventoryFlag::DroneBay, 4),
        );
    });

    let res = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(med_stack_id),
                PyRep::Integer(4),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("bandwidth-capped partial launch of 2/4 drones must succeed");

    let seq = extract_v24_launch_entry(&res, med_stack_id);
    assert_eq!(seq.len(), 3, "expected 2 launched IDs + 1 MaxBandwidthExceeded tuple, got {seq:?}");
    assert!(seq[0].as_i64().is_some() && seq[1].as_i64().is_some());
    assert_eq!(
        seq[2],
        PyRep::Tuple(vec![PyRep::String("MaxBandwidthExceeded".into())]),
        "remaining drones exceeding bandwidth must emit MaxBandwidthExceeded tuple"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&med_stack_id).map(|i| i.quantity)),
        Some(2)
    );

    // Trying to launch from the remaining 10 Mbit/s stack with only 5 Mbit/s free launches 0 -> top-level Err.
    let err_bw = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(med_stack_id),
                PyRep::Integer(2),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bw.code, "MaxBandwidthExceeded");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&med_stack_id).map(|i| i.quantity)),
        Some(2)
    );

    // With Drones skill 0 (max_drones_allowed == 0), V24 launch returns top-level NoDroneManagementAbilities.
    let (ctx_no_skill, mut sess_no_skill, char2, ship2, sys2) = make_drone_test_ctx();
    let _ = ctx_no_skill.systems.get_or_spawn(sys2);
    seed_pilot_skill(&ctx_no_skill, char2, 3_436, 0);
    ctx_no_skill.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            710_003,
            make_fighter_item(710_003, 2_456, char2, ship2, InventoryFlag::DroneBay, 3),
        );
    });
    let err_no_skill = handle_npcs_drones(
        &ctx_no_skill,
        &mut sess_no_skill,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(710_003),
                PyRep::Integer(3),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_skill.code, "NoDroneManagementAbilities");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v24_launch_drones_multi_item_partial_and_per_item_errors() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let _ = ctx.systems.get_or_spawn(sys_id);
    ctx.db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");
    set_test_hull_bandwidth(&mut ctx, 587, 25.0);
    ctx.invalidate_ship_fitting(ship_id);
    let kwargs = HashMap::new();

    let stack_a = 710_010_i64; // 3x 5 Mbit/s -> all 3 launch (15 Mbit/s, 3 slots)
    let heavy_b = 710_011_i64; // 1x 25 Mbit/s -> 0 launch (MaxBandwidthExceeded)
    let stack_c = 710_012_i64; // 4x 5 Mbit/s -> 2 launch (25 Mbit/s, 5 slots), 2 stay (NoDroneManagementAbilitiesLeft)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_a,
            make_fighter_item(stack_a, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 3),
        );
        snap.items.insert(
            heavy_b,
            make_fighter_item(heavy_b, 2_444, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            stack_c,
            make_fighter_item(stack_c, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 4),
        );
    });

    let res = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(stack_a), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::Integer(heavy_b), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(stack_c), PyRep::Integer(4)]),
            ]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("multi-item V24 launch must partially succeed");

    let seq_a = extract_v24_launch_entry(&res, stack_a);
    assert_eq!(seq_a.len(), 3);
    assert!(seq_a.iter().all(|v| v.as_i64().is_some()));

    let seq_b = extract_v24_launch_entry(&res, heavy_b);
    assert_eq!(seq_b, &[PyRep::Tuple(vec![PyRep::String("MaxBandwidthExceeded".into())])]);

    let seq_c = extract_v24_launch_entry(&res, stack_c);
    assert_eq!(seq_c.len(), 3);
    assert!(seq_c[0].as_i64().is_some() && seq_c[1].as_i64().is_some());
    assert_eq!(
        seq_c[2],
        PyRep::Tuple(vec![PyRep::String("NoDroneManagementAbilitiesLeft".into())])
    );

    let active: Vec<_> = crate::services::drone_test_helpers::active_drones_for_ship(&ctx, ship_id)
        .into_iter()
        .filter(|d| !d.is_abandoned)
        .collect();
    let (active_count, active_bw) =
        (active.len(), active.iter().map(|d| d.bandwidth_used).sum::<f64>());
    assert_eq!(active_count, 5);
    assert!((active_bw - 25.0).abs() < 1e-6);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&heavy_b).map(|i| (i.flag, i.quantity))),
        Some((InventoryFlag::DroneBay, 1))
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&stack_c).map(|i| (i.flag, i.quantity))),
        Some((InventoryFlag::DroneBay, 2))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v24_launch_drones_partial_with_existing_space_drones_and_boundary_inputs() {
    let kwargs = HashMap::new();

    // 1. Existing 3/5 drones in space (15/25 Mbit/s) + launch 4 -> launches 2, leaves 2, emits 3 OnItemChange & 2 OnDroneStateChange.
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let _ = ctx.systems.get_or_spawn(sys_id);
    ctx.db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");
    set_test_hull_bandwidth(&mut ctx, 587, 25.0);
    ctx.invalidate_ship_fitting(ship_id);
    let seed_stack = 710_020_i64;
    let stack_rem = 710_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            seed_stack,
            make_fighter_item(seed_stack, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 3),
        );
        snap.items.insert(
            stack_rem,
            make_fighter_item(stack_rem, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 4),
        );
    });
    let _ = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(seed_stack),
                PyRep::Integer(3),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("seed 3/5 drones must launch");
    let _ = ctx.drain_notifications_for_char(char_id);

    let res = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(stack_rem),
                PyRep::Integer(4),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("partial launch of remaining 2/5 slots must succeed");
    let seq = extract_v24_launch_entry(&res, stack_rem);
    assert_eq!(seq.len(), 3, "expected 2 launched IDs + 1 error tuple, got {seq:?}");
    assert!(seq[0].as_i64().is_some() && seq[1].as_i64().is_some());
    assert_eq!(
        seq[2],
        PyRep::Tuple(vec![PyRep::String("NoDroneManagementAbilitiesLeft".into())])
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&stack_rem).map(|i| (i.flag, i.quantity))),
        Some((InventoryFlag::DroneBay, 2))
    );
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.iter().filter(|n| n.event == "OnItemChange").count(), 3);
    assert_eq!(notifs.iter().filter(|n| n.event == "OnDroneStateChange").count(), 2);

    // 2. Existing 10/25 Mbit/s bandwidth used (1x 10 Mbit/s) + launch 4x 10 Mbit/s -> launches 1, leaves 3, MaxBandwidthExceeded.
    let (mut ctx_bw, mut sess_bw, char_bw, ship_bw, sys_bw) = make_drone_test_ctx();
    let _ = ctx_bw.systems.get_or_spawn(sys_bw);
    set_test_hull_bandwidth(&mut ctx_bw, 587, 25.0);
    ctx_bw.invalidate_ship_fitting(ship_bw);
    let med_seed = 710_030_i64;
    let med_stack = 710_031_i64;
    ctx_bw.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            med_seed,
            make_fighter_item(med_seed, 3_186, char_bw, ship_bw, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            med_stack,
            make_fighter_item(med_stack, 3_186, char_bw, ship_bw, InventoryFlag::DroneBay, 4),
        );
    });
    let _ = handle_npcs_drones(
        &ctx_bw,
        &mut sess_bw,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(med_seed),
                PyRep::Integer(1),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("initial 10 Mbit/s drone must launch");
    let res_bw = handle_npcs_drones(
        &ctx_bw,
        &mut sess_bw,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(med_stack),
                PyRep::Integer(4),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("partial launch of 1 more 10 Mbit/s drone must succeed");
    let seq_bw = extract_v24_launch_entry(&res_bw, med_stack);
    assert_eq!(seq_bw.len(), 2, "expected 1 launched ID + MaxBandwidthExceeded, got {seq_bw:?}");
    assert!(seq_bw[0].as_i64().is_some());
    assert_eq!(
        seq_bw[1],
        PyRep::Tuple(vec![PyRep::String("MaxBandwidthExceeded".into())])
    );
    assert_eq!(
        ctx_bw.db.with_snapshot(|s| s.items.get(&med_stack).map(|i| i.quantity)),
        Some(3)
    );

    // 3. Requesting more than stack quantity (8 requested from 3-unit stack) on empty 5-cap ship launches all 3 cleanly with NO error tuple,
    //    empty V24 LaunchDrones([], None, False) returns empty Dict, and legacy list [d1, d2] at 4/5 slots still rejects all-or-nothing.
    let (mut ctx_edge, mut sess_edge, char_e, ship_e, sys_e) = make_drone_test_ctx();
    let _ = ctx_edge.systems.get_or_spawn(sys_e);
    set_test_hull_bandwidth(&mut ctx_edge, 587, 25.0);
    ctx_edge.invalidate_ship_fitting(ship_e);
    let over_req_stack = 710_040_i64;
    let single_d1 = 710_041_i64;
    let legacy_a = 710_042_i64;
    let legacy_b = 710_043_i64;
    ctx_edge.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            over_req_stack,
            make_fighter_item(over_req_stack, 2_456, char_e, ship_e, InventoryFlag::DroneBay, 3),
        );
        snap.items.insert(
            single_d1,
            make_fighter_item(single_d1, 2_456, char_e, ship_e, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            legacy_a,
            make_fighter_item(legacy_a, 2_456, char_e, ship_e, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            legacy_b,
            make_fighter_item(legacy_b, 2_456, char_e, ship_e, InventoryFlag::DroneBay, 1),
        );
    });
    let empty_rep = handle_npcs_drones(
        &ctx_edge,
        &mut sess_edge,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![]), PyRep::None, PyRep::Bool(false)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("empty V24 LaunchDrones must succeed with empty dict");
    assert_eq!(empty_rep, PyRep::Dict(vec![]));

    let res_over_req = handle_npcs_drones(
        &ctx_edge,
        &mut sess_edge,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(over_req_stack),
                PyRep::Integer(8),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("requesting 8 from a 3-unit stack must launch all 3");
    let seq_over_req = extract_v24_launch_entry(&res_over_req, over_req_stack);
    assert_eq!(seq_over_req.len(), 3, "expected 3 IDs and no error tuple: {seq_over_req:?}");
    assert!(seq_over_req.iter().all(|v| v.as_i64().is_some()));

    let _ = handle_npcs_drones(
        &ctx_edge,
        &mut sess_edge,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(single_d1),
                PyRep::Integer(1),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("4th drone must launch cleanly");

    let err_legacy = handle_npcs_drones(
        &ctx_edge,
        &mut sess_edge,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(legacy_a),
            PyRep::Integer(legacy_b),
        ])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_legacy.code, "NoDroneManagementAbilitiesLeft");
    assert_eq!(
        ctx_edge.db.with_snapshot(|s| s.items.get(&legacy_a).map(|i| i.flag)),
        Some(InventoryFlag::DroneBay)
    );
    assert_eq!(
        ctx_edge.db.with_snapshot(|s| s.items.get(&legacy_b).map(|i| i.flag)),
        Some(InventoryFlag::DroneBay)
    );
}

#[tokio::test]
async fn entity_drone_commands_reject_docked_cloaked_missing_ship_and_invalid_targets_without_mutation() {
    const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
    const CODE_CANNOT_ACTIVATE_CLOAKED: &str = "CannotActivateCloaked";
    const CODE_DRONE_NOT_FOUND: &str = "DroneNotFound";
    const CODE_NO_DRONES_IN_SPACE: &str = "NoDronesInSpace";
    const CODE_INVALID_TARGET: &str = "InvalidTarget";

    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let station_id = ctx.sde.stations.keys().next().copied().unwrap_or(60_003_760);
    let mut sde = (*ctx.sde).clone();
    sde.types.entry(11_370).or_insert_with(|| {
        make_fighter_type(11_370, 330, 7, 5.0, vec![])
    });
    ctx.sde = Arc::new(sde);

    let entity_cmds: &[(&str, Vec<PyRep>)] = &[
        ("CmdReturnBay", vec![]),
        ("CmdReturnHome", vec![]),
        ("CmdReturnAndOrbit", vec![]),
        ("CmdAssist", vec![PyRep::Integer(90_000_777)]),
        ("CmdGuard", vec![PyRep::Integer(90_000_777)]),
        ("CmdAbandonDrone", vec![]),
        ("CmdReconnectToDrones", vec![]),
    ];

    // 1. Docked in station (session) -> ShipNotInSpace
    sess.station_id = Some(station_id);
    for (method, args) in entity_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, "entity", method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} must reject station-docked session");
    }

    // 2. Docked in structure (session) -> ShipNotInSpace
    sess.station_id = None;
    sess.structure_id = Some(1_000_000_000_001);
    for (method, args) in entity_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, "entity", method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} must reject structure-docked session");
    }
    sess.structure_id = None;

    // 3. Undocked session while DB character is still docked -> ShipNotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
    });
    for (method, args) in entity_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, "entity", method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} must reject DB-docked character");
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });

    // 4. Undocked in space without spawned ship ball -> ShipNotInSpace
    for (method, args) in entity_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, "entity", method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} must reject missing ship ball");
    }

    // 5. Spawn ship ball + launch active drone and seed abandoned drone in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    // With 0 drones in space, Station/invalid IDs and empty selectors reject without mutation
    for method in [
        "CmdReturnBay",
        "CmdReturnHome",
        "CmdReturnAndOrbit",
        "CmdAbandonDrone",
        "CmdReconnectToDrones",
    ] {
        let err_station = handle_npcs_drones(
            &ctx,
            &mut sess,
            "entity",
            method,
            &[PyRep::Integer(station_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_station.code, CODE_DRONE_NOT_FOUND, "{method} with non-drone ID");

        let err_empty = handle_npcs_drones(&ctx, &mut sess, "entity", method, &[], &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err_empty.code, CODE_NO_DRONES_IN_SPACE, "{method} with 0 drones in space");
    }
    for method in ["CmdAssist", "CmdGuard"] {
        let res_none = handle_npcs_drones(
            &ctx,
            &mut sess,
            "entity",
            method,
            &[PyRep::Integer(90_000_777)],
            &kwargs,
        )
        .await
        .unwrap()
        .expect("CmdAssist/CmdGuard with 0 active drones returns non-mutating droneCount 0");
        assert_eq!(py_dict_get(&res_none, "droneCount").and_then(PyRep::as_i64), Some(0));
    }

    // Launch active drone (700_020) and spawn an abandoned drone (700_021)
    let active_drone_id = 700_020i64;
    let aban_drone_id = 700_021i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            active_drone_id,
            make_fighter_item(active_drone_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            aban_drone_id,
            make_fighter_item(aban_drone_id, 2_456, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let launch_res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(active_drone_id)])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    let active_ball = match launch_res {
        PyRep::List(v) => v[0].as_i64().unwrap(),
        other => panic!("expected LaunchDrones list, got {other:?}"),
    };
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: aban_drone_id,
                type_id: 2_456,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_600.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let mut st = crate::services::drone_test_helpers::active_drone(&ctx, active_ball).unwrap();
    st.ball_id = aban_drone_id;
    st.drone_item_id = aban_drone_id;
    st.owner_char_id = 0;
    st.parent_ship_id = 0;
    st.is_abandoned = true;
    crate::services::drone_test_helpers::seed_active_drone(&ctx, st);

    // 6. Active non-gate cloak rejects all 7 commands without mutating drones or cloak state
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    });
    for (method, args) in entity_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, "entity", method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_CANNOT_ACTIVATE_CLOAKED, "{method} must reject active cloak");
    }
    let cs_after_cloak = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_cloak.is_cloaked);
    let d = crate::services::drone_test_helpers::active_drone(&ctx, active_ball).unwrap();
    assert!(!d.is_abandoned);
    assert_eq!(d.assist_char_id, None);
    assert_eq!(d.guard_char_id, None);
    assert!(crate::services::drone_test_helpers::active_drone(&ctx, aban_drone_id).unwrap().is_abandoned);

    // Active group-330 cloak module in DB also rejects even if CombatState::is_cloaked is false
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);
    let cloak_mod_id = 700_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            make_fighter_item(cloak_mod_id, 11_370, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    for (method, args) in entity_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, "entity", method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_CANNOT_ACTIVATE_CLOAKED, "{method} must reject active cloak module");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });

    // 7. Invalid Assist/Guard targets (self or station in SDE) reject without mutation
    let err_self_assist = handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdAssist",
        &[PyRep::Integer(char_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_self_assist.code, CODE_INVALID_TARGET);
    if let Some(&sde_station_id) = ctx.sde.stations.keys().next() {
        let err_station_guard = handle_npcs_drones(
            &ctx,
            &mut sess,
            "entity",
            "CmdGuard",
            &[PyRep::Integer(sde_station_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_station_guard.code, CODE_INVALID_TARGET);
    }
    let d = crate::services::drone_test_helpers::active_drone(&ctx, active_ball).unwrap();
    assert_eq!(d.assist_char_id, None);
    assert_eq!(d.guard_char_id, None);

    // 8. Non-owned drone filter returns droneCount 0 without mutating, then valid in-space uncloaked commands succeed cleanly
    let assist_foreign = handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdAssist",
        &[PyRep::Integer(90_000_777), PyRep::List(vec![PyRep::Integer(999_888_777)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("CmdAssist with foreign drone filter returns droneCount 0");
    assert_eq!(py_dict_get(&assist_foreign, "droneCount").and_then(PyRep::as_i64), Some(0));
    assert_eq!(
        crate::services::drone_test_helpers::active_drone(&ctx, active_ball)
            .unwrap()
            .assist_char_id,
        None
    );

    let assist_ok = handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdAssist",
        &[PyRep::Integer(90_000_777)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("valid CmdAssist succeeds");
    assert_eq!(py_dict_get(&assist_ok, "droneCount").and_then(PyRep::as_i64), Some(1));
    handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdAbandonDrone",
        &[PyRep::List(vec![PyRep::Integer(active_ball)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("valid CmdAbandonDrone succeeds");
    handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdReconnectToDrones",
        &[PyRep::List(vec![PyRep::Integer(active_ball)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("valid CmdReconnectToDrones succeeds");
    handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(active_ball)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("valid CmdReturnBay succeeds");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&active_drone_id).map(|i| i.flag)),
        Some(InventoryFlag::DroneBay)
    );

    // Returning a DB ship ID rejects with DroneNotFound, while a destroyed absent drone ID no-ops cleanly
    let err_ship_ret = handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_ship_ret.code, CODE_DRONE_NOT_FOUND);
    let vanished_res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(700_999_888)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("CmdReturnBay on destroyed drone ID succeeds as no-op");
    assert_eq!(vanished_res, PyRep::Dict(Vec::new()));
}

#[tokio::test]
async fn entity_return_home_and_p1_r2_scoop_salvage_guards() {
    const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
    const CODE_CANNOT_ACTIVATE_CLOAKED: &str = "CannotActivateCloaked";
    const CODE_DRONE_NOT_FOUND: &str = "DroneNotFound";
    const CODE_NO_DRONES_IN_SPACE: &str = "NoDronesInSpace";

    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let station_id = ctx.sde.stations.keys().next().copied().unwrap_or(60_003_760);
    let mut sde = (*ctx.sde).clone();
    sde.types.entry(11_370).or_insert_with(|| make_fighter_type(11_370, 330, 7, 5.0, vec![]));
    sde.types.entry(32_787).or_insert_with(|| {
        make_fighter_type(32_787, 1159, 18, 5.0, vec![(1272, 5.0), (37, 1_500.0)])
    });
    ctx.sde = Arc::new(sde);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    // 1. Empty list / tuple selectors reject with NoDronesInSpace when 0 controlled drones are in space
    let mut empty_kw = HashMap::new();
    empty_kw.insert("droneIDs".into(), PyRep::Tuple(vec![]));
    for method in ["CmdReturnHome", "CmdReturnAndOrbit"] {
        let err_empty_arg = handle_npcs_drones(
            &ctx,
            &mut sess,
            "entity",
            method,
            &[PyRep::List(vec![])],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_empty_arg.code, CODE_NO_DRONES_IN_SPACE);

        let err_empty_kw = handle_npcs_drones(
            &ctx,
            &mut sess,
            "entity",
            method,
            &[PyRep::None],
            &empty_kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_empty_kw.code, CODE_NO_DRONES_IN_SPACE);
    }

    // Launch a combat drone and seed an active salvage drone, an abandoned drone, and a cross-system drone
    let active_drone_id = 701_020i64;
    let salvage_drone_id = 701_021i64;
    let aban_drone_id = 701_022i64;
    let other_sys_drone_id = 701_023i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            active_drone_id,
            make_fighter_item(active_drone_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            salvage_drone_id,
            make_fighter_item(salvage_drone_id, 32_787, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            aban_drone_id,
            make_fighter_item(aban_drone_id, 2_456, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let launch_res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(active_drone_id),
            PyRep::Integer(salvage_drone_id),
        ])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    let (active_ball, salvage_ball) = match launch_res {
        PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()),
        other => panic!("expected LaunchDrones list, got {other:?}"),
    };
    let _ = sys_handle.set_ball_position(active_ball, DVec3::new(3_000.0, 0.0, 0.0)).await;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: aban_drone_id,
                type_id: 2_456,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_600.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let base_st = crate::services::drone_test_helpers::active_drone(&ctx, active_ball).unwrap();
    let mut ab_st = base_st.clone();
    ab_st.ball_id = aban_drone_id;
    ab_st.drone_item_id = aban_drone_id;
    ab_st.owner_char_id = 0;
    ab_st.parent_ship_id = 0;
    ab_st.is_abandoned = true;

    let mut cross_st = base_st;
    cross_st.ball_id = other_sys_drone_id;
    cross_st.drone_item_id = other_sys_drone_id;
    cross_st.solar_system_id = 30_002_187;
    crate::services::drone_test_helpers::seed_active_drones(&ctx, [ab_st, cross_st]);

    // 2. CmdReturnHome / CmdReturnAndOrbit reject abandoned or cross-system IDs (positional or kwargs["droneIDs"])
    for bad_id in [aban_drone_id, other_sys_drone_id] {
        let mut bad_kw = HashMap::new();
        bad_kw.insert("droneIDs".into(), PyRep::List(vec![PyRep::Integer(bad_id)]));
        for method in ["CmdReturnHome", "CmdReturnAndOrbit"] {
            let err_pos = handle_npcs_drones(
                &ctx,
                &mut sess,
                "entity",
                method,
                &[PyRep::Integer(bad_id)],
                &kwargs,
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(err_pos.code, CODE_DRONE_NOT_FOUND, "{method} positional {bad_id}");

            let err_kw = handle_npcs_drones(
                &ctx,
                &mut sess,
                "entity",
                method,
                &[PyRep::None],
                &bad_kw,
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(err_kw.code, CODE_DRONE_NOT_FOUND, "{method} kwargs {bad_id}");
        }
    }
    assert!(crate::services::drone_test_helpers::active_drone(&ctx, aban_drone_id).unwrap().is_abandoned);
    assert_eq!(
        crate::services::drone_test_helpers::active_drone(&ctx, other_sys_drone_id)
            .unwrap()
            .solar_system_id,
        30_002_187
    );

    // 3. CmdReturnHome honors kwargs["droneIDs"] selectively and empty list recalls controlled drones
    crate::services::drone_test_helpers::update_active_drone(&ctx, active_ball, |d| {
        d.target_id = Some(999_111);
    })
    .unwrap();
    crate::services::drone_test_helpers::update_active_drone(&ctx, salvage_ball, |d| {
        d.target_id = Some(999_222);
    })
    .unwrap();
    let mut kw_active = HashMap::new();
    kw_active.insert("droneIDs".into(), PyRep::List(vec![PyRep::Integer(active_ball)]));
    handle_npcs_drones(&ctx, &mut sess, "entity", "CmdReturnHome", &[PyRep::None], &kw_active)
        .await
        .unwrap()
        .expect("CmdReturnHome with kwargs[droneIDs] succeeds");
    assert_eq!(
        crate::services::drone_test_helpers::active_drone(&ctx, active_ball)
            .unwrap()
            .target_id,
        None
    );
    assert_eq!(
        crate::services::drone_test_helpers::active_drone(&ctx, salvage_ball)
            .unwrap()
            .target_id,
        Some(999_222),
        "selective kwargs[droneIDs] must not recall unspecified drone"
    );
    handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnAndOrbit",
        &[PyRep::List(vec![])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("empty list recalls all controlled drones");
    assert_eq!(
        crate::services::drone_test_helpers::active_drone(&ctx, salvage_ball)
            .unwrap()
            .target_id,
        None
    );

    // 4. ScoopToDroneBay, ScoopDrone, and CmdSalvage reject docked & cloaked without mutating space drones
    let scoop_salvage_cmds: &[(&str, &str, Vec<PyRep>)] = &[
        ("ship", "ScoopToDroneBay", vec![PyRep::Integer(aban_drone_id)]),
        ("ship", "ScoopDrone", vec![PyRep::Integer(aban_drone_id)]),
        ("entity", "CmdSalvage", vec![]),
    ];

    sess.station_id = Some(station_id);
    for (svc, method, args) in scoop_salvage_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} station-docked");
    }
    sess.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
    });
    for (svc, method, args) in scoop_salvage_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} DB-docked");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });

    let _ = sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    for (svc, method, args) in scoop_salvage_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_CANNOT_ACTIVATE_CLOAKED, "{method} CombatState cloaked");
    }
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);

    let cloak_mod_id = 701_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            make_fighter_item(cloak_mod_id, 11_370, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    for (svc, method, args) in scoop_salvage_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_CANNOT_ACTIVATE_CLOAKED, "{method} DB cloak module");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });

    // Abandoned drone remained untouched in space across all rejected calls; uncloaked scoop & salvage-target-check now pass guard
    assert!(crate::services::drone_test_helpers::has_active_drone(&ctx, aban_drone_id));
    let salv_target_err = handle_npcs_drones(&ctx, &mut sess, "entity", "CmdSalvage", &[], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(salv_target_err.code, "TargetRequired");
    handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(aban_drone_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("uncloaked ScoopToDroneBay succeeds");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&aban_drone_id).map(|i| i.flag)),
        Some(InventoryFlag::DroneBay)
    );
}

#[tokio::test]
async fn bf1_strict_ship_not_in_space_and_db_char_mismatch_rejects_even_with_active_drones() {
    const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";

    let (ctx, mut sess, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = DVec3::new(100_000.0, 0.0, 0.0);

    let spawn_ship = |pos: DVec3| Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    sys_handle
        .spawn_ball_with_combat(spawn_ship(ship_pos), CombatState::default())
        .await
        .unwrap();

    let d1_id = 702_001i64;
    let salv_id = 702_002i64;
    let origin_aban_id = 702_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            d1_id,
            make_fighter_item(d1_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            salv_id,
            make_fighter_item(salv_id, 32_787, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            origin_aban_id,
            make_fighter_item(origin_aban_id, 2_456, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    let launch_res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(d1_id), PyRep::Integer(salv_id)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("LaunchDrones succeeds");
    let (d1_ball, salv_ball) = match launch_res {
        PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()),
        other => panic!("expected LaunchDrones list, got {other:?}"),
    };

    // Abandoned drone near DVec3::ZERO (100m from origin, 99.9km from ship_pos)
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: origin_aban_id,
                type_id: 2_456,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_600.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let mut ab = crate::services::drone_test_helpers::active_drone(&ctx, d1_ball).unwrap();
    ab.ball_id = origin_aban_id;
    ab.drone_item_id = origin_aban_id;
    ab.owner_char_id = 0;
    ab.parent_ship_id = 0;
    ab.is_abandoned = true;
    crate::services::drone_test_helpers::seed_active_drone(&ctx, ab);

    // While ship ball is spawned at (100_000, 0, 0), ScoopToDroneBay on drone at (100, 0, 0) uses real ship_pos -> OutOfScoopRange
    let far_scoop_err = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(origin_aban_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(far_scoop_err.code, "OutOfScoopRange");

    let gated_cmds: &[(&str, &str, Vec<PyRep>)] = &[
        ("entity", "CmdReturnBay", vec![PyRep::List(vec![PyRep::Integer(d1_ball)])]),
        ("entity", "CmdReturnHome", vec![PyRep::List(vec![PyRep::Integer(d1_ball)])]),
        ("entity", "CmdReturnAndOrbit", vec![PyRep::List(vec![PyRep::Integer(d1_ball)])]),
        ("entity", "CmdAssist", vec![PyRep::Integer(90_000_777)]),
        ("entity", "CmdGuard", vec![PyRep::Integer(90_000_777)]),
        ("entity", "CmdAbandonDrone", vec![PyRep::List(vec![PyRep::Integer(d1_ball)])]),
        ("entity", "CmdReconnectToDrones", vec![PyRep::List(vec![PyRep::Integer(origin_aban_id)])]),
        ("entity", "CmdSalvage", vec![PyRep::Integer(999_001)]),
        ("ship", "ScoopToDroneBay", vec![PyRep::Integer(origin_aban_id)]),
        ("ship", "ScoopDrone", vec![PyRep::Integer(origin_aban_id)]),
    ];

    // 1. Remove active ship ball while active drones still exist in space -> unconditional ShipNotInSpace
    assert!(sys_handle.remove_ball(ship_id).await.is_some());
    for (svc, method, args) in gated_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            err.code, CODE_SHIP_NOT_IN_SPACE,
            "{svc}.{method} must reject with ShipNotInSpace when ship ball is absent even with active drones"
        );
    }

    // Re-spawn ship ball and verify DB solar_system_id mismatch rejects all commands with ShipNotInSpace
    sys_handle
        .spawn_ball_with_combat(spawn_ship(ship_pos), CombatState::default())
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_002_187;
    });
    for (svc, method, args) in gated_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            err.code, CODE_SHIP_NOT_IN_SPACE,
            "{svc}.{method} must reject DB solar_system_id mismatch"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id;
    });

    // 3. DB active_ship_id mismatch rejects all commands with ShipNotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id + 999;
    });
    for (svc, method, args) in gated_cmds {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            err.code, CODE_SHIP_NOT_IN_SPACE,
            "{svc}.{method} must reject DB active_ship_id mismatch"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });

    // Verify none of the rejected calls mutated any drone state
    let d1_state = crate::services::drone_test_helpers::active_drone(&ctx, d1_ball).unwrap();
    assert!(!d1_state.is_abandoned);
    assert_eq!(d1_state.parent_ship_id, ship_id);
    assert!(!crate::services::drone_test_helpers::active_drone(&ctx, salv_ball).unwrap().is_abandoned);
    assert!(crate::services::drone_test_helpers::active_drone(&ctx, origin_aban_id).unwrap().is_abandoned);
}

#[tokio::test]
async fn bf2_none_arg_with_drone_ids_kwarg_and_cross_system_abandon_isolation() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let other_sys_id = 30_002_187u32;
    let empty_kw = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = DVec3::new(50_000.0, 0.0, 0.0);

    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: ship_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let d1_item = 703_001i64;
    let d2_item = 703_002i64;
    let other_sys_drone = 703_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            d1_item,
            make_fighter_item(d1_item, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            d2_item,
            make_fighter_item(d2_item, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            other_sys_drone,
            make_fighter_item(
                other_sys_drone,
                2_456,
                char_id,
                other_sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
    });

    let launch_res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(d1_item), PyRep::Integer(d2_item)])],
        &empty_kw,
    )
    .await
    .unwrap()
    .expect("LaunchDrones succeeds");
    let (d1, d2) = match launch_res {
        PyRep::List(v) => (v[0].as_i64().unwrap(), v[1].as_i64().unwrap()),
        other => panic!("expected LaunchDrones list, got {other:?}"),
    };
    let _ = sys_handle
        .set_ball_position(d1, ship_pos + DVec3::new(500.0, 0.0, 0.0))
        .await;
    let _ = sys_handle
        .set_ball_position(d2, ship_pos + DVec3::new(600.0, 0.0, 0.0))
        .await;

    // Seed an active drone owned by the same pilot in another solar system
    let mut cross = crate::services::drone_test_helpers::active_drone(&ctx, d1).unwrap();
    cross.ball_id = other_sys_drone;
    cross.drone_item_id = other_sys_drone;
    cross.solar_system_id = other_sys_id;
    cross.owner_char_id = char_id;
    cross.parent_ship_id = ship_id;
    cross.is_abandoned = false;
    crate::services::drone_test_helpers::seed_active_drone(&ctx, cross);

    let mut kw_d1 = HashMap::new();
    kw_d1.insert("droneIDs".into(), PyRep::List(vec![PyRep::Integer(d1)]));
    let mut kw_other = HashMap::new();
    kw_other.insert("droneIDs".into(), PyRep::List(vec![PyRep::Integer(other_sys_drone)]));

    // 1. CmdAbandonDrone(None, droneIDs=[d1]) abandons ONLY d1; d2 and cross-system drone stay untouched
    handle_npcs_drones(&ctx, &mut sess, "entity", "CmdAbandonDrone", &[PyRep::None], &kw_d1)
        .await
        .unwrap()
        .expect("CmdAbandonDrone(None, droneIDs=[d1]) succeeds");
    assert!(
        crate::services::drone_test_helpers::active_drone(&ctx, d1)
            .unwrap()
            .is_abandoned,
        "d1 must be abandoned"
    );
    let d2_after = crate::services::drone_test_helpers::active_drone(&ctx, d2).unwrap();
    assert!(!d2_after.is_abandoned, "d2 must remain controlled");
    assert_eq!(d2_after.parent_ship_id, ship_id);
    assert!(
        !crate::services::drone_test_helpers::active_drone(&ctx, other_sys_drone)
            .unwrap()
            .is_abandoned,
        "cross-system drone must remain active"
    );

    // Cross-system drone filter rejects via RPC and is ignored by direct sp6_lifecycle::handle_cmd_abandon_drone
    let cross_err = handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdAbandonDrone",
        &[PyRep::None],
        &kw_other,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(cross_err.code, "DroneNotFound");
    crate::sp6_lifecycle::handle_cmd_abandon_drone(&ctx, &mut sess, &[PyRep::None], &kw_other)
        .await
        .expect("direct handle_cmd_abandon_drone ignores cross-system ID");
    assert!(
        !crate::services::drone_test_helpers::active_drone(&ctx, other_sys_drone)
            .unwrap()
            .is_abandoned
    );

    // Wildcard CmdAbandonDrone([]) abandons remaining same-system drone d2, but NEVER cross-system drone
    handle_npcs_drones(&ctx, &mut sess, "entity", "CmdAbandonDrone", &[], &empty_kw)
        .await
        .unwrap()
        .expect("wildcard CmdAbandonDrone succeeds for remaining same-system drone d2");
    assert!(crate::services::drone_test_helpers::active_drone(&ctx, d1).unwrap().is_abandoned);
    assert!(crate::services::drone_test_helpers::active_drone(&ctx, d2).unwrap().is_abandoned);
    assert!(
        !crate::services::drone_test_helpers::active_drone(&ctx, other_sys_drone)
            .unwrap()
            .is_abandoned,
        "same-system filter must protect drones in other solar systems"
    );

    // 2. CmdReconnectToDrones(None, droneIDs=[d1]) reconnects ONLY d1; d2 remains abandoned
    handle_npcs_drones(
        &ctx,
        &mut sess,
        "entity",
        "CmdReconnectToDrones",
        &[PyRep::None],
        &kw_d1,
    )
    .await
    .unwrap()
    .expect("CmdReconnectToDrones(None, droneIDs=[d1]) succeeds");
    let d1_reconnected = crate::services::drone_test_helpers::active_drone(&ctx, d1).unwrap();
    assert!(!d1_reconnected.is_abandoned, "d1 must be reconnected");
    assert_eq!(d1_reconnected.parent_ship_id, ship_id);
    assert!(
        crate::services::drone_test_helpers::active_drone(&ctx, d2)
            .unwrap()
            .is_abandoned,
        "d2 must stay abandoned"
    );

    // Direct sp6_lifecycle::handle_cmd_reconnect_to_drones(None, droneIDs=[d2]) reconnects d2 cleanly
    let mut kw_d2 = HashMap::new();
    kw_d2.insert("droneIDs".into(), PyRep::List(vec![PyRep::Integer(d2)]));
    crate::sp6_lifecycle::handle_cmd_reconnect_to_drones(&ctx, &mut sess, &[PyRep::None], &kw_d2)
        .await
        .expect("direct handle_cmd_reconnect_to_drones honors kwargs[droneIDs] after None");
    let d2_reconnected = crate::services::drone_test_helpers::active_drone(&ctx, d2).unwrap();
    assert!(!d2_reconnected.is_abandoned);
    assert_eq!(d2_reconnected.parent_ship_id, ship_id);

    // 3. CmdReturnBay(None, droneIDs=[d1]) recalls ONLY d1 to DroneBay; d2 remains active in space
    handle_npcs_drones(&ctx, &mut sess, "entity", "CmdReturnBay", &[PyRep::None], &kw_d1)
        .await
        .unwrap()
        .expect("CmdReturnBay(None, droneIDs=[d1]) succeeds");
    assert!(
        !crate::services::drone_test_helpers::has_active_drone(&ctx, d1),
        "d1 must be recalled to bay"
    );
    assert!(
        crate::services::drone_test_helpers::has_active_drone(&ctx, d2),
        "d2 must remain in space"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&d1_item].flag, InventoryFlag::DroneBay);
        assert_eq!(snap.items[&d1_item].location_id, ship_id);
        assert_eq!(snap.items[&d2_item].flag, InventoryFlag::None);
        assert_eq!(snap.items[&d2_item].location_id, sys_id as i64);
    });

    // Direct sp6_lifecycle::handle_cmd_return_bay(None, droneIDs=[d2]) recalls d2 cleanly
    crate::sp6_lifecycle::handle_cmd_return_bay(&ctx, &mut sess, &[PyRep::None], &kw_d2)
        .await
        .expect("direct handle_cmd_return_bay honors kwargs[droneIDs] after None");
    assert!(!crate::services::drone_test_helpers::has_active_drone(&ctx, d2));
    assert!(
        !crate::services::drone_test_helpers::active_drone(&ctx, other_sys_drone)
            .unwrap()
            .is_abandoned,
        "cross-system drone remains intact throughout"
    );
}

#[tokio::test]
async fn bf01_drone_commands_reject_active_upwell_structure_control() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let char_id = 90_000_001_i64;
    let st_id = 1_000_000_009_001_i64;
    let sys_id = 30_000_142_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(st_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = st_id;
        }
    });
    let mut sess = SessionState::new(88_901);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.structure_id = Some(st_id);
    sess.ship_id = Some(st_id);
    let kw = HashMap::new();

    for (svc, method, args) in [
        ("entity", "CmdReturnBay", vec![]),
        ("entity", "CmdReturnHome", vec![]),
        ("entity", "CmdAssist", vec![PyRep::Integer(90_000_002)]),
        ("entity", "CmdAbandonDrone", vec![]),
        ("entity", "CmdReconnectToDrones", vec![]),
        ("entity", "CmdSalvage", vec![]),
        ("ship", "ScoopToDroneBay", vec![PyRep::Integer(500_000_001)]),
    ] {
        let err = handle_npcs_drones(&ctx, &mut sess, svc, method, &args, &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, "ShipNotInSpace", "{svc}.{method}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_contexts_isolate_drone_reservation_id_allocation() {
    let pair =
        eve_testkit::process_global_ratchet::TwoContextPair::from_factory(make_drone_test_ctx);
    let (ctx_a, mut sess_a, char_a, ship_a, sys_a) = pair.left;
    let _ = ctx_a.systems.get_or_spawn(sys_a);
    let (ctx_b, _sess_b, _char_b, _ship_b, _sys_b) = pair.right;

    assert_eq!(ctx_a.systems.drone_rt_state.alloc_drone_reservation_id(), -200_000);
    assert_eq!(ctx_b.systems.drone_rt_state.alloc_drone_reservation_id(), -200_000);

    // Advance ctx_a via both direct reservation allocation and LaunchDrones RPC.
    assert_eq!(ctx_a.systems.drone_rt_state.alloc_drone_reservation_id(), -200_001);
    ctx_a
        .db
        .acquire_online_lock(1, sess_a.session_id, Some(char_a), true, ctx_a.now_unix_secs())
        .expect("online lock");
    let drone_stack = 710_901_i64;
    ctx_a.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_stack,
            make_fighter_item(drone_stack, 2_456, char_a, ship_a, InventoryFlag::DroneBay, 2),
        );
    });
    let kwargs = HashMap::new();
    let launch_res = handle_npcs_drones(
        &ctx_a,
        &mut sess_a,
        "ship",
        "LaunchDrones",
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(drone_stack),
                PyRep::Integer(2),
            ])]),
            PyRep::None,
            PyRep::Bool(false),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("LaunchDrones on ctx_a must succeed");
    assert_eq!(extract_v24_launch_entry(&launch_res, drone_stack).len(), 2);
    assert_eq!(ctx_a.systems.drone_rt_state.alloc_drone_reservation_id(), -200_004);

    // ctx_b's reservation ID sequence must remain completely unadvanced by ctx_a.
    assert_eq!(ctx_b.systems.drone_rt_state.alloc_drone_reservation_id(), -200_001);
}
