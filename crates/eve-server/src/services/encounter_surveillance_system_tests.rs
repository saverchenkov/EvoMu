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
