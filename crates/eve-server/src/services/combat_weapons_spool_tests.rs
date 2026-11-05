pub(super) use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use super::*;
pub(super) use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_machonet::SessionState;
use glam::DVec3;
use std::sync::Arc;

#[path = "combat_weapons_destruction_tests.rs"]
mod combat_weapons_destruction_tests;

pub(super) async fn spawn_spool_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    shield: f64,
    armor: f64,
    hull: f64,
    cap: f64,
) -> Ball {
    let ball = Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            ball.clone(),
            CombatState {
                shield_hp: shield,
                max_shield_hp: shield.max(100.0),
                armor_hp: armor,
                max_armor_hp: armor.max(100.0),
                structure_hp: hull,
                max_structure_hp: hull.max(100.0),
                capacitor: cap,
                max_capacitor: cap.max(100.0),
                signature_radius: 120.0,
                base_signature_radius: Some(120.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    ball
}

pub(super) fn make_spool_test_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    // Vedmak / Triglavian cruiser 47271 & Battleship with Frigate Escape Bay 24690 (attr 3020=1)
    sde.types.insert(
        47_271,
        make_fighter_type(47_271, 26, 6, 10_000.0, vec![(263, 1_000.0), (265, 1_000.0), (9, 1_000.0)]),
    );
    sde.types.insert(
        24_690,
        make_fighter_type(
            24_690,
            27,
            6,
            50_000.0,
            vec![(3020, 1.0), (263, 2_000.0), (265, 2_000.0), (9, 2_000.0)],
        ),
    );
    // Capsule pod 670
    sde.types.insert(670, make_fighter_type(670, 29, 6, 500.0, vec![(9, 400.0)]));
    // Assault Frigate 11393 (escape bay eligible group 324)
    sde.types.insert(11_393, make_fighter_type(11_393, 324, 6, 2_500.0, vec![(9, 400.0)]));
    // Heavy Entropic Disintegrator 47240 (cat 7, group 1986: cycle 3000ms, opt 20km, falloff 10km, perCycle 2733=0.10, maxBonus 2734=1.50, effect 42)
    let mut disint = make_fighter_type(
        47_240,
        1986,
        7,
        10.0,
        vec![
            (54, 20_000.0),
            (158, 10_000.0),
            (160, 0.5),
            (620, 40_000.0),
            (2733, 0.10),
            (2734, 1.50),
            (137, 47_245.0),
        ],
    );
    disint.effect_ids = vec![42];
    sde.types.insert(47_240, disint);
    // Baryon Exotic Plasma charge 47245 (cat 8, group 1987)
    sde.types.insert(
        47_245,
        make_fighter_type(47_245, 1987, 8, 0.01, vec![(118, 200.0), (116, 100.0)]),
    );
    // Generic module / cargo item
    sde.types.insert(587, make_fighter_type(587, 25, 6, 2_500.0, vec![(9, 300.0)]));
    // NPC entity type 500_001 with bounty attr 481 = 150_000.0
    sde.types.insert(
        500_001,
        make_fighter_type(500_001, 550, 11, 5_000.0, vec![(481, 150_000.0)]),
    );

    ctx.sde = Arc::new(sde);
    (ctx, session, char_id, ship_id, sys_id)
}

#[test]
fn entropic_spool_lifecycle_target_switch_unlock_and_weapon_banking() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let mod1 = 740_001i64;
    let mod2 = 740_002i64;
    let other_mod = 740_003i64;
    let disint_t = ctx.sde.types.get(&47_240).unwrap();

    // Default multiplier is 1.0
    assert!((get_entropic_spool_multiplier(&ctx, ship_id, mod1) - 1.0).abs() < 1e-6);

    // Fresh activation initializes spool at 1.0
    let (cyc, mult) = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        300_000_001,
        disint_t,
        Some(740_010),
        DamageVector::new(0.0, 200.0, 0.0, 0.0),
        25.0,
        3.0,
    )
    .unwrap();
    assert_eq!(cyc, 0);
    assert!((mult - 1.0).abs() < 1e-6);

    // Advance cycles up to max cap (1.0 + 1.50 = 2.50)
    let (c1, m1) = advance_entropic_spool_cycle(&ctx, ship_id, mod1);
    assert_eq!(c1, 1);
    assert!((m1 - 1.10).abs() < 1e-6);
    for _ in 0..20 {
        advance_entropic_spool_cycle(&ctx, ship_id, mod1);
    }
    assert!((get_entropic_spool_multiplier(&ctx, ship_id, mod1) - 2.50).abs() < 1e-6);

    // Mark active in DB and try reactivating same target -> AlreadyActive
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod1);
    });
    let err = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        300_000_001,
        disint_t,
        Some(740_010),
        DamageVector::new(0.0, 200.0, 0.0, 0.0),
        25.0,
        3.0,
    )
    .unwrap_err();
    assert_eq!(err.code, "AlreadyActive");

    // Retargeting a different ball resets spool back to 1.0
    let (c_sw, m_sw) = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        300_000_002,
        disint_t,
        Some(740_010),
        DamageVector::new(0.0, 200.0, 0.0, 0.0),
        25.0,
        3.0,
    )
    .unwrap();
    assert_eq!(c_sw, 0);
    assert!((m_sw - 1.0).abs() < 1e-6);

    // Target unlock clears spools targeting unlocked ball and leaves other spools untouched
    let _ = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod2,
        300_000_003,
        disint_t,
        None,
        DamageVector::new(0.0, 200.0, 0.0, 0.0),
        25.0,
        3.0,
    )
    .unwrap();
    advance_entropic_spool_cycle(&ctx, ship_id, mod2);
    clear_entropic_spools_on_target_unlock(&ctx, ship_id, char_id, Some(300_000_002));
    assert!(!with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, mod1))));
    assert!(with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, mod2))));
    reset_entropic_spool(&ctx, ship_id, mod2);
    assert!(!with_combat_runtime(&ctx, |rt| rt.entropic_spools.contains_key(&(ship_id, mod2))));

    // LinkAllWeapons ownership violation check + weapon bank grouping
    let err_own = handle_link_all_weapons(&ctx, 999_888, ship_id).unwrap_err();
    assert_eq!(err_own.code, "ShipOwnershipViolation");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod2, make_fighter_item(mod2, 47_240, char_id, ship_id, eve_db::InventoryFlag::from_u16(27), 1));
        snap.items.insert(mod1, make_fighter_item(mod1, 47_240, char_id, ship_id, eve_db::InventoryFlag::from_u16(28), 1));
        snap.items.insert(other_mod, make_fighter_item(other_mod, 47_240, char_id, ship_id, eve_db::InventoryFlag::from_u16(29), 1));
    });
    let banks_rep = handle_link_all_weapons(&ctx, char_id, ship_id).unwrap();
    let PyRep::Dict(entries) = &banks_rep else { panic!("expected weapon banks dict") };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_i64(), Some(mod1));

    // notify_weapon_banks_updated with dissolved master emits both notifications
    notify_weapon_banks_updated(&ctx, char_id, ship_id, &[mod1], &banks_rep);
    let _ = &mut session;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_system_entropic_spools_ramps_damage_consumes_ammo_and_handles_stop_conditions() {
    let (ctx, _, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let target_id = 300_000_050i64;
    let mod1 = 740_020i64;
    let ammo1 = 740_021i64;
    let mod2 = 740_022i64;

    spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 500.0).await;
    spawn_spool_ball(&ctx, sys_id, target_id, 587, DVec3::new(5_000.0, 0.0, 0.0), 3_000.0, 1_000.0, 1_000.0, 500.0).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod1, make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(ammo1, make_fighter_item(ammo1, 47_245, char_id, ship_id, InventoryFlag::HiSlot0, 5));
        snap.items.insert(mod2, make_fighter_item(mod2, 47_240, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.module_active_states.insert(mod1);
        snap.module_active_states.insert(mod2);
    });

    let disint_t = ctx.sde.types.get(&47_240).unwrap();
    handle_entropic_activation(
        &ctx, char_id, ship_id, sys_id, mod1, target_id, disint_t, Some(ammo1),
        DamageVector::new(0.0, 300.0, 0.0, 0.0), 40.0, 3.0,
    )
    .unwrap();
    // mod2 has no ammo -> deactivates on first cycle tick
    handle_entropic_activation(
        &ctx, char_id, ship_id, sys_id, mod2, target_id, disint_t, None,
        DamageVector::new(0.0, 300.0, 0.0, 0.0), 40.0, 3.0,
    )
    .unwrap();

    step_system_entropic_spools(&ctx, sys_id, 3.1).await;
    let mult_after_1 = get_entropic_spool_multiplier(&ctx, ship_id, mod1);
    assert!(mult_after_1 > 1.0, "expected spooled multiplier > 1.0, got {mult_after_1}");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod2)), "empty mod2 should deactivate");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&ammo1).map(|i| i.quantity)), Some(4));

    // Step 2 more cycles -> continues ramping damage
    let sh_mid = sys_handle.get_combat_state(target_id).await.unwrap().unwrap().shield_hp;
    step_system_entropic_spools(&ctx, sys_id, 6.1).await;
    assert!(get_entropic_spool_multiplier(&ctx, ship_id, mod1) > mult_after_1);
    assert!(sys_handle.get_combat_state(target_id).await.unwrap().unwrap().shield_hp < sh_mid);

    // Out-of-range check stops spool cleanly
    let _ = sys_handle.set_ball_position(target_id, DVec3::new(95_000.0, 0.0, 0.0)).await;
    step_system_entropic_spools(&ctx, sys_id, 3.1).await;
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod1)), "out-of-range spool should deactivate");
}
