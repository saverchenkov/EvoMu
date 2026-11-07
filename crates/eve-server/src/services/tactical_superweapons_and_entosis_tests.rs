//! Semantic unit tests split from `tactical_modules_tests`.

#![forbid(unsafe_code)]

use super::*;

use crate::system_actor::ActiveModuleCycle;
use eve_db::{
    FleetRecord, InventoryFlag, VirtualServerClock,
};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use glam::DVec3;
use std::sync::Arc;

#[tokio::test]
async fn disruptive_lance_superweapon_and_warmup_ballpark_hit() {
    let ctx = test_ctx_with_custom_types(|sde| {
        if let Some(mut sys) = sde.solar_systems.get(&30_000_142).cloned() {
            sys.solar_system_id = 30_002_505;
            sys.security_status = -0.5;
            sde.solar_systems.insert(30_002_505, sys);
        }
        sde.types.insert(
            999_201,
            custom_inv_type(sde, 999_201, 4594, 6, "Test Lancer Dreadnought", &[(5418, 20.0), (5417, 10.0)]),
        );
        sde.types.insert(
            77398,
            custom_inv_type(
                sde,
                77398,
                588,
                7,
                "Test Disruptive Lance",
                &[
                    (6, 10_000.0),
                    (54, 150_000.0),
                    (73, 300_000.0),
                    (114, 5_000.0),
                    (713, 16274.0),
                    (714, 1_000.0),
                    (2259, 10_000.0),
                    (2263, 2_500.0),
                    (2264, 5_000.0),
                    (2265, 1_000.0),
                    (2260, 5_000.0),
                ],
            ),
        );
    });

    let char_id = 94_000_020;
    let victim_char_id = 94_000_021;
    let lowsec_sys = 30_002_505;
    let lancer_id = 1_000_400_201;
    let victim_id = 1_000_400_202;
    let lance_mod_id = 1_000_400_203;
    let siege_mod_id = 1_000_400_204;
    let fuel_item_id = 1_000_400_205;

    ensure_char(&ctx, char_id, 98_000_010, lowsec_sys, lancer_id, 2);
    ensure_char(&ctx, victim_char_id, 98_000_011, lowsec_sys, victim_id, 1);
    insert_item(&ctx, lancer_id, 999_201, char_id, lowsec_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_id, 587, victim_char_id, lowsec_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, lance_mod_id, 77398, char_id, lancer_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, siege_mod_id, 4292, char_id, lancer_id, InventoryFlag::HiSlot1, 1, true);

    spawn_ship_ball_with_cs(&ctx, lowsec_sys, lancer_id, 999_201, char_id, DVec3::ZERO, 50_000.0).await;
    spawn_ship_ball_with_cs(&ctx, lowsec_sys, victim_id, 587, victim_char_id, DVec3::new(10_000.0, 0.0, 0.0), 10_000.0).await;

    // 1. Highsec guard
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, 30_000_142, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "CannotActivateDoomsdayInHighSec"
    );

    // 2. Requires active Siege module
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "SiegeRequiredForLance"
    );

    // 3. Requires Disruptive Lance skills (77739, 77738)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(siege_mod_id);
    });
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );

    // 4. Missing isotopes fails BEFORE draining capacitor
    grant_skill(&ctx, char_id, 77739, 4);
    grant_skill(&ctx, char_id, 77738, 4);
    let sys = ctx.systems.get_or_spawn(lowsec_sys);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
            .await
            .unwrap_err()
            .code,
        "InsufficientFuel"
    );
    assert_eq!(sys.get_combat_state(lancer_id).await.unwrap().unwrap().capacitor, 50_000.0);

    // 5. Load isotopes and fire Disruptive Lance -> hits victim 10km along X axis, sets caster/victim locks
    insert_item(&ctx, fuel_item_id, 16274, char_id, lancer_id, InventoryFlag::Cargo, 2_500, false);
    let lance_res = activate_superweapon_or_lance(&ctx, char_id, lancer_id, lowsec_sys, lance_mod_id, "ActivateSlashModule", Some(victim_id))
        .await
        .expect("lancer beam fire");
    assert_eq!(lance_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(1_000));
    assert_eq!(lance_res.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    assert!(has_lance_caster_lock(&ctx, lancer_id));
    assert_eq!(incoming_remote_repair_multiplier(&ctx, victim_id), 0.50);

    // 6. Direct fire_lance_warmup_in_ballpark queues LanceBeamFired pending action
    let mut bp = eve_destiny::Ballpark::new(lowsec_sys);
    bp.balls.insert(
        lancer_id,
        Ball {
            id: lancer_id,
            type_id: 999_201,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 2_000.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.balls.insert(
        victim_id,
        Ball {
            id: victim_id,
            type_id: 587,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    let cyc = ActiveModuleCycle {
        module_id: lance_mod_id,
        ship_id: lancer_id,
        owner_char_id: Some(char_id),
        module_type_id: 77398,
        target_id: None,
        damage_vector: DamageVector::new(1_000.0, 0.0, 0.0, 0.0),
        duration_s: 300.0,
        elapsed_s: 15.0,
        repeat: 0,
        cap_need_per_cycle: 0.0,
        lance_beam_dir: Some(DVec3::X),
        effect_name: "superWeapon".into(),
        ..Default::default()
    };
    let mut pending = Vec::new();
    let mut destroyed = Vec::new();
    fire_lance_warmup_in_ballpark(
        &mut bp,
        &cyc,
        &Some(ctx.sde.clone()),
        &Some(ctx.db.clone()),
        &ctx.tactical_state,
        &mut pending,
        &mut destroyed,
    );
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn entosis_link_warmup_contest_pause_and_command_burst_activation() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_210,
            custom_inv_type(sde, 999_210, 1313, 7, "Test Entosis Link I", &[(73, 120_000.0)]),
        );
    });

    let char_id = 94_000_030;
    let char2_id = 94_000_031;
    let sys_id = 30_002_505;
    let ship_id = 1_000_400_301;
    let ship2_id = 1_000_400_302;
    let entosis_id = 1_000_400_303;
    let entosis2_id = 1_000_400_304;
    let stront_id = 1_000_400_305;
    let stront2_id = 1_000_400_306;
    let node_id = 1_895_000_123;

    ensure_char(&ctx, char_id, 98_000_020, sys_id, ship_id, 2);
    ensure_char(&ctx, char2_id, 98_000_021, sys_id, ship2_id, 2);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, ship2_id, 587, char2_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, entosis_id, 999_210, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, entosis2_id, 999_210, char2_id, ship2_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, stront_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, stront2_id, 16275, char2_id, ship2_id, InventoryFlag::Cargo, 10, false);

    spawn_ship_ball_with_cs(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, 1_000.0).await;
    spawn_ship_ball_with_cs(&ctx, sys_id, ship2_id, 587, char2_id, DVec3::new(1_000.0, 0.0, 0.0), 1_000.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let ent_type = ctx.sde.types.get(&999_210).unwrap().clone();
    // Cycle 1: both opposing pilots start warmup cycle at t=1_800_000_000
    let c1 = handle_entosis_link_activation(&ctx, &sys, char_id, ship_id, entosis_id, &ent_type, Some(node_id))
        .await
        .expect("entosis cycle 1");
    assert_eq!(c1.dict_get("isWarmupCycle").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(c1.dict_get("captureRateMultiplier").and_then(|v| v.as_f64()), Some(0.0));
    let _ = handle_entosis_link_activation(&ctx, &sys, char2_id, ship2_id, entosis2_id, &ent_type, Some(node_id)).await;

    // Cycle 2: pilot 1 enters active capture (progressDelta == 1.0), then pilot 2 enters cycle 2 -> contests & pauses!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_310)));
    let c2 = handle_entosis_link_activation(&ctx, &sys, char_id, ship_id, entosis_id, &ent_type, Some(node_id))
        .await
        .expect("entosis cycle 2");
    assert_eq!(c2.dict_get("isWarmupCycle").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(c2.dict_get("captureRateMultiplier").and_then(|v| v.as_f64()), Some(1.0));

    let c2_contested = handle_entosis_link_activation(&ctx, &sys, char2_id, ship2_id, entosis2_id, &ent_type, Some(node_id))
        .await
        .expect("entosis contested cycle 2");
    assert_eq!(c2_contested.dict_get("isPaused").and_then(|v| v.as_bool()), Some(true));

    clear_entosis_warmup_on_unlock(&ctx, ship_id, Some(node_id));

    // Command Burst: Alpha clone guard vs Omega Fleet activation
    // 42826 = Armor Command Burst I, 42832 = Armor Energizing Charge
    if ctx.sde.types.contains_key(&42826) && ctx.sde.types.contains_key(&42832) {
        let burst_type = ctx.sde.types.get(&42826).unwrap().clone();
        let alpha_char = 94_000_040;
        let alpha_ship = 1_000_400_401;
        let alpha_burst = 1_000_400_402;
        ensure_char(&ctx, alpha_char, 98_000_020, sys_id, alpha_ship, 0);
        let fit_a = ShipFittingEngine::from_sde(alpha_ship, 587, &ctx.sde);
        let state_b = DogmaItemState::from_sde(alpha_char, 42826, &ctx.sde);
        let alpha_err = activate_command_burst(&ctx, &sys, alpha_char, alpha_ship, sys_id, alpha_burst, &burst_type, &state_b, Some(42832), &fit_a)
            .await
            .unwrap_err();
        assert_eq!(alpha_err.code, "OmegaRequiredForCommandBurst");

        let burst_mod_id = 1_000_400_410;
        let burst_charge_id = 1_000_400_411;
        insert_item(&ctx, burst_mod_id, 42826, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
        insert_item(&ctx, burst_charge_id, 42832, char_id, ship_id, InventoryFlag::HiSlot1, 200, false);
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                700_010,
                FleetRecord {
                    fleet_id: 700_010,
                    boss_id: char_id,
                    members: vec![char_id, char2_id],
                    ..Default::default()
                },
            );
            snap.characters.get_mut(&char_id).unwrap().fleet_id = Some(700_010);
            snap.characters.get_mut(&char2_id).unwrap().fleet_id = Some(700_010);
        });
        let fit_omega = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
        let state_omega = DogmaItemState::from_sde(char_id, 42826, &ctx.sde);
        let burst_ok = activate_command_burst(&ctx, &sys, char_id, ship_id, sys_id, burst_mod_id, &burst_type, &state_omega, None, &fit_omega)
            .await
            .expect("omega fleet command burst");
        assert_eq!(burst_ok.dict_get("chargeTypeID").and_then(|v| v.as_i64()), Some(42832));
        assert!(ctx
            .drain_notifications_for_char(char2_id)
            .iter()
            .any(|n| n.event == "OnDbuffUpdated"));
    }
}
