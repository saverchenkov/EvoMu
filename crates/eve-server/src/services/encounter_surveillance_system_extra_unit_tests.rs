//! Deep branch-coverage and mutation-killing unit tests for `encounter_surveillance_system`.

#![forbid(unsafe_code)]

use super::encounter_surveillance_system::{
    handle_ess_link, prime_cache_result, select_ess_reserve_key_in_cargo,
};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use crate::services::space_destiny::{set_ess_reserve_pulses_in_blobs, EssReservePulseState};
use eve_db::{InventoryFlag, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_proto::PyRep;
use eve_sde::SolarSystem;
use std::sync::Arc;

fn add_test_system(
    sde: &mut eve_sde::SdeRegistry,
    sys_id: u32,
    region_id: u32,
    sec: f64,
    galactic_pos_ly: [f64; 3],
) {
    sde.solar_systems.insert(
        sys_id,
        SolarSystem {
            solar_system_id: sys_id,
            region_id,
            constellation_id: 20_000_700,
            name: format!("Sys_{sys_id}"),
            security_status: sec,
            radius: 1.0e12,
            galactic_pos_ly,
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
}

#[test]
fn reserve_key_selection_covers_all_quadrants_galactic_pos_fallback_and_target_arg() {
    let (mut ctx, _sess, char_id, ship_id, _hs_id) = make_fighter_ctx_and_session();
    let sde_mut = Arc::make_mut(&mut ctx.sde);

    for tid in 57_714..=57_721 {
        sde_mut
            .types
            .insert(tid, make_fighter_type(tid, 4102, 17, 1.0, vec![]));
    }

    // 4 explicit region systems + 4 galactic_pos_ly fallback systems (region 10_000_999)
    add_test_system(sde_mut, 30_100_001, 10_000_003, -0.5, [0.0, 0.0, 0.0]); // Nw explicit
    add_test_system(sde_mut, 30_100_002, 10_000_007, -0.5, [0.0, 0.0, 0.0]); // Ne explicit
    add_test_system(sde_mut, 30_100_003, 10_000_005, -0.5, [0.0, 0.0, 0.0]); // Se explicit
    add_test_system(sde_mut, 30_100_004, 10_000_014, -0.5, [0.0, 0.0, 0.0]); // Sw explicit

    add_test_system(sde_mut, 30_200_001, 10_000_999, -0.5, [-5.0, 0.0, 0.0]); // (false, true) -> Nw
    add_test_system(sde_mut, 30_200_002, 10_000_999, -0.5, [5.0, 0.0, 0.0]); // (true, true) -> Ne
    add_test_system(sde_mut, 30_200_003, 10_000_999, -0.5, [5.0, 0.0, -5.0]); // (true, false) -> Se
    add_test_system(sde_mut, 30_200_004, 10_000_999, -0.5, [-5.0, 0.0, -5.0]); // (false, false) -> Sw

    let all_keys = vec![
        make_fighter_item(101, 57_714, char_id, ship_id, InventoryFlag::Cargo, 1), // Nw 15
        make_fighter_item(102, 57_715, char_id, ship_id, InventoryFlag::Cargo, 1), // Nw 45
        make_fighter_item(103, 57_716, char_id, ship_id, InventoryFlag::Cargo, 1), // Ne 15
        make_fighter_item(104, 57_719, char_id, ship_id, InventoryFlag::Cargo, 1), // Ne 45
        make_fighter_item(105, 57_717, char_id, ship_id, InventoryFlag::Cargo, 1), // Se 15
        make_fighter_item(106, 57_718, char_id, ship_id, InventoryFlag::Cargo, 1), // Se 45
        make_fighter_item(107, 57_720, char_id, ship_id, InventoryFlag::Cargo, 1), // Sw 15
        make_fighter_item(108, 57_721, char_id, ship_id, InventoryFlag::Cargo, 1), // Sw 45
        make_fighter_item(109, 34, char_id, ship_id, InventoryFlag::Cargo, 100),   // Non-key
    ];

    for (sys_id, key_15, key_45) in [
        (30_100_001, (101, 57_714, 15), (102, 57_715, 45)),
        (30_200_001, (101, 57_714, 15), (102, 57_715, 45)),
        (30_100_002, (103, 57_716, 15), (104, 57_719, 45)),
        (30_200_002, (103, 57_716, 15), (104, 57_719, 45)),
        (30_100_003, (105, 57_717, 15), (106, 57_718, 45)),
        (30_200_003, (105, 57_717, 15), (106, 57_718, 45)),
        (30_100_004, (107, 57_720, 15), (108, 57_721, 45)),
        (30_200_004, (107, 57_720, 15), (108, 57_721, 45)),
    ] {
        assert_eq!(
            select_ess_reserve_key_in_cargo(&ctx.sde, all_keys.iter(), char_id, ship_id, sys_id, None)
                .unwrap(),
            key_15
        );
        // Selecting 45m key by item_id or type_id via target_arg
        assert_eq!(
            select_ess_reserve_key_in_cargo(
                &ctx.sde,
                all_keys.iter(),
                char_id,
                ship_id,
                sys_id,
                Some(key_45.0)
            )
            .unwrap(),
            key_45
        );
        assert_eq!(
            select_ess_reserve_key_in_cargo(
                &ctx.sde,
                all_keys.iter(),
                char_id,
                ship_id,
                sys_id,
                Some(key_45.1 as i64)
            )
            .unwrap(),
            key_45
        );
    }

    // Passing non-key item_id (109) in target_arg is never accepted
    assert!(select_ess_reserve_key_in_cargo(
        &ctx.sde,
        all_keys.iter(),
        char_id,
        ship_id,
        30_100_001,
        Some(109)
    )
    .is_err());

    // Items with wrong owner, wrong location, non-Cargo flag, or quantity == 0 are ignored
    let invalid_items = vec![
        make_fighter_item(201, 57_714, char_id + 1, ship_id, InventoryFlag::Cargo, 1),
        make_fighter_item(202, 57_714, char_id, ship_id + 1, InventoryFlag::Cargo, 1),
        make_fighter_item(203, 57_714, char_id, ship_id, InventoryFlag::Hangar, 1),
        make_fighter_item(204, 57_714, char_id, ship_id, InventoryFlag::Cargo, 0),
    ];
    assert!(select_ess_reserve_key_in_cargo(
        &ctx.sde,
        invalid_items.iter(),
        char_id,
        ship_id,
        30_100_001,
        None
    )
    .is_err());
}

#[test]
fn prime_cache_result_covers_wormhole_missing_system_active_link_and_expired_pulses() {
    let (mut ctx, mut sess, char_id, _ship_id, _hs_id) = make_fighter_ctx_and_session();
    let null_sys = 30_004_759_u32;
    let wh_sys = 31_000_005_u32;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    add_test_system(sde_mut, null_sys, 10_000_060, -0.9, [-10.0, 0.0, -10.0]);
    add_test_system(sde_mut, wh_sys, 11_000_001, -1.0, [0.0, 0.0, 0.0]);

    // 1. No solar_system_id on session and no args -> PyRep::None
    sess.solar_system_id = None;
    assert_eq!(prime_cache_result(&ctx, &sess, &[]).unwrap(), PyRep::None);

    // 2. Wormhole system (>= 31_000_000) -> PyRep::None even though sec <= 0.0
    assert_eq!(
        prime_cache_result(&ctx, &sess, &[PyRep::Integer(wh_sys as i64)]).unwrap(),
        PyRep::None
    );

    // 3. Null-sec system not yet in snap.system_states -> PyRep::None
    assert_eq!(
        prime_cache_result(&ctx, &sess, &[PyRep::Integer(null_sys as i64)]).unwrap(),
        PyRep::None
    );

    // 4. Null-sec system with active mainBankLink and expired reserve unlock -> rem_pulses == 0
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                ess_main_bank: IskCents::from_isk(10_000_000),
                ess_reserve_bank: IskCents::from_isk(20_000_000),
                ess_linked_character_id: Some(char_id),
                ess_link_started_tick: Some(1_700_000_000),
                ..Default::default()
            },
        );
        set_ess_reserve_pulses_in_blobs(
            &mut snap.service_state_blobs,
            null_sys,
            &EssReservePulseState {
                unlocked_until_unix: now.saturating_sub(10),
                pulses_total: 15,
                pulses_remaining: 7,
                last_pulse_sec: Some(now.saturating_sub(120)),
                link_started_unix: None,
                reserve_linked_character_id: None,
                reserve_link_started_tick: None,
            },
        );
    });

    let primed = prime_cache_result(&ctx, &sess, &[PyRep::Integer(null_sys as i64)]).unwrap();
    assert_eq!(
        primed
            .dict_get("reserveBankPulsesRemaining")
            .and_then(|v| v.as_i64()),
        Some(0)
    );
    let link = primed.dict_get("mainBankLink").unwrap();
    assert_eq!(
        link.dict_get("characterID").and_then(|v| v.as_i64()),
        Some(char_id)
    );
}

#[tokio::test]
async fn handle_ess_link_covers_preconditions_range_boundary_conflicts_cooldown_and_termination() {
    let (mut ctx, mut sess, char_id, ship_id, hs_id) = make_fighter_ctx_and_session();
    let null_sys = 30_004_759_u32;
    let ess_ball_id = 820_000_000 + null_sys as i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    add_test_system(sde_mut, null_sys, 10_000_060, -0.9, [-10.0, 0.0, -10.0]);
    sde_mut
        .types
        .insert(57_720, make_fighter_type(57_720, 4102, 17, 1.0, vec![]));

    // 1. Docked in station -> rejected
    sess.station_id = Some(60_003_760);
    assert!(handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[])
        .await
        .is_err());
    sess.station_id = None;

    // 2. No active ship -> rejected
    sess.ship_id = None;
    assert!(handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[])
        .await
        .is_err());
    sess.ship_id = Some(ship_id);

    // 3. High-sec system -> rejected
    sess.solar_system_id = Some(hs_id);
    assert!(handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[])
        .await
        .is_err());
    sess.solar_system_id = Some(null_sys);

    // 4. SafetyLevel::Green -> rejected
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    assert!(handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[])
        .await
        .is_err());
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });

    // 5. Ship not yet in space -> rejected
    assert!(handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)]
    )
    .await
    .is_err());

    let sys = ctx.systems.get_or_spawn(null_sys);
    sys.spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 600,
            position: DVec3::new(2_501.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::new(),
    )
    .await
    .unwrap();

    // 6. Missing args[0] or ESS ball not in space -> rejected
    assert!(handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[])
        .await
        .is_err());
    assert!(handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)]
    )
    .await
    .is_err());

    // 7. Spawn ESS ball (type 32882) at origin; ship at 2501m -> OutsideEssLinkRange
    sys.spawn_ball_with_combat(
        Ball {
            id: ess_ball_id,
            type_id: 32_882,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::new(),
    )
    .await
    .unwrap();
    assert!(handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)]
    )
    .await
    .is_err());

    // Move ship to exact boundary 2500.0m
    let _ = sys.remove_ball(ship_id).await;
    sys.spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 600,
            position: DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::new(),
    )
    .await
    .unwrap();

    // 8. Seed state with empty main bank -> EssBankEmpty; already linked by another pilot -> EssAlreadyLinked
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents::from_isk(10_000_000),
                ess_linked_character_id: Some(char_id + 99),
                ..Default::default()
            },
        );
    });
    assert!(handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)]
    )
    .await
    .is_err());

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.get_mut(&null_sys).unwrap();
        st.ess_linked_character_id = None;
    });
    assert!(handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)]
    )
    .await
    .is_err());

    // 9. Reserve bank with stack of 2 keys decrements quantity to 1, and completing last pulse terminates link
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            991_001,
            make_fighter_item(991_001, 57_720, char_id, ship_id, InventoryFlag::Cargo, 2),
        );
    });
    handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.items[&991_001].quantity, 1);
        set_ess_reserve_pulses_in_blobs(
            &mut snap.service_state_blobs,
            null_sys,
            &EssReservePulseState {
                unlocked_until_unix: ctx.now_unix() + 60,
                pulses_total: 1,
                pulses_remaining: 1,
                last_pulse_sec: None,
                link_started_unix: Some(ctx.now_unix().saturating_sub(10)),
                reserve_linked_character_id: Some(char_id),
                reserve_link_started_tick: Some(0),
            },
        );
    });
    // Immediate call before 5 ticks -> EssLinkTimerNotElapsed
    assert!(handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)]
    )
    .await
    .is_err());

    for _ in 0..6 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let final_pulse = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .unwrap();
    assert_eq!(
        final_pulse.dict_get("stolenISK").and_then(|v| v.as_f64()),
        Some(10_000_000.0)
    );
}
