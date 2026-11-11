use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{InventoryFlag, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_sde::SolarSystem;
use std::collections::HashMap;
use std::sync::Arc;

fn setup_nullsec_ess_ctx() -> (ServerContext, SessionState, i64, i64, u32, i64) {
    let (mut ctx, mut sess, char_id, ship_id, _highsec_id) = make_fighter_ctx_and_session();
    let null_sys_id = 30_004_759_u32;
    let ess_ball_id = 820_000_000 + null_sys_id as i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.solar_systems.insert(
        null_sys_id,
        SolarSystem {
            solar_system_id: null_sys_id,
            region_id: 10_000_060, // Delve -> Sw quadrant
            constellation_id: 20_000_700,
            name: "1DQ1-A".into(),
            security_status: -0.9,
            radius: 1.0e12,
            galactic_pos_ly: [-10.0, 0.0, -10.0],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    sde_mut
        .types
        .insert(57720, make_fighter_type(57720, 4102, 17, 1.0, vec![])); // Sw 15m key
    sde_mut
        .types
        .insert(57714, make_fighter_type(57714, 4102, 17, 1.0, vec![])); // Nw 15m key

    sess.solar_system_id = Some(null_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.system_states.insert(
            null_sys_id,
            SystemDynamicState {
                solar_system_id: null_sys_id,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents::from_isk(15_000_000),
                ess_reserve_bank: IskCents::from_isk(45_000_000),
                ess_main_bank_contributors: HashMap::new(),
                ess_cycle_started_unix: 0,
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                ..SystemDynamicState::default()
            },
        );
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    (ctx, sess, char_id, ship_id, null_sys_id, ess_ball_id)
}

#[test]
fn test_quadrant_resolution_and_select_ess_reserve_key_in_cargo() {
    let (ctx, _sess, char_id, ship_id, null_sys_id, _ess_ball_id) = setup_nullsec_ess_ctx();
    assert!(matches!(
        system_ess_quadrant(&ctx.sde, null_sys_id),
        EssQuadrant::Sw
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57714),
        Some((EssQuadrant::Nw, 15))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57715),
        Some((EssQuadrant::Nw, 45))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57716),
        Some((EssQuadrant::Ne, 15))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57719),
        Some((EssQuadrant::Ne, 45))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57717),
        Some((EssQuadrant::Se, 15))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57718),
        Some((EssQuadrant::Se, 45))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57720),
        Some((EssQuadrant::Sw, 15))
    ));
    assert!(matches!(
        ess_key_quadrant_and_duration(57721),
        Some((EssQuadrant::Sw, 45))
    ));
    assert!(ess_key_quadrant_and_duration(34).is_none());

    // Empty cargo -> MissingEssReserveSecurityKey
    let empty: Vec<eve_db::InventoryItemRecord> = Vec::new();
    assert_eq!(
        select_ess_reserve_key_in_cargo(&ctx.sde, empty.iter(), char_id, ship_id, null_sys_id, None)
            .unwrap_err()
            .code,
        "MissingEssReserveSecurityKey"
    );

    // Wrong quadrant key -> EssReserveKeyWrongQuadrant
    let wrong = vec![make_fighter_item(
        801,
        57714,
        char_id,
        ship_id,
        InventoryFlag::Cargo,
        1,
    )];
    assert_eq!(
        select_ess_reserve_key_in_cargo(&ctx.sde, wrong.iter(), char_id, ship_id, null_sys_id, None)
            .unwrap_err()
            .code,
        "EssReserveKeyWrongQuadrant"
    );

    // Matching quadrant key -> Ok((802, 57720, 15))
    let matching = vec![make_fighter_item(
        802,
        57720,
        char_id,
        ship_id,
        InventoryFlag::Cargo,
        1,
    )];
    assert_eq!(
        select_ess_reserve_key_in_cargo(
            &ctx.sde,
            matching.iter(),
            char_id,
            ship_id,
            null_sys_id,
            None
        )
        .unwrap(),
        (802, 57720, 15)
    );
}

#[tokio::test]
async fn test_prime_cache_result_and_handle_ess_link_main_and_reserve() {
    let (ctx, sess, char_id, ship_id, null_sys_id, ess_ball_id) = setup_nullsec_ess_ctx();

    // Highsec returns PyRep::None
    let hs = prime_cache_result(&ctx, &sess, &[PyRep::Integer(30_000_142)]).unwrap();
    assert_eq!(hs, PyRep::None);

    // Nullsec returns Dict with mainValue and reserveValue
    let ns = prime_cache_result(&ctx, &sess, &[]).unwrap();
    assert_eq!(
        ns.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(null_sys_id as i64)
    );
    assert_eq!(
        ns.dict_get("mainValue").and_then(|v| v.as_f64()),
        Some(15_000_000.0)
    );
    assert_eq!(
        ns.dict_get("reserveValue").and_then(|v| v.as_f64()),
        Some(45_000_000.0)
    );

    // Spawn player ship and ESS beacon (type 32882) within 2500m
    let sys = ctx.systems.get_or_spawn(null_sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: DVec3::new(1000.0, 0.0, 0.0),
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
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ess_ball_id,
                type_id: 32882,
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
        .await;

    // 1. First call to AttemptLinkToMainBank starts link
    let start_main = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .expect("Starting main bank link should succeed");
    assert_eq!(
        start_main.dict_get("stolenISK").and_then(|v| v.as_f64()),
        Some(0.0)
    );

    // Backdate link start tick by 5 ticks so second call completes the payout
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&null_sys_id) {
            st.ess_link_started_tick = Some(0);
        }
    });
    for _ in 0..6 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let finish_main = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToMainBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .expect("Completing main bank link after 5 ticks should succeed");
    assert_eq!(
        finish_main.dict_get("stolenISK").and_then(|v| v.as_f64()),
        Some(15_000_000.0)
    );

    // 2. Seed matching Sw reserve key in cargo and test AttemptLinkToReserveBank
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_001,
            make_fighter_item(
                990_001,
                57720,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                1,
            ),
        );
    });
    let start_res = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .expect("Starting reserve bank link with matching key should succeed");
    assert_eq!(
        start_res.dict_get("stolenISK").and_then(|v| v.as_f64()),
        Some(0.0)
    );

    for _ in 0..6 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let pulse_res = handle_ess_link(
        &ctx,
        &sess,
        "AttemptLinkToReserveBank",
        &[PyRep::Integer(ess_ball_id)],
    )
    .await
    .expect("Completing reserve bank pulse after 5 ticks should pay out");
    assert!(
        pulse_res
            .dict_get("stolenISK")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 0.0
    );
}

#[tokio::test]
async fn test_ess_all_error_guards_and_tick_active_links_sever() {
    let (ctx, sess, char_id, ship_id, null_sys_id, ess_ball_id) = setup_nullsec_ess_ctx();
    let other_char = 90_000_222_i64;

    // 1. prime_cache_result branches: non-nullsec, missing system, unauthenticated
    let unauth = SessionState::default();
    assert!(prime_cache_result(&ctx, &unauth, &[]).is_err());
    assert_eq!(
        prime_cache_result(&ctx, &sess, &[PyRep::Integer(99_999_999)]).unwrap(),
        PyRep::None
    );

    // 2. handle_ess_link pre-flight guards
    let mut s_docked = sess.clone();
    s_docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_ess_link(&ctx, &s_docked, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    let mut s_noship = sess.clone();
    s_noship.ship_id = None;
    assert_eq!(
        handle_ess_link(&ctx, &s_noship, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut s_nosys = sess.clone();
    s_nosys.solar_system_id = None;
    assert_eq!(
        handle_ess_link(&ctx, &s_nosys, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    let mut s_highsec = sess.clone();
    s_highsec.solar_system_id = Some(30_000_142);
    assert_eq!(
        handle_ess_link(&ctx, &s_highsec, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "EssOnlyInNullSec"
    );

    // Green safety blocked
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    // Ship not in space
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys = ctx.systems.get_or_spawn(null_sys_id);
    // Pod (670) rejected
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 15.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "CantDoThatInPod"
    );

    // Normal ship warping -> ShipWarping
    let _ = sys.remove_ball(ship_id).await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 50.0,
                speed_fraction: 1.0,
                mode: MovementMode::new_warp(
                    DVec3::new(500.0, 0.0, 0.0),
                    DVec3::new(100_000_000.0, 0.0, 0.0),
                    3.0,
                ),
            },
            CombatState::new(),
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "ShipWarping"
    );

    // Destroyed ship -> ShipDestroyed
    let _ = sys.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = sys
        .set_combat_state(
            ship_id,
            CombatState {
                structure_hp: 0.0,
                ..CombatState::default()
            },
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "ShipDestroyed"
    );

    // Cloaked ship -> CantLinkWhileCloaked
    let _ = sys
        .set_combat_state(
            ship_id,
            CombatState {
                structure_hp: 1000.0,
                is_cloaked: true,
                ..CombatState::default()
            },
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "CantLinkWhileCloaked"
    );
    let _ = sys
        .set_combat_state(
            ship_id,
            CombatState {
                structure_hp: 1000.0,
                is_cloaked: false,
                ..CombatState::default()
            },
        )
        .await;

    // Missing arg / missing beacon / wrong type / out of range
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[])
            .await
            .unwrap_err()
            .code,
        "EssBeaconNotPresent"
    );
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "EssBeaconNotPresent"
    );
    let non_ess_id = 820_999_001_i64;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: non_ess_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(non_ess_id)])
            .await
            .unwrap_err()
            .code,
        "EssBeaconNotPresent"
    );

    // Spawn ESS far away -> OutsideEssLinkRange
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ess_ball_id,
                type_id: 32882,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1e9,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "OutsideEssLinkRange"
    );

    // Move ship within range; test EssAlreadyLinked & EssReserveAlreadyLinked
    let _ = sys.set_ball_position(ess_ball_id, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&null_sys_id) {
            st.ess_linked_character_id = Some(other_char);
        }
        let mut pst = crate::services::space_destiny::get_ess_reserve_pulses(snap,
            null_sys_id,
        );
        pst.reserve_linked_character_id = Some(other_char);
        crate::services::space_destiny::set_ess_reserve_pulses(snap,
            null_sys_id,
            &pst,
        );
    });
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "EssAlreadyLinked"
    );
    assert_eq!(
        handle_ess_link(&ctx, &sess, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)])
            .await
            .unwrap_err()
            .code,
        "EssReserveAlreadyLinked"
    );

    // tick_ess_main_bank_links: first when ship is in range (kept), then out of range (severed!)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&null_sys_id) {
            st.ess_linked_character_id = Some(char_id);
        }
    });
    assert_eq!(tick_ess_main_bank_links(&ctx).await, 0);

    let _ = sys.set_ball_position(ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(tick_ess_main_bank_links(&ctx).await, 1);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&null_sys_id].ess_linked_character_id),
        None
    );

    // Test EssQuadrant::other branches via system_ess_quadrant
    assert!(matches!(system_ess_quadrant(&ctx.sde, 30_000_142), EssQuadrant::Ne | EssQuadrant::Nw | EssQuadrant::Se | EssQuadrant::Sw));

    // Cover prime_cache_result with active mainBankLink + expired unlocked_until, select_ess_reserve_key target_arg,
    // reserve link already-unlocked auto-calc, EssReservePulseCooldown, reserve pulse exhaustion,
    // EssBankEmpty, SystemStateNotFound, and tick_ess_main_bank_links when ship is cloaked.
    let cur_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&null_sys_id) {
            st.ess_linked_character_id = Some(char_id);
            st.ess_link_started_tick = Some(10);
        }
        let mut pst = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys_id);
        pst.unlocked_until_unix = cur_now.saturating_sub(10);
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys_id, &pst);
    });
    let primed = prime_cache_result(&ctx, &sess, &[]).unwrap();
    assert!(primed.dict_get("mainBankLink").is_some_and(|v| !matches!(v, PyRep::None)));

    // Cloaked ship in range is severed by tick_ess_main_bank_links
    let _ = sys.set_ball_position(ship_id, DVec3::ZERO).await;
    let _ = sys.set_combat_state(ship_id, eve_destiny::CombatState { is_cloaked: true, ..Default::default() }).await;
    assert_eq!(tick_ess_main_bank_links(&ctx).await, 1);
    let _ = sys.set_combat_state(ship_id, eve_destiny::CombatState { is_cloaked: false, ..Default::default() }).await;

    // select_ess_reserve_key_in_cargo with specific target_arg
    let quad_key_tid = match system_ess_quadrant(&ctx.sde, null_sys_id) {
        EssQuadrant::Nw => 57714,
        EssQuadrant::Ne => 57716,
        EssQuadrant::Se => 57717,
        EssQuadrant::Sw => 57720,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(888_777_001, eve_db::InventoryItemRecord {
            item_id: 888_777_001,
            type_id: quad_key_tid,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..Default::default()
        });
    });
    assert!(ctx.db.with_snapshot(|snap| {
        select_ess_reserve_key_in_cargo(&ctx.sde, snap.items.values(), char_id, ship_id, null_sys_id, Some(888_777_001)).is_ok()
    }));
    assert!(ctx.db.with_snapshot(|snap| {
        select_ess_reserve_key_in_cargo(&ctx.sde, snap.items.values(), char_id, ship_id, null_sys_id, Some(999_999_999)).is_err()
    }));

    // AttemptLinkToReserveBank when unlocked via combat_runtime (pulses_total==0 auto-calc), then cooldown, then final pulse completion
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.insert(null_sys_id, cur_now + 120);
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&null_sys_id) {
            st.ess_reserve_bank = eve_db::IskCents(100_000_00);
        }
        let mut pst = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys_id);
        pst.unlocked_until_unix = 0;
        pst.pulses_total = 0;
        pst.pulses_remaining = 0;
        pst.last_pulse_sec = None;
        pst.reserve_linked_character_id = None;
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys_id, &pst);
    });
    let _ = handle_ess_link(&ctx, &sess, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)]).await.unwrap();
    // Advance clock 10 ticks so StartLink completes first pulse -> hits EssReservePulseCooldown on second attempt
    for _ in 0..6 {
        let _ = sys.try_step_full_tick(1.0);
    }
    let _ = handle_ess_link(&ctx, &sess, "CompleteReserveBankLink", &[PyRep::Integer(ess_ball_id)]).await;
    // Now on 60s cooldown or completed if 1 pulse; test explicit pulses_remaining==0 -> EssReservePulsesExhausted
    ctx.db.with_mut_snapshot(|snap| {
        let mut pst = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys_id);
        pst.pulses_remaining = 0;
        pst.reserve_linked_character_id = None;
        pst.unlocked_until_unix = cur_now + 120;
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys_id, &pst);
    });
    assert_eq!(handle_ess_link(&ctx, &sess, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)]).await.unwrap_err().code, "EssReservePulsesExhausted");

    // Empty reserve bank -> EssBankEmpty
    ctx.db.with_mut_snapshot(|snap| {
        let mut pst = crate::services::space_destiny::get_ess_reserve_pulses(snap, null_sys_id);
        pst.pulses_remaining = 1;
        pst.pulses_total = 1;
        pst.last_pulse_sec = None;
        crate::services::space_destiny::set_ess_reserve_pulses(snap, null_sys_id, &pst);
        if let Some(st) = snap.system_states.get_mut(&null_sys_id) {
            st.ess_reserve_bank = eve_db::IskCents(0);
        }
    });
    assert_eq!(handle_ess_link(&ctx, &sess, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)]).await.unwrap_err().code, "EssBankEmpty");

    // Unknown system state -> SystemStateNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.remove(&null_sys_id);
    });
    assert_eq!(handle_ess_link(&ctx, &sess, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)]).await.unwrap_err().code, "SystemStateNotFound");
    let _ = sess.solar_system_id;
}

#[cfg(test)]
#[path = "encounter_surveillance_system_extra_unit_tests.rs"]
mod encounter_surveillance_system_extra_unit_tests;
