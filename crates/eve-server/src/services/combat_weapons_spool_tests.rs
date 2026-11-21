pub(super) use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use super::*;
pub(super) use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_machonet::SessionState;
use glam::DVec3;
use std::sync::Arc;

#[cfg(test)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_entropic_spool_and_deactivate_pushes_updated_fit_projection_to_actor() {
    let (mut ctx, _, char_id, ship_id, sys_id) = make_spool_test_ctx();
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        if let Some(ship_t) = sde.types.get_mut(&47_271) {
            ship_t.default_attributes.insert(eve_dogma::attr_ids::HI_SLOTS, 4.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::TURRET_SLOTS_LEFT, 4.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::CPU_OUTPUT, 500.0);
            ship_t.default_attributes.insert(eve_dogma::attr_ids::POWER_OUTPUT, 500.0);
        }
        if let Some(disint_t) = sde.types.get_mut(&47_240) {
            if !disint_t.effect_ids.contains(&12) {
                disint_t.effect_ids.push(12);
            }
        }
    }
    let target_id = 300_000_060i64;
    let mod1 = 740_030i64;
    let ammo1 = 740_031i64;
    let mod_idle = 740_032i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
        }
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 47_271, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            mod1,
            make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            ammo1,
            make_fighter_item(ammo1, 47_245, char_id, ship_id, InventoryFlag::HiSlot0, 5),
        );
        snap.items.insert(
            mod_idle,
            make_fighter_item(mod_idle, 47_240, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.module_active_states.insert(mod1);
    });

    spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 500.0).await;
    spawn_spool_ball(
        &ctx,
        sys_id,
        target_id,
        587,
        DVec3::new(5_000.0, 0.0, 0.0),
        3_000.0,
        1_000.0,
        1_000.0,
        500.0,
    )
    .await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let rev_before = ctx
        .invalidate_and_push_fit(ship_id)
        .expect("initial FitProjection pushed");
    let _ = sys_handle.get_ball_snapshot(ship_id).await;
    let proj_before = sys_handle
        .latest_fit_projection(ship_id)
        .expect("initial FitProjection stored in actor");
    assert_eq!(
        proj_before.modules.get(&mod1).map(|m| m.state),
        Some(eve_dogma::ModuleState::Active)
    );
    assert!(
        !matches!(
            proj_before.modules.get(&mod_idle).map(|m| m.state),
            Some(eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded)
        ),
        "idle module must remain inactive before spool stop"
    );

    let disint_t = ctx.sde.types.get(&47_240).unwrap();
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        target_id,
        disint_t,
        Some(ammo1),
        DamageVector::new(0.0, 300.0, 0.0, 0.0),
        40.0,
        3.0,
    )
    .unwrap();

    stop_entropic_spool_and_deactivate(&ctx, char_id, ship_id, mod1);
    let _ = sys_handle.get_ball_snapshot(ship_id).await;

    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod1)));
    let rev_after = ctx.current_fit_rev(ship_id);
    assert!(
        rev_after.0 > rev_before.0,
        "stop_entropic_spool_and_deactivate must advance FitRev (before={}, after={})",
        rev_before.0,
        rev_after.0
    );
    let proj_after = sys_handle
        .latest_fit_projection(ship_id)
        .expect("stop_entropic_spool_and_deactivate must push updated FitProjection");
    assert_eq!(proj_after.rev, rev_after);
    assert!(
        !matches!(
            proj_after.modules.get(&mod1).map(|m| m.state),
            Some(eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded)
        ),
        "deactivated entropic module must no longer be Active in actor FitProjection"
    );
    assert!(
        !matches!(
            proj_after.modules.get(&mod_idle).map(|m| m.state),
            Some(eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded)
        ),
        "unrelated module state must remain unchanged"
    );
}

#[tokio::test]
async fn entropic_spool_ammo_depletion_emits_tombstone_and_sublocation_expel_gh1124() {
    let (ctx, session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let mod1 = 740_901_i64;
    let ammo1 = 740_902_i64;
    let target_id = 1_896_901_i64;

    ctx.db
        .acquire_online_lock(session.user_id.unwrap_or(1), 999_201, Some(char_id), true, ctx.now_unix())
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod1,
            make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        let mut ammo_rec = make_fighter_item(ammo1, 47_245, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        ammo_rec.singleton = false;
        snap.items.insert(ammo1, ammo_rec);
        snap.module_active_states.insert(mod1);
        snap.module_active_charges.insert(mod1, 47_245);
    });
    spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 500.0).await;
    spawn_spool_ball(&ctx, sys_id, target_id, 587, DVec3::new(5_000.0, 0.0, 0.0), 3_000.0, 1_000.0, 1_000.0, 500.0).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();

    let disint_t = ctx.sde.types.get(&47_240).unwrap();
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        target_id,
        disint_t,
        Some(ammo1),
        DamageVector::new(0.0, 300.0, 0.0, 0.0),
        40.0,
        3.0,
    )
    .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    step_system_entropic_spools(&ctx, sys_id, 3.1).await;

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ammo1), "entropic last round must be removed from snap.items");
        assert!(!snap.module_active_charges.contains_key(&mod1), "depleted module_active_charges must be removed");
    });

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(ammo1))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(0))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "entropic 1 -> 0 round depletion must emit OnItemChange tombstone"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(rows)) if rows.iter().any(|r| {
                    matches!(r, PyRep::Tuple(cols)
                        if cols.get(3) == Some(&PyRep::Integer(805))
                            && cols.get(5) == Some(&PyRep::Real(0.0))
                            && cols.get(6) == Some(&PyRep::Real(1.0)))
                }))
        }),
        "entropic 1 -> 0 round depletion must emit OnModuleAttributeChanges(805: 1.0 -> 0.0)"
    );

    step_system_entropic_spools(&ctx, sys_id, 3.1).await;
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod1)), "next cycle with 0 rounds stops module");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entropic_spool_rejects_untrained_charge_on_activation_and_step_gh1124() {
    let (mut ctx, session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let mut sde = (*ctx.sde).clone();
    if let Some(ch) = sde.types.get_mut(&47_245) {
        ch.default_attributes.insert(182, 12_210.0);
        ch.default_attributes.insert(277, 1.0);
    }
    ctx.sde = Arc::new(sde);

    let mod1 = 740_951_i64;
    let ammo1 = 740_952_i64;
    let target_id = 1_896_951_i64;
    ctx.db
        .acquire_online_lock(session.user_id.unwrap_or(1), 999_202, Some(char_id), true, ctx.now_unix())
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod1,
            make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        let mut ammo_rec = make_fighter_item(ammo1, 47_245, char_id, ship_id, InventoryFlag::HiSlot0, 5);
        ammo_rec.singleton = false;
        snap.items.insert(ammo1, ammo_rec);
    });
    spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 500.0).await;
    spawn_spool_ball(&ctx, sys_id, target_id, 587, DVec3::new(5_000.0, 0.0, 0.0), 3_000.0, 1_000.0, 1_000.0, 500.0).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();

    let disint_t = ctx.sde.types.get(&47_240).unwrap();
    let err = handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        target_id,
        disint_t,
        Some(ammo1),
        DamageVector::new(0.0, 300.0, 0.0, 0.0),
        40.0,
        3.0,
    )
    .expect_err("untrained Entropic charge must fail handle_entropic_activation");
    assert_eq!(err.code, "SkillPrerequisitesNotMet");

    // Train skill 12210 level 1 -> activation succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            12_210,
            eve_db::TrainedSkillRecord {
                type_id: 12_210,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.module_active_states.insert(mod1);
        snap.module_active_charges.insert(mod1, 47_245);
    });
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        target_id,
        disint_t,
        Some(ammo1),
        DamageVector::new(0.0, 300.0, 0.0, 0.0),
        40.0,
        3.0,
    )
    .expect("trained Entropic charge activation succeeds");

    // Remove prerequisite skill before next spool cycle -> step stops cleanly without consuming ammo
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.skills.get_mut(&char_id) {
            m.remove(&12_210);
        }
    });
    step_system_entropic_spools(&ctx, sys_id, 3.1).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ammo1].quantity, 5, "untrained spool step must not consume charge");
        assert!(!snap.module_active_states.contains(&mod1), "untrained spool step must stop module");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_system_entropic_spools_uses_surface_distance_on_capital_hull_1247() {
    let (mut ctx, _, char_id, ship_id, sys_id) = make_spool_test_ctx();
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        if let Some(disint) = sde.types.get_mut(&47_240) {
            disint.default_attributes.insert(54, 10_000.0);
            disint.default_attributes.insert(158, 1_000.0);
        }
    }
    let target_id = 300_000_090i64;
    let mod1 = 740_090i64;
    let ammo1 = 740_091i64;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Capital hull radius 6,800m at origin + target radius 40m at x=15,000m:
    // center distance = 15,000m (> 13,000m cutoff), surface distance = 8,160m (< 10,000m optimal).
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 47_271,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 6_800.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 10_000.0,
                max_shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                max_armor_hp: 10_000.0,
                structure_hp: 10_000.0,
                max_structure_hp: 10_000.0,
                capacitor: 5_000.0,
                max_capacitor: 5_000.0,
                signature_radius: 10_000.0,
                base_signature_radius: Some(10_000.0),
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    spawn_spool_ball(&ctx, sys_id, target_id, 587, DVec3::new(15_000.0, 0.0, 0.0), 3_000.0, 1_000.0, 1_000.0, 500.0).await;
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            12_210,
            eve_db::TrainedSkillRecord {
                type_id: 12_210,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.items.insert(mod1, make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        let mut ammo_rec = make_fighter_item(ammo1, 47_245, char_id, ship_id, InventoryFlag::HiSlot0, 5);
        ammo_rec.singleton = false;
        snap.items.insert(ammo1, ammo_rec);
        snap.module_active_states.insert(mod1);
        snap.module_active_charges.insert(mod1, 47_245);
    });

    let disint_t = ctx.sde.types.get(&47_240).unwrap();
    handle_entropic_activation(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod1,
        target_id,
        disint_t,
        Some(ammo1),
        DamageVector::new(0.0, 300.0, 0.0, 0.0),
        40.0,
        3.0,
    )
    .unwrap();

    step_system_entropic_spools(&ctx, sys_id, 3.1).await;
    assert!(
        get_entropic_spool_multiplier(&ctx, ship_id, mod1) > 1.0,
        "entropic spool must ramp at 8,160m surface distance on 6,800m hull despite 15,000m center distance"
    );
    assert!(
        sys_handle.get_combat_state(target_id).await.unwrap().unwrap().shield_hp < 3_000.0,
        "entropic spool must apply shield damage within surface range"
    );
    assert!(
        ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod1)),
        "in-range surface-distance spool must remain active"
    );

    // Move target to x=20,000m -> surface distance = 13,160m > 13,000m cutoff -> spool stops cleanly.
    let _ = sys_handle.set_ball_position(target_id, DVec3::new(20_000.0, 0.0, 0.0)).await;
    step_system_entropic_spools(&ctx, sys_id, 3.1).await;
    assert!(
        !ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod1)),
        "out-of-range spool past 13,000m surface distance must stop cleanly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_destruction_turret_on_unpiloted_corp_ship_attributes_corp_not_ceo() {
    let (ctx, _, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();
    let corp_id = 98_999_104i64;
    let ally_id = 99_999_104i64;
    let ceo_id = 90_999_104i64;
    let ceo_ship = 200_999_104i64;
    let corp_ship_id = 1_990_104_001i64;

    spawn_spool_ball(&ctx, sys_id, attacker_ship, 47_271, DVec3::ZERO, 10_000.0, 10_000.0, 10_000.0, 5_000.0).await;
    let corp_ship_ball = spawn_spool_ball(
        &ctx,
        sys_id,
        corp_ship_id,
        587,
        DVec3::new(2_000.0, 0.0, 0.0),
        0.0,
        0.0,
        1.0,
        0.0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ceo = snap.characters[&attacker_id].clone();
        ceo.character_id = ceo_id;
        ceo.name = "Corp CEO".to_string();
        ceo.corporation_id = corp_id;
        ceo.alliance_id = Some(ally_id);
        ceo.active_ship_id = ceo_ship;
        snap.characters.insert(ceo_id, ceo);

        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Target Spool Corp".to_string();
        corp.ticker = "TSCRP".to_string();
        corp.ceo_id = ceo_id;
        corp.alliance_id = Some(ally_id);
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ceo_ship,
            make_fighter_item(ceo_ship, 587, ceo_id, i64::from(sys_id), InventoryFlag::None, 1),
        );
        snap.items.insert(
            corp_ship_id,
            make_fighter_item(corp_ship_id, 587, corp_id, i64::from(sys_id), InventoryFlag::None, 1),
        );
        snap.item_positions.insert(corp_ship_id, [2_000.0, 0.0, 0.0]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(corp_ship_id, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(corp_ship_id, eve_db::PersistedCombatHp::default());
    });

    // Simulate turret handler passing `player_target_info = Some((corp.ceo_id, corp_ship_id))`.
    let res = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        corp_ship_id,
        &corp_ship_ball,
        Some((ceo_id, corp_ship_id)),
        sys_id,
        250.0,
        now_unix,
    )
    .await;

    assert!(res.kill_id > 0, "corp ship turret kill must emit killmail");
    assert!(res.wreck_id > 0, "corp ship turret kill must spawn wreck");
    assert_eq!(res.corpse_id, 0);
    assert!(
        sys_handle.get_ball_snapshot(corp_ship_id).await.unwrap().is_none(),
        "corp ship ball must be removed from Ballpark"
    );
    let wreck_slim = sys_handle
        .get_ball_slim_item(res.wreck_id)
        .await
        .unwrap()
        .expect("corp ship wreck slim item present");
    assert_eq!(wreck_slim.dict_get("ownerID").and_then(eve_proto::PyRep::as_i64), Some(corp_id));
    assert_eq!(wreck_slim.dict_get("corpID").and_then(eve_proto::PyRep::as_i64), Some(corp_id));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&corp_ship_id));
        assert!(!snap.item_positions.contains_key(&corp_ship_id));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&corp_ship_id));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&corp_ship_id));
        assert_eq!(snap.items[&res.wreck_id].owner_id, corp_id);
        let km = &snap.killmails[&res.kill_id];
        assert_eq!(km.victim_character_id, 0, "must not misattribute corp ship loss to CEO");
        assert_eq!(km.victim_corp_id, corp_id);
        assert_eq!(km.victim_alliance_id, Some(ally_id));
        assert_eq!(km.attacker_character_id, attacker_id);
        assert_eq!(snap.characters[&ceo_id].active_ship_id, ceo_ship, "CEO must not be podded");
        assert!(!snap.mails.iter().any(|m| m.recipient_id <= 0));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_system_entropic_spools_destroys_unpiloted_corp_ship_without_npc_fallback() {
    let (ctx, _, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let corp_id = 98_999_105i64;
    let ally_id = 99_999_105i64;
    let corp_ship_id = 1_990_104_002i64;
    let mod1 = 740_105i64;
    let ammo1 = 740_106i64;

    spawn_spool_ball(&ctx, sys_id, attacker_ship, 47_271, DVec3::ZERO, 10_000.0, 10_000.0, 10_000.0, 5_000.0).await;
    spawn_spool_ball(
        &ctx,
        sys_id,
        corp_ship_id,
        587,
        DVec3::new(1_000.0, 0.0, 0.0),
        1.0,
        1.0,
        1.0,
        0.0,
    )
    .await;
    sys_handle
        .add_target_lock(
            attacker_ship,
            TargetLock {
                target_id: corp_ship_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Spool Victim Corp".to_string();
        corp.ticker = "SVCRP".to_string();
        corp.alliance_id = Some(ally_id);
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            corp_ship_id,
            make_fighter_item(corp_ship_id, 587, corp_id, i64::from(sys_id), InventoryFlag::None, 1),
        );
        snap.item_positions.insert(corp_ship_id, [1_000.0, 0.0, 0.0]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(corp_ship_id, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(corp_ship_id, eve_db::PersistedCombatHp::default());

        snap.skills.entry(attacker_id).or_default().insert(
            12_210,
            eve_db::TrainedSkillRecord {
                type_id: 12_210,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.items.insert(mod1, make_fighter_item(mod1, 47_240, attacker_id, attacker_ship, InventoryFlag::HiSlot0, 1));
        let mut ammo_rec = make_fighter_item(ammo1, 47_245, attacker_id, attacker_ship, InventoryFlag::HiSlot0, 5);
        ammo_rec.singleton = false;
        snap.items.insert(ammo1, ammo_rec);
        snap.module_active_states.insert(mod1);
        snap.module_active_charges.insert(mod1, 47_245);
    });

    let km_before = ctx.db.with_snapshot(|s| s.killmails.len());
    let disint_t = ctx.sde.types.get(&47_240).unwrap();
    handle_entropic_activation(
        &ctx,
        attacker_id,
        attacker_ship,
        sys_id,
        mod1,
        corp_ship_id,
        disint_t,
        Some(ammo1),
        DamageVector::new(0.0, 500.0, 0.0, 0.0),
        40.0,
        3.0,
    )
    .unwrap();

    step_system_entropic_spools(&ctx, sys_id, 3.1).await;

    assert!(
        sys_handle.get_ball_snapshot(corp_ship_id).await.unwrap().is_none(),
        "entropic spool kill must remove unpiloted corp ship from Ballpark"
    );
    let expected_wreck_tid = ctx.sde.resolve_wreck_type_id(587, None).unwrap_or(26_557);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&corp_ship_id));
        assert!(!snap.item_positions.contains_key(&corp_ship_id));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&corp_ship_id));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&corp_ship_id));
        assert!(!snap.module_active_states.contains(&mod1));
        assert!(
            snap.committed_idempotency_keys
                .contains(&(corp_ship_id, "ship_insurance_payout".to_string())),
            "entropic spool kill of unpiloted corp ship must execute death pipeline, not NPC fallback"
        );
        assert_eq!(snap.killmails.len(), km_before + 1);
        let km = snap
            .killmails
            .values()
            .find(|k| k.victim_corp_id == corp_id && k.ship_type_id == 587)
            .expect("corporate killmail emitted");
        assert_eq!(km.victim_character_id, 0);
        assert_eq!(km.victim_alliance_id, Some(ally_id));
        assert!(
            snap.items
                .values()
                .any(|it| it.type_id == expected_wreck_tid && it.owner_id == corp_id),
            "wreck must belong to victim corporation, not attacker NPC fallback"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_ship_or_npc_destruction_npc_wreck_populates_sde_type_name() {
    let (mut ctx, _, attacker_id, attacker_ship, sys_id) = make_spool_test_ctx();
    let mut sde = (*ctx.sde).clone();
    let mut wreck_type = make_fighter_type(26_557, 186, 2, 10_000.0, Vec::new());
    wreck_type.name = "Frigate Wreck".to_string();
    sde.types.insert(26_557, wreck_type);
    sde.wrecks.insert(500_001, 26_557);
    ctx.sde = Arc::new(sde);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();
    let npc_id = 1_896_000_123i64;
    let npc_ball = spawn_spool_ball(
        &ctx,
        sys_id,
        npc_id,
        500_001,
        DVec3::new(1_000.0, 0.0, 0.0),
        0.0,
        0.0,
        10.0,
        0.0,
    )
    .await;

    let res = handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker_id,
        attacker_ship,
        47_271,
        47_240,
        npc_id,
        &npc_ball,
        None,
        sys_id,
        1_250.0,
        now_unix,
    )
    .await;

    assert!(res.wreck_id > 0, "NPC turret kill must spawn wreck");
    let wreck_slim = sys_handle
        .get_ball_slim_item(res.wreck_id)
        .await
        .unwrap()
        .expect("NPC wreck slim item present");
    assert_eq!(
        wreck_slim.dict_get("name"),
        Some(&eve_proto::PyRep::String("Frigate Wreck".into()))
    );
}

#[tokio::test]
async fn handle_link_all_weapons_atomic_guards_state_and_preserves_existing_banks() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let other_ship_id = 749_100i64;
    let mod1 = 749_201i64;
    let mod2 = 749_202i64;
    let mod_cargo = 749_203i64;
    let other_mod1 = 749_204i64;
    let other_mod2 = 749_205i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            other_ship_id,
            make_fighter_item(other_ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1),
        );
        snap.items.insert(
            mod1,
            make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            mod2,
            make_fighter_item(mod2, 47_240, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            mod_cargo,
            make_fighter_item(mod_cargo, 47_240, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
        snap.items.insert(
            other_mod1,
            make_fighter_item(other_mod1, 47_240, char_id, other_ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            other_mod2,
            make_fighter_item(other_mod2, 47_240, char_id, other_ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.weapon_banks
            .insert(ship_id, std::collections::HashMap::from([(mod1, vec![mod1, mod2])]));
    });

    let expected_banks = std::collections::HashMap::from([(mod1, vec![mod1, mod2])]);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 1_000.0).await;

    // 1. TOCTOU DB docked (session still appears in space, DB character is docked in station).
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    let err = handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, ship_id, true)
        .unwrap_err();
    assert_eq!(err.code, "CannotLinkWeaponsWhileDocked");
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.weapon_banks.get(&ship_id), Some(&expected_banks));
    });

    // 2. TOCTOU ship stored in station while DB character appears undocked.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.items.get_mut(&ship_id).unwrap().location_id = 60_003_760;
    });
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons(&ctx, char_id, ship_id).unwrap_err().code,
        "CannotLinkWeaponsWhileDocked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    // 3. Session docked even when DB ship/character are in space.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(sys_id);
    });
    session.structure_id = Some(1_000_000_000_001);
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, ship_id, true)
            .unwrap_err()
            .code,
        "CannotLinkWeaponsWhileDocked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    session.structure_id = None;

    // 4. Non-active owned ship is rejected atomically.
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons(&ctx, char_id, other_ship_id).unwrap_err().code,
        "ShipNotActive"
    );
    assert_eq!(
        handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, other_ship_id, true)
            .unwrap_err()
            .code,
        "ShipNotActive"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    // 5. Cloaked and gate-cloaked states fail without mutating banks.
    sys.modify_combat_state(ship_id, |cs| cs.is_cloaked = true).await.unwrap();
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, ship_id, true)
            .unwrap_err()
            .code,
        "CannotLinkWeaponsWhileCloaked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    sys.modify_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 10;
    })
    .await
    .unwrap();
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons(&ctx, char_id, ship_id).unwrap_err().code,
        "CannotLinkWeaponsWhileCloaked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    sys.modify_combat_state(ship_id, |cs| cs.gate_cloak_remaining_ticks = 0)
        .await
        .unwrap();

    // 6. With < 2 fitted weapons: require_linkable_pair=true rejects & preserves weapon_banks;
    // require_linkable_pair=false clears stale banks cleanly.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod2).unwrap().flag = InventoryFlag::Cargo;
    });
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, ship_id, true)
            .unwrap_err()
            .code,
        "InvalidWeaponGroup"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.weapon_banks.get(&ship_id), Some(&expected_banks));
    });

    let empty = handle_link_all_weapons(&ctx, char_id, ship_id).expect("require_linkable_pair=false clears stale bank");
    assert_eq!(empty, eve_proto::PyRep::Dict(Vec::new()));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.weapon_banks.get(&ship_id).is_none_or(std::collections::HashMap::is_empty));
    });
}

#[tokio::test]
async fn handle_link_all_weapons_atomic_excludes_offline_weapons() {
    let (ctx, session, char_id, ship_id, sys_id) = make_spool_test_ctx();
    let mod1 = 749_301i64;
    let mod2 = 749_302i64;
    let mod3 = 749_303i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod1,
            make_fighter_item(mod1, 47_240, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            mod2,
            make_fighter_item(mod2, 47_240, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            mod3,
            make_fighter_item(mod3, 47_240, char_id, ship_id, InventoryFlag::HiSlot2, 1),
        );
        snap.module_online_states.insert(mod1, true);
        snap.module_online_states.insert(mod2, false);
        snap.module_online_states.insert(mod3, false);
    });
    let _ = spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 1_000.0).await;

    // Only 1 online weapon -> require_linkable_pair=true rejects; require_linkable_pair=false stays empty.
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, ship_id, true)
            .unwrap_err()
            .code,
        "InvalidWeaponGroup"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    let empty = handle_link_all_weapons(&ctx, char_id, ship_id).unwrap();
    assert_eq!(empty, eve_proto::PyRep::Dict(Vec::new()));

    // Bringing mod2 online links only [mod1, mod2] and excludes offline mod3.
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod2, true);
    });
    handle_link_all_weapons_atomic(&ctx, Some(&session), char_id, ship_id, true)
        .expect("two online identical weapons link");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.weapon_banks.get(&ship_id),
            Some(&std::collections::HashMap::from([(mod1, vec![mod1, mod2])]))
        );
    });
}

#[tokio::test]
async fn is_linkable_weapon_type_rejects_charge_group_non_weapons_and_mining_lasers() {
    let (ctx, sess, char_id, ship_id, sys_id) = make_spool_test_ctx();

    let positive_types = [
        450u32,  // Gatling Pulse Laser I (group 53)
        484,     // 125mm Gatling AutoCannon I (group 55)
        561,     // 75mm Gatling Rail I (group 74)
        47_272,  // Light Entropic Disintegrator I (group 1986)
        54_739,  // Small Vorton Projector I (group 4060)
        13_320,  // Cruise Missile Launcher I (group 506)
        499,     // Light Missile Launcher I (group 509)
        501,     // Heavy Missile Launcher I (group 510)
        35_921,  // Standup Anticapital Missile Launcher I (cat 66, group 1327)
        35_922,  // Standup Multirole Missile Launcher I (cat 66, group 1562)
        47_240,  // Synthetic test disintegrator turret
    ];
    for tid in positive_types {
        let ty = ctx
            .sde
            .types
            .get(&tid)
            .unwrap_or_else(|| panic!("missing positive type {tid}"));
        assert!(
            is_linkable_weapon_type(ty),
            "expected linkable weapon for {tid} ({})",
            ty.name
        );
    }

    let negative_types = [
        483u32,  // Miner I (turretFitted group 54)
        18_068,  // Modulated Deep Core Miner II (turretFitted + chargeGroup1)
        25_266,  // Gas Cloud Scoop I (turretFitted group 737)
        17_482,  // Strip Miner I (group 464)
        17_912,  // Modulated Strip Miner II (chargeGroup1)
        42_526,  // Armor Command Burst I (chargeGroup1)
        41_476,  // Small Ancillary Remote Armor Repairer (chargeGroup1)
        41_480,  // Small Ancillary Remote Shield Booster (chargeGroup1)
        4_258,   // Core Probe Launcher II (chargeGroup1)
        27_914,  // Bomb Launcher I (chargeGroup1)
        35_923,  // Standup Guided Bomb Launcher I (chargeGroup1)
        22_782,  // Interdiction Sphere Launcher I (chargeGroup1)
        85_084,  // Small Breacher Pod Launcher (chargeGroup1)
        19_660,  // Festival Launcher (chargeGroup1)
        35_926,  // Standup Point Defense Battery I (chargeGroup1)
        35_928,  // Standup Arcing Vorton Projector I (Doomsday)
    ];
    for tid in negative_types {
        let ty = ctx
            .sde
            .types
            .get(&tid)
            .unwrap_or_else(|| panic!("missing negative type {tid}"));
        assert!(
            !is_linkable_weapon_type(ty),
            "non-weapon type {tid} ({}) must not be linkable",
            ty.name
        );
    }

    let mut wrong_group = ctx.sde.types.get(&450).expect("450").clone();
    wrong_group.group_id = 54;
    assert!(!is_linkable_weapon_type(&wrong_group));

    let mut missing_effect = ctx.sde.types.get(&450).expect("450").clone();
    missing_effect.effect_ids.clear();
    assert!(!is_linkable_weapon_type(&missing_effect));

    let _ = spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 1_000.0).await;

    let mod_a = 9_476_101i64;
    let mod_b = 9_476_102i64;
    for bad_tid in [42_526u32, 17_912, 483, 41_476, 4_258, 27_914] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                mod_a,
                eve_db::InventoryItemRecord {
                    item_id: mod_a,
                    type_id: bad_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                mod_b,
                eve_db::InventoryItemRecord {
                    item_id: mod_b,
                    type_id: bad_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: eve_db::InventoryFlag::HiSlot1,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        let err = handle_link_all_weapons_atomic(&ctx, Some(&sess), char_id, ship_id, true)
            .expect_err("non-weapon pair must not link");
        assert_eq!(err.code, "InvalidWeaponGroup");
        ctx.db.with_snapshot(|snap| {
            assert!(snap.weapon_banks.get(&ship_id).map_or(true, |b| b.is_empty()));
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_a).expect("mod_a").type_id = 450;
        snap.items.get_mut(&mod_b).expect("mod_b").type_id = 450;
    });
    assert!(handle_link_all_weapons_atomic(&ctx, Some(&sess), char_id, ship_id, true).is_ok());
    ctx.db.with_snapshot(|snap| {
        let banks = snap.weapon_banks.get(&ship_id).expect("banks");
        assert_eq!(banks.get(&mod_a), Some(&vec![mod_a, mod_b]));
    });
}

#[tokio::test]
async fn handle_link_all_weapons_atomic_emits_weapon_group_destroyed_for_removed_masters() {
    let (ctx, sess, char_id, ship_id, sys_id) = make_spool_test_ctx();
    ctx.db
        .acquire_online_lock(
            sess.user_id.unwrap_or(1),
            999_476,
            Some(char_id),
            true,
            ctx.now_unix(),
        )
        .unwrap();
    let _ = spawn_spool_ball(&ctx, sys_id, ship_id, 47_271, DVec3::ZERO, 1_000.0, 1_000.0, 1_000.0, 1_000.0).await;

    let m1 = 9_476_201i64;
    let m2 = 9_476_202i64;
    let m3 = 9_476_203i64;
    let old_k1 = 9_476_210i64;
    let old_k2 = 9_476_211i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (item_id, type_id, flag) in [
            (m1, 47_240u32, eve_db::InventoryFlag::HiSlot0),
            (m2, 47_240, eve_db::InventoryFlag::HiSlot1),
            (m3, 47_240, eve_db::InventoryFlag::HiSlot2),
            (old_k1, 450, eve_db::InventoryFlag::HiSlot3),
            (old_k2, 450, eve_db::InventoryFlag::HiSlot4),
        ] {
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.weapon_banks.insert(
            ship_id,
            std::collections::HashMap::from([(m2, vec![m2, m3]), (old_k1, vec![old_k1, old_k2])]),
        );
        snap.module_online_states.insert(old_k2, false);
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    assert!(handle_link_all_weapons_atomic(&ctx, Some(&sess), char_id, ship_id, true).is_ok());
    let notifs = ctx.drain_notifications_for_char(char_id);
    let destroyed: Vec<(i64, i64)> = notifs
        .iter()
        .filter(|n| n.event == "OnWeaponGroupDestroyed")
        .map(|n| match n.args.as_slice() {
            [eve_proto::PyRep::Integer(s), eve_proto::PyRep::Integer(m)] => (*s, *m),
            other => panic!("unexpected OnWeaponGroupDestroyed args: {other:?}"),
        })
        .collect();
    assert_eq!(destroyed, vec![(ship_id, m2), (ship_id, old_k1)]);
    assert_eq!(
        notifs.iter().filter(|n| n.event == "OnWeaponBanksChanged").count(),
        1
    );
    ctx.db.with_snapshot(|snap| {
        let banks = snap.weapon_banks.get(&ship_id).expect("banks");
        assert_eq!(banks.len(), 1);
        assert_eq!(banks.get(&m1), Some(&vec![m1, m2, m3]));
    });

    assert!(handle_link_all_weapons_atomic(&ctx, Some(&sess), char_id, ship_id, true).is_ok());
    let idem_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        idem_notifs.iter().filter(|n| n.event == "OnWeaponGroupDestroyed").count(),
        0
    );
    assert_eq!(
        idem_notifs.iter().filter(|n| n.event == "OnWeaponBanksChanged").count(),
        1
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(m2, false);
        snap.module_online_states.insert(m3, false);
    });
    assert!(handle_link_all_weapons_atomic(&ctx, Some(&sess), char_id, ship_id, false).is_ok());
    let dissolve_notifs = ctx.drain_notifications_for_char(char_id);
    let dissolved: Vec<(i64, i64)> = dissolve_notifs
        .iter()
        .filter(|n| n.event == "OnWeaponGroupDestroyed")
        .map(|n| match n.args.as_slice() {
            [eve_proto::PyRep::Integer(s), eve_proto::PyRep::Integer(m)] => (*s, *m),
            other => panic!("unexpected OnWeaponGroupDestroyed args: {other:?}"),
        })
        .collect();
    assert_eq!(dissolved, vec![(ship_id, m1)]);
    assert_eq!(
        dissolve_notifs.iter().filter(|n| n.event == "OnWeaponBanksChanged").count(),
        1
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.weapon_banks.get(&ship_id).map_or(true, |b| b.is_empty()));
    });
}
