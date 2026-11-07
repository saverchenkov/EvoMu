//! Additional unit and mutation-killing tests for `combat_npcs_drones`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::*;
use eve_db::{FleetRecord, InventoryFlag};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use glam::DVec3;
use std::sync::Arc;

#[cfg(test)]
#[path = "combat_npcs_drones_dispatch_unit_tests.rs"]
mod combat_npcs_drones_dispatch_unit_tests;

pub(super) fn seed_skill(ctx: &ServerContext, char_id: i64, skill_type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut sk = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.values().next().cloned())
            .expect("seed character has skills");
        sk.type_id = skill_type_id;
        sk.level = level;
        snap.skills
            .entry(char_id)
            .or_default()
            .insert(skill_type_id, sk);
    });
}

pub(super) fn make_ball(id: i64, type_id: u32, pos: DVec3, vel: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: vel,
        max_velocity: 2_600.0,
        inertia: 1.0,
        mass: 5_000.0,
        radius: 25.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

pub(super) fn setup_drone_env() -> (ServerContext, SessionState, i64, i64, u32) {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
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
    sde.types.insert(
        28_352,
        make_fighter_type(
            28_352,
            883,
            6,
            1_000_000.0,
            vec![(1271, 125.0), (283, 1_500.0), (3223, 1.0)],
        ),
    );
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
    sde.types.insert(
        28_280,
        make_fighter_type(
            28_280,
            549,
            18,
            25.0,
            vec![
                (1272, 25.0),
                (37, 1.0),
                (54, 45_000.0),
                (158, 15_000.0),
                (160, 0.08),
                (64, 2.5),
                (114, 32.0),
                (51, 4_000.0),
            ],
        ),
    );
    sde.types.insert(
        41_030,
        make_fighter_type(41_030, 101, 18, 1_100.0, vec![(1272, 20.0), (37, 400.0)]),
    );
    sde.types.insert(
        33_681,
        make_fighter_type(33_681, 1159, 18, 5.0, vec![(1272, 5.0), (37, 1_500.0)]),
    );
    ctx.sde = Arc::new(sde);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 587;
        }
    });
    seed_skill(&ctx, char_id, 3436, 5);
    (ctx, session, char_id, ship_id, sys_id)
}

#[tokio::test]
async fn drone_space_state_math_and_step_engaged_drones_volleys() {
    let (ctx, _session, char_id, ship_id, sys_id) = setup_drone_env();
    let drone_item_id = 910_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut it = make_fighter_item(drone_item_id, 2_456, char_id, sys_id as i64, InventoryFlag::DroneBay, 1);
        it.dynamic_attributes.insert(160, 0.001); // clamped to 0.01
        it.dynamic_attributes.insert(158, 100.0); // clamped to 500.0
        it.dynamic_attributes.insert(51, 90_000.0); // clamped to 60s
        snap.items.insert(drone_item_id, it);
    });

    let mut d = DroneSpaceState {
        ball_id: 910_002,
        drone_item_id,
        type_id: 2_456,
        group_id: 100,
        owner_char_id: char_id,
        original_owner_char_id: char_id,
        parent_ship_id: ship_id,
        solar_system_id: sys_id,
        target_id: Some(910_003),
        activity_state: 1,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 2_600.0,
        optimal_range: 2_000.0,
        damage_vector: DamageVector::new(0.0, 40.0, 0.0, 0.0),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };

    assert!((d.effective_tracking_speed(&ctx) - 0.01).abs() < 1e-6);
    assert!((d.effective_falloff_range(&ctx) - 500.0).abs() < 1e-6);
    assert_eq!(d.cycle_duration_secs(&ctx), 60);

    // Clear dynamic overrides so SDE defaults apply (tracking 2.2, falloff 3000, cycle 4s)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&drone_item_id).unwrap().dynamic_attributes.clear();
    });
    assert!((d.effective_tracking_speed(&ctx) - 2.2).abs() < 1e-6);
    assert!((d.effective_falloff_range(&ctx) - 3_000.0).abs() < 1e-6);
    assert_eq!(d.cycle_duration_secs(&ctx), 4);

    // Hit multiplier branches:
    // 1) raw_dist <= 1.0
    let b_drone = make_ball(d.ball_id, 2_456, DVec3::ZERO, DVec3::ZERO);
    let mut b_target = make_ball(910_003, 587, DVec3::new(0.5, 0.0, 0.0), DVec3::new(10.0, 0.0, 0.0));
    let m_close = d.compute_combat_hit_multiplier(&ctx, &b_drone, &b_target, 40.0);
    assert!((0.05..=1.0).contains(&m_close));

    // 2) non-sentry raw_dist > 15_000.0 clamps combat_dist to 1_500.0 vs sentry uses raw_dist
    b_target.position = DVec3::new(25_000.0, 0.0, 0.0);
    b_target.velocity = DVec3::new(50.0, 0.0, 0.0); // purely radial (ortho_spd <= 1e-3)
    let m_mobile_far = d.compute_combat_hit_multiplier(&ctx, &b_drone, &b_target, 40.0);
    d.is_sentry = true;
    let m_sentry_far = d.compute_combat_hit_multiplier(&ctx, &b_drone, &b_target, 40.0);
    assert!(m_mobile_far > m_sentry_far);
    d.is_sentry = false;

    // Spawn drone and target balls and step_engaged_drones
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    b_target.position = DVec3::new(1_000.0, 0.0, 0.0);
    let _ = sys_handle
        .spawn_ball_with_combat(
            b_drone,
            CombatState {
                shield_hp: 50.0,
                max_shield_hp: 50.0,
                armor_hp: 50.0,
                max_armor_hp: 50.0,
                structure_hp: 50.0,
                max_structure_hp: 50.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            b_target,
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..Default::default()
            },
        )
        .await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(d.ball_id, d.clone());
        rt.drone_next_ready_unix.insert(d.ball_id, 100);
    });

    // Before ready time -> no volley
    step_engaged_drones(&ctx, 50).await;
    let cs_before = sys_handle.get_combat_state(910_003).await.unwrap().unwrap();
    assert!((cs_before.shield_hp - 500.0).abs() < 1e-6);

    // At ready time -> volley applied and next ready advanced by 4s
    step_engaged_drones(&ctx, 100).await;
    let cs_after = sys_handle.get_combat_state(910_003).await.unwrap().unwrap();
    assert!(cs_after.shield_hp < 500.0);
    let next_ready = with_combat_runtime(&ctx, |rt| rt.drone_next_ready_unix[&d.ball_id]);
    assert_eq!(next_ready, 104);
}
