//! Semantic unit tests split from `fitting_module_cycles_helpers_extra_unit_tests`.

use super::fitting_module_cycles_helpers_extra_unit_tests::*;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use crate::services::inventory_fitting::fitting_module_cycles::*;
use crate::system_actor::PendingCycleAction;
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, TargetLock};
use eve_dogma::{DogmaItemState, FittedModule, ModuleState, ShipFittingEngine};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn burst_jammer_safety_block_and_pending_cycle_actions_salvage_repeat_and_overload() {
    let ctx = make_helper_ctx(|sde| {
        sde.types.insert(
            999_201,
            custom_inv_type(
                sde,
                999_201,
                80,
                7,
                &[(238, 50.0), (239, 50.0), (240, 50.0), (241, 50.0), (197, 15_000.0)],
            ),
        );
        sde.types.insert(
            999_202,
            custom_inv_type(sde, 999_202, 201, 7, &[(238, 100.0), (54, 50_000.0), (2044, 20_000.0)]),
        );
    });
    let char_id = 93_100_020i64;
    let neutral_id = 93_100_021i64;
    let sys_id = 30_000_142u32; // Jita (highsec >= 0.45)
    let ship_id = 1_000_400_201i64;
    let neutral_ship = 1_000_400_202i64;
    let burst_id = 1_000_400_203i64;
    let ecm_id = 1_000_400_204i64;

    seed_char_and_ship(&ctx, char_id, 98_000_001, sys_id, ship_id);
    seed_char_and_ship(&ctx, neutral_id, 98_000_002, sys_id, neutral_ship);

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, oid, pos) in [
        (ship_id, char_id, DVec3::ZERO),
        (neutral_ship, neutral_id, DVec3::new(2_000.0, 0.0, 0.0)),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id: bid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 500.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: oid,
                char_id: Some(oid),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    // 1. Highsec Burst Jammer with Green safety near neutral capsuleer -> SafetyBlocked
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let burst_type = ctx.sde.types.get(&999_201).unwrap().clone();
    let burst_state = DogmaItemState::from_sde(char_id, 999_201, &ctx.sde);
    let err_safety = handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, burst_id, &burst_type, &burst_state, None, 20.0,
    )
    .await
    .unwrap_err();
    assert_eq!(err_safety.code, "SafetyBlocked");

    // 2. Targeted ECM with high jam strength against locked target succeeds
    let ecm_type = ctx.sde.types.get(&999_202).unwrap().clone();
    let ecm_state = DogmaItemState::from_sde(char_id, 999_202, &ctx.sde);
    handle_ecm_or_burst_jammer_activation(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        ecm_id,
        &ecm_type,
        &ecm_state,
        Some(neutral_ship),
        15.0,
    )
    .await
    .unwrap();

    // 3. Survey scanner fallback range when ship is not in space (ship_pos == DVec3::ZERO)
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            77_100_001,
            AsteroidRecord {
                asteroid_id: 77_100_001,
                solar_system_id: sys_id,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 0, // depleted -> skipped
                initial_quantity: 500,
                radius: 500.0,
                position: (1_000.0, 0.0, 0.0),
            },
        );
    });
    let scan_res = handle_survey_scanner_activation(
        &ctx,
        &sys,
        sys_id,
        char_id,
        999_999_999, // not in ballpark -> ship_pos == ZERO
        burst_id,
        &ecm_type,
        &ecm_state,
    )
    .await;
    assert_eq!(scan_res.dict_get("count").and_then(|v| v.as_i64()), Some(0));

    // 4. execute_pending_cycle_actions: SalvageEnd (refunds deducted_cap_gj), RepeatCycleStart (deactivates on error), SyncOverloadState
    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        ecm_id,
        FittedModule {
            item_id: ecm_id,
            type_id: 999_202,
            flag_id: 19,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ecm_id,
            InventoryItemRecord {
                item_id: ecm_id,
                type_id: 999_202,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    {
        let mut st = sys.actor_state.write().unwrap();
        st.pending_cycle_actions.push(PendingCycleAction::SalvageEnd {
            char_id,
            ship_id,
            module_id: burst_id,
            module_type_id: 25861,
            wreck_id: 888_888_888, // non-existent wreck -> deactivates and refunds cap
            repeat: 1,
            effect_name: "salvaging".into(),
            next_cycle_cap_deducted: true,
            deducted_cap_gj: 120.0,
        });
        st.pending_cycle_actions.push(PendingCycleAction::RepeatCycleStart {
            char_id,
            ship_id,
            module_id: ecm_id,
            module_type_id: 999_202, // ECM without target_id -> fails and deactivates
            target_id: None,
            duration_s: 5.0,
            is_overloaded: false,
            effect_name: "targetPassively".into(),
        });
        st.pending_cycle_actions.push(PendingCycleAction::SyncOverloadState {
            char_id: Some(char_id),
            ship_id,
            module_id: ecm_id,
            is_overloaded: true,
        });
    }
    execute_pending_cycle_actions(&ctx, sys_id).await;
    let cs_after = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_after.capacitor - 620.0).abs() < 1e-3, "deducted cap should be refunded");
}

#[tokio::test]
async fn try_handle_mercden_remote_shield_repair_validation_and_remote_assistance_eval() {
    let ctx = make_helper_ctx(|sde| {
        let mt = custom_inv_type(sde, 999_599, 41, 7, &[(54, 10_000.0), (6, 50.0), (68, 1_500.0)]);
        sde.types.insert(999_599, mt);
    });
    let char_id = 93_100_030i64;
    let sys_id = 30_000_142u32;
    let ship_id = 1_000_400_301i64;
    let mod_id = 1_000_400_302i64;
    let den_id = 1_000_400_303i64;

    seed_char_and_ship(&ctx, char_id, 98_000_001, sys_id, ship_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let mod_type = ctx.sde.types.get(&999_599).unwrap().clone();
    let fm = FittedModule {
        item_id: mod_id,
        type_id: 999_599,
        flag_id: 19,
        state: ModuleState::Online,
        charge_type_id: None,
        charge_quantity: 0,
        damage_hp: 0.0,
        max_hp: 40.0,
        dynamic_attribute_overrides: HashMap::new(),
    };

    // 1. Target not locked -> TargetNotLocked
    let err_lock = try_handle_mercden_remote_shield_repair(
        &ctx, &sys, sys_id, char_id, ship_id, mod_id, &fm, &mod_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_lock.code, "TargetNotLocked");

    // Spawn ship ball uncloaked, add target lock, then cloak to test Cloaked error
    sys.spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            is_cloaked: false,
            capacitor: 10.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        TargetLock {
            target_id: den_id,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    )
    .await
    .unwrap();
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: true,
            capacitor: 10.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // 2. Cloaked -> Cloaked
    let err_cloak = try_handle_mercden_remote_shield_repair(
        &ctx, &sys, sys_id, char_id, ship_id, mod_id, &fm, &mod_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_cloak.code, "Cloaked");

    // 3. Tethered -> Tethered
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: false,
            is_tethered: true,
            capacitor: 10.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let err_teth = try_handle_mercden_remote_shield_repair(
        &ctx, &sys, sys_id, char_id, ship_id, mod_id, &fm, &mod_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_teth.code, "Tethered");

    // 4. Structure at 80km (> 40km range) -> TargetOutOfRange
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_cloaked: false,
            is_tethered: false,
            capacitor: 10.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: 85_230,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Merc Den".into(),
                state: "ShieldVulnerable".into(),
                position: [80_000.0, 0.0, 0.0],
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });
    let err_rng = try_handle_mercden_remote_shield_repair(
        &ctx, &sys, sys_id, char_id, ship_id, mod_id, &fm, &mod_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_rng.code, "TargetOutOfRange");

    // 5. Move structure within range (5km), but ship only has 10 GJ (< 50 GJ cap_need) -> NotEnoughCapacitor
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&den_id).unwrap().position = [5_000.0, 0.0, 0.0];
    });
    let err_cap = try_handle_mercden_remote_shield_repair(
        &ctx, &sys, sys_id, char_id, ship_id, mod_id, &fm, &mod_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_cap.code, "NotEnoughCapacitor");

    // 6. evaluate_module_cycle_aggression_or_assistance with None target and non-combat group returns Ok(())
    assert!(evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 55, None).await.is_ok());
    assert!(evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 46, Some(den_id)).await.is_ok());
}
