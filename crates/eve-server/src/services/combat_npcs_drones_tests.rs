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
