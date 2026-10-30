//! Integration test suite for Abyssal Deadspace weathers, spatial hazard clouds,
//! automated pylons, trace scanning/collapse, and timer collapse podding.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites_abyssal::{
    apply_abyssal_weather_dogma_modifiers_to_ship, apply_abyssal_weather_to_combat_and_nav,
    evaluate_abyssal_spatial_clouds_for_ship, pulse_abyssal_automated_pylons,
    restore_abyssal_weather_dogma_modifiers, step_abyssal_hazards_and_pylons_in_ballpark,
    AbyssalWeatherKind, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID,
    BIOLUMINESCENT_CLOUD_TYPE_ID, FILAMENT_CLOUD_TYPE_ID,
    MULTIBODY_TRACKING_PYLON_TYPE_ID, SPATIAL_CLOUD_RADIUS_M,
    TACHYON_CLOUD_TYPE_ID,
};
use crate::common;

#[tokio::test]
async fn test_abyssal_weather_all_types_and_tier_scaling() {
    let ctx = ServerContext::new();
    let char_id = 90_000_101_i64;
    let ship_id = 100_000_101_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Test tier scaling: T0/T1 -> 30%, T2/T3 -> 50%, T4/T5/T6 -> 70%
    for (tier, expected_pct) in [
        (0_u8, 30.0),
        (1_u8, 30.0),
        (2_u8, 50.0),
        (3_u8, 50.0),
        (4_u8, 70.0),
        (5_u8, 70.0),
        (6_u8, 70.0),
    ] {
        let (penalty_pct, bonus_pct) = match tier {
            0 | 1 => (30.0, 30.0),
            2 | 3 => (50.0, 50.0),
            _ => (70.0, 70.0),
        };
        assert_eq!(penalty_pct, expected_pct);
        assert_eq!(bonus_pct, expected_pct);

        // Test all 5 weather types
        for weather in [
            AbyssalWeatherKind::Electrical,
            AbyssalWeatherKind::Dark,
            AbyssalWeatherKind::Exotic,
            AbyssalWeatherKind::Firestorm,
            AbyssalWeatherKind::Gamma,
        ] {
            let mut cs = ctx.build_type_combat_state(621);
            let mut max_vel = 250.0;
            let mut opt_range = 15_000.0;

            apply_abyssal_weather_to_combat_and_nav(weather, penalty_pct, bonus_pct, &mut cs, &mut max_vel, &mut opt_range);
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(cs.shield_resonance[0] > 0.5 && cs.armor_resonance[0] > 0.5, "Electrical storm penalizes EM shield & armor resonance");
                }
                AbyssalWeatherKind::Dark => {
                    assert!(max_vel > 250.0 && opt_range < 15_000.0, "Dark weather boosts velocity and reduces turret optimal");
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(cs.shield_resonance[2] > 0.6 && cs.armor_resonance[2] > 0.75, "Exotic weather penalizes Kinetic shield & armor resonance");
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(cs.max_armor_hp > 1000.0 && cs.shield_resonance[1] > 0.8, "Firestorm boosts armor HP and penalizes Thermal resonance");
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(cs.max_shield_hp > 1000.0 && cs.shield_resonance[3] > 0.5, "Gamma boosts shield HP and penalizes Explosive resonance");
                }
            }
            let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, ship_id, weather, penalty_pct, bonus_pct);
            assert!(!saved.is_empty(), "Saved pre-weather attributes");
            let dyn_attrs = ctx.db.with_snapshot(|s| s.items.get(&ship_id).unwrap().dynamic_attributes.clone());
            let base_ty = ctx.sde.types.get(&621).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => assert!(dyn_attrs[&55] < base_ty.default_attributes[&55], "Electrical lowers cap recharge time (attr 55)"),
                AbyssalWeatherKind::Dark => {
                    assert!(dyn_attrs[&37] > base_ty.default_attributes[&37], "Dark boosts max velocity (attr 37)");
                    assert!(dyn_attrs.contains_key(&54), "Dark penalizes optimal range (attr 54)");
                }
                AbyssalWeatherKind::Exotic => assert!(dyn_attrs[&564] > base_ty.default_attributes[&564], "Exotic boosts scan resolution (attr 564)"),
                AbyssalWeatherKind::Firestorm => assert!(dyn_attrs[&265] > base_ty.default_attributes[&265], "Firestorm boosts armor HP (attr 265)"),
                AbyssalWeatherKind::Gamma => assert!(dyn_attrs[&263] > base_ty.default_attributes[&263], "Gamma boosts shield capacity (attr 263)"),
            }
            restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
            assert!(ctx.db.with_snapshot(|s| s.items.get(&ship_id).unwrap().dynamic_attributes.is_empty()), "Pre-weather dynamic_attributes cleanly restored");
        }
    }
}

#[tokio::test]
async fn test_abyssal_spatial_hazard_clouds_and_composed_velocity() {
    let ctx = ServerContext::new();
    let abyssal_sys = 32_000_001_u32;
    let char_id = 90_000_102_i64;
    let ship_id = 100_000_102_i64;
    let booster_id = 100_000_103_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621,
                owner_id: char_id,
                location_id: abyssal_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Fitted Small Shield Booster (type 10838, group 40)
        snap.items.insert(
            booster_id,
            eve_db::InventoryItemRecord {
                item_id: booster_id,
                type_id: 10838,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 621,
        position: DVec3::new(0.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 621);
    let _ = sys_handle.spawn_ball_with_combat(ship_ball, cs).await;

    assert_eq!(TACHYON_CLOUD_TYPE_ID, 80_876, "Small Tachyon Cloud type 80876");
    assert_eq!(BIOLUMINESCENT_CLOUD_TYPE_ID, 47440, "Medium Bioluminescent Cloud type 47440");
    assert_eq!(FILAMENT_CLOUD_TYPE_ID, 47472, "Medium Filament Cloud type 47472");

    // Spawn 3 clouds: Tachyon at (15000, 0, 0), Bioluminescent at (-15000, 0, 0), Filament at (0, 15000, 0)
    for (cid, ctid, cpos) in [
        (10_001_i64, TACHYON_CLOUD_TYPE_ID, DVec3::new(15_000.0, 0.0, 0.0)),
        (10_002_i64, BIOLUMINESCENT_CLOUD_TYPE_ID, DVec3::new(-15_000.0, 0.0, 0.0)),
        (10_003_i64, FILAMENT_CLOUD_TYPE_ID, DVec3::new(0.0, 15_000.0, 0.0)),
    ] {
        let cball = Ball {
            id: cid, type_id: ctid, position: cpos, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: SPATIAL_CLOUD_RADIUS_M,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let _ = sys_handle.spawn_ball(cball).await;
    }

    let default_vel = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(250.0);
    let default_cap_rech = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&55).copied()).unwrap_or(120_000.0);

    // 1. Move ship into Tachyon cloud boundary (+300% signature radius, 4x velocity, 0.5x inertia)
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let effect = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyssal_sys, ship_id).await;
    assert_eq!(effect, Some("TachyonCloud"));
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_tach = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((ball.max_velocity - default_vel * 4.0).abs() < 1e-3, "Tachyon cloud quadruples max velocity");
    assert!((ball.inertia - 0.5 * 0.5).abs() < 1e-3, "Tachyon cloud halves inertia (-50%)");
    assert!((cs_tach.signature_radius - 125.0 * 4.0).abs() < 1e-3, "Tachyon cloud blooms signature radius +300% (x4)");
    assert!(ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.dynamic_attributes.is_empty()).unwrap_or(true)), "Dynamic attributes must not be polluted by spatial clouds");

    // 2. Move ship into Bioluminescent cloud boundary (+300% sig radius = 4x)
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(-15_000.0, 0.0, 0.0)).await;
    let effect = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyssal_sys, ship_id).await;
    assert_eq!(effect, Some("BioluminescentCloud"));
    let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs.signature_radius - 125.0 * 4.0).abs() < 1e-3, "Bioluminescent cloud blooms signature radius +300% (x4)");
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((ball.max_velocity - default_vel).abs() < 1e-3, "Velocity unchanged in Bioluminescent cloud");

    // 3. Move ship into Filament cloud boundary (0.6x shield boost & cycle duration, unchanged sig & cap recharge)
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(0.0, 15_000.0, 0.0)).await;
    let effect = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyssal_sys, ship_id).await;
    assert_eq!(effect, Some("FilamentCloud"));
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_fil = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((ball.max_velocity - default_vel).abs() < 1e-3, "Filament cloud does not change velocity");
    assert!((cs_fil.shield_boost_multiplier - 0.6).abs() < 1e-6, "Filament cloud applies -40% shield boost penalty (0.6x)");
    assert!((cs_fil.signature_radius - 125.0).abs() < 1e-3, "Filament cloud does not alter signature radius");
    assert!((cs_fil.capacitor_recharge_time_ms - default_cap_rech).abs() < 1.0, "Filament cloud does not alter cap recharge time");

    // Verify active Small Shield Booster I (10838) heals 0.6x inside FilamentCloud vs 1.0x outside
    let mut sess = SessionState::new(883_310);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id; ch.solar_system_id = abyssal_sys; ch.active_ship_id = ship_id; ch.station_id = None;
        snap.characters.insert(char_id, ch);
        snap.module_online_states.insert(booster_id, true);
    });
    sess.user_id = Some(1); sess.character_id = Some(char_id); sess.solar_system_id = Some(abyssal_sys); sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let mut cs_dmg = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    (cs_dmg.shield_hp, cs_dmg.max_shield_hp, cs_dmg.shield_recharge_time_ms, cs_dmg.capacitor, cs_dmg.max_capacitor) = (100.0, 2_000.0, 1.0e15, 5_000.0, 5_000.0);
    let _ = sys_handle.set_combat_state(ship_id, cs_dmg).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(booster_id), PyRep::String("shieldBoosting".into()), PyRep::None, PyRep::Integer(10)], &kw, None).await.expect("Activate shield booster in FilamentCloud");
    let _ = sys_handle.step_manual(1.0).await;
    let cs_1_in = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let healed_1_in_cloud = cs_1_in.shield_hp - 100.0;
    for _ in 0..9 { let _ = sys_handle.step_manual(1.0).await; }
    let cs_10_in = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let healed_10s_in_cloud = cs_10_in.shield_hp - 100.0;
    let cap_used_10s_in_cloud = 5_000.0 - cs_10_in.capacitor;
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(booster_id)], &kw, None).await;

    // 4. Move ship outside all clouds and verify full 1.0x shield boost & 4.0s cycle restored
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(0.0, 0.0, 0.0)).await;
    let effect = evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyssal_sys, ship_id).await;
    assert!(effect.is_none(), "No cloud effect outside boundary");
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((ball.max_velocity - default_vel).abs() < 1e-3, "Base velocity restored outside cloud boundary");
    let cs_out = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_out.signature_radius - 125.0).abs() < 1e-3, "Base signature radius restored outside cloud boundary");
    assert!((cs_out.shield_boost_multiplier - 1.0).abs() < 1e-6, "Shield boost multiplier restored to 1.0 outside FilamentCloud");
    for _ in 0..5 { let _ = sys_handle.step_manual(1.0).await; }
    let mut cs_out = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    (cs_out.shield_hp, cs_out.max_shield_hp, cs_out.shield_recharge_time_ms, cs_out.capacitor, cs_out.max_capacitor) = (100.0, 2_000.0, 1.0e15, 5_000.0, 5_000.0);
    let _ = sys_handle.set_combat_state(ship_id, cs_out).await;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(booster_id), PyRep::String("shieldBoosting".into()), PyRep::None, PyRep::Integer(10)], &kw, None).await.expect("Activate shield booster outside cloud");
    let _ = sys_handle.step_manual(1.0).await;
    let cs_1_out = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let healed_1_outside = cs_1_out.shield_hp - 100.0;
    for _ in 0..9 { let _ = sys_handle.step_manual(1.0).await; }
    let cs_10_out = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let healed_10s_outside = cs_10_out.shield_hp - 100.0;
    let cap_used_10s_outside = 5_000.0 - cs_10_out.capacitor;
    assert!(healed_1_in_cloud > 0.0 && (healed_1_in_cloud - healed_1_outside * 0.6).abs() < 1e-3, "Single-cycle shield boost inside FilamentCloud ({healed_1_in_cloud}) must be 0.6x outside ({healed_1_outside})");
    assert!((healed_10s_in_cloud - healed_10s_outside).abs() < 1e-3, "Sustained 10s HPS inside FilamentCloud ({healed_10s_in_cloud}) must match outside ({healed_10s_outside}) due to -40% cycle duration");
    assert!(cap_used_10s_in_cloud > cap_used_10s_outside * 1.5, "Faster cycle inside FilamentCloud ({cap_used_10s_in_cloud} GJ) must consume ~1.67x capacitor vs outside ({cap_used_10s_outside} GJ)");
    assert!(ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.dynamic_attributes.is_empty()).unwrap_or(true)), "Dynamic attributes remain empty outside clouds");

    // 5. Test real-time tick in Ballpark via step_abyssal_hazards_and_pylons_in_ballpark
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
    let (pre_vel, pre_in) = { let b = bp.balls.get(&ship_id).unwrap(); (b.max_velocity, b.inertia) };
    step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyssal_sys);
    assert!((bp.balls.get(&ship_id).unwrap().max_velocity - pre_vel * 4.0).abs() < 1e-3, "Tick applies Tachyon cloud 4x max velocity");
    assert!((bp.balls.get(&ship_id).unwrap().inertia - pre_in * 0.5).abs() < 1e-3, "Tick applies Tachyon cloud 0.5x inertia");
    assert!(ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.dynamic_attributes.is_empty()).unwrap_or(true)), "Tick does not pollute dynamic_attributes");
}

#[tokio::test]
async fn test_abyssal_automated_pylons_and_field_disable_on_destruction() {
    let ctx = ServerContext::new();
    let abyssal_sys = 32_000_001_u32;
    let char_id = 90_000_103_i64;
    let ship_id = 100_000_104_i64;
    let small_suppressor_id = 100_000_105_i64;
    let tracking_pylon_id = 100_000_106_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (Frigate)
                owner_id: char_id,
                location_id: abyssal_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.shield_hp = 5000.0;
    cs.max_shield_hp = 5000.0;
    let _ = sys_handle.spawn_ball_with_combat(ship_ball, cs).await;

    let drone_id = 100_000_107_i64;
    let drone_ball = Ball {
        id: drone_id,
        type_id: 47847, // Sparkneedle Tessella (Rogue Drone frigate)
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 25.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut drone_cs = ctx.build_type_combat_state(47847);
    drone_cs.shield_hp = 500.0;
    drone_cs.max_shield_hp = 500.0;
    let _ = sys_handle.spawn_ball_with_combat(drone_ball, drone_cs).await;

    assert_eq!(AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 47437, "Deviant Automata Suppressor Short type 47437");
    assert_eq!(MULTIBODY_TRACKING_PYLON_TYPE_ID, 47469, "Multibody Tracking Pylon Short type 47469");

    // Spawn Small Automata Suppressor (15 km range) at (0, 0, 0)
    let pylon_ball = Ball {
        id: small_suppressor_id,
        type_id: AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0,
        radius: 15_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut pylon_cs = ctx.build_type_combat_state(AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID);
    pylon_cs.structure_hp = 5000.0;
    pylon_cs.max_structure_hp = 5000.0;
    let _ = sys_handle.spawn_ball_with_combat(pylon_ball, pylon_cs).await;

    // Spawn Multibody Tracking Pylon (20 km range) at (0, 0, 0)
    let track_ball = Ball {
        id: tracking_pylon_id,
        type_id: MULTIBODY_TRACKING_PYLON_TYPE_ID,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0,
        radius: 20_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut track_cs = ctx.build_type_combat_state(MULTIBODY_TRACKING_PYLON_TYPE_ID);
    track_cs.structure_hp = 5000.0;
    track_cs.max_structure_hp = 5000.0;
    let _ = sys_handle.spawn_ball_with_combat(track_ball, track_cs).await;

    // 1. Pulse pylons: suppressor damages rogue drone frigate (within 15km), player ship takes NO suppressor damage, tracking pylon buffs tracking
    let drone_hp_before = sys_handle.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
    let ship_hp_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let (dmg_count, track_count) = pulse_abyssal_automated_pylons(&ctx, abyssal_sys).await;
    assert!(dmg_count > 0, "Automata suppressor damaged rogue drone");
    assert!(track_count > 0, "Tracking pylon buffed ship tracking");
    let drone_hp_after = sys_handle.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
    assert!(drone_hp_after < drone_hp_before, "Rogue drone sustained explosive damage from suppressor");
    let ship_cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(ship_cs_after.shield_hp, ship_hp_before, "Player ship takes NO damage from suppressor");
    assert!((ship_cs_after.tracking_speed_multiplier - 1.8).abs() < 1e-6, "Multibody Tracking Pylon grants +80% tracking speed (1.8x)");

    // Dynamic attributes must NOT be polluted with tracking speed
    let tracking = ctx.db.with_snapshot(|s| s.items.get(&ship_id).and_then(|i| i.dynamic_attributes.get(&160).copied()).unwrap_or(0.0));
    assert_eq!(tracking, 0.0, "Dynamic attributes not polluted with tracking speed");

    // 2. Destroy both the suppressor pylon and tracking pylon; verify aura reverts to 1.0x
    let cs_before_kill = sys_handle.get_combat_state(small_suppressor_id).await.unwrap().unwrap();
    let hp_before_kill = cs_before_kill.shield_hp + cs_before_kill.armor_hp + cs_before_kill.structure_hp;
    let partial_dmg = DamageVector { em: 2000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let _ = sys_handle.apply_damage_with_attacker(small_suppressor_id, partial_dmg, Some(char_id)).await;
    let cs_mid = sys_handle.get_combat_state(small_suppressor_id).await.unwrap().unwrap();
    let hp_mid = cs_mid.shield_hp + cs_mid.armor_hp + cs_mid.structure_hp;
    assert!(hp_mid < hp_before_kill, "Pylon HP decreased after damage: {hp_mid} < {hp_before_kill}");
    let kill_dmg = DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 };
    let _ = sys_handle.apply_damage_with_attacker(small_suppressor_id, kill_dmg, Some(char_id)).await;
    let _ = sys_handle.apply_damage_with_attacker(tracking_pylon_id, kill_dmg, Some(char_id)).await;
    assert!(sys_handle.get_ball_snapshot(small_suppressor_id).await.unwrap().is_none(), "Destroyed suppressor removed");
    assert!(sys_handle.get_ball_snapshot(tracking_pylon_id).await.unwrap().is_none(), "Destroyed tracking pylon removed");

    // Pulse again: destroyed pylons do not damage or buff tracking (tracking_speed_multiplier reverts to 1.0)
    let hp_before2 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let (dmg_count2, track_count2) = pulse_abyssal_automated_pylons(&ctx, abyssal_sys).await;
    assert_eq!((dmg_count2, track_count2), (0, 0), "Destroyed pylons do not pulse");
    let cs_after2 = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(hp_before2, cs_after2.shield_hp, "No damage taken from destroyed suppressor");
    assert!((cs_after2.tracking_speed_multiplier - 1.0).abs() < 1e-6, "Tracking speed multiplier reverts to 1.0 when pylon is destroyed");
}

#[tokio::test]
async fn test_abyssal_trace_scanning_and_fleet_entry() {
    // parity: feat:navigation.abyssal-trace-collapse, feat:navigation.filament-abyssal
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let origin_sys = 30_000_002_u32; // Lashesih (0.8 high-sec)
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    let ship1 = 100_000_001_i64;
    let ship2 = 100_000_002_i64;
    let fil_item = 100_000_109_i64;
    let fil_type = 47761_u32; // Calm Exotic Filament

    let mut sess1 = SessionState::new(883_301);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char1)], &kw, None)
        .await.expect("SelectCharacterID char1");

    let mut sess2 = SessionState::new(883_302);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char2)], &kw, None)
        .await.expect("SelectCharacterID char2");

    let fleet_id = 99_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char1, ship1), (char2, ship2)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = origin_sys; ch.active_ship_id = sid; ch.station_id = None; ch.structure_id = None;
            }
            snap.items.insert(sid, eve_db::InventoryItemRecord {
                item_id: sid, type_id: 587, owner_id: cid, location_id: origin_sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.items.insert(fil_item, eve_db::InventoryItemRecord {
            item_id: fil_item, type_id: fil_type, owner_id: char1, location_id: ship1,
            flag: InventoryFlag::Cargo, quantity: 3, singleton: false, ..Default::default()
        });
        snap.fleets.insert(fleet_id, eve_db::FleetRecord { fleet_id, boss_id: char1, members: vec![char1, char2], ..Default::default() });
    });

    sess1.solar_system_id = Some(origin_sys); sess1.ship_id = Some(ship1); sess1.station_id = None; sess1.structure_id = None;
    sess2.solar_system_id = Some(origin_sys); sess2.ship_id = Some(ship2); sess2.station_id = None; sess2.structure_id = None;

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    for (sid, x) in [(ship1, 100.0), (ship2, 2_000_000.0)] {
        let b = Ball {
            id: sid, type_id: 587, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let _ = origin_handle.spawn_ball(b).await;
    }

    // Deploy an un-entered trace for char2 first to verify RF-03 (survives other pocket exit, despawns at 20m lifetime)
    let fil_unentered = 100_000_119_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fil_unentered, eve_db::InventoryItemRecord {
            item_id: fil_unentered, type_id: fil_type, owner_id: char2, location_id: ship2,
            flag: InventoryFlag::Cargo, quantity: 3, singleton: false, ..Default::default()
        });
    });
    let unent_rep = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_unentered)], &kw, None).await.expect("Deploy unentered trace");
    let unent_trace_id = unent_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();

    // 1. Char 1 deploys filament entrance
    let deploy_rep = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_item)], &kw, None).await.expect("AbyssalEntranceDeployment");
    let trace_id = deploy_rep.dict_get("traceBallID").or_else(|| deploy_rep.dict_get("traceID")).and_then(|v| v.as_i64()).unwrap();
    let trace_ball = origin_handle.get_ball_snapshot(trace_id).await.unwrap().expect("Trace spawned in space");
    assert_eq!(trace_ball.1.type_id, 47_458, "Abyssal Trace must be SDE type 47458");

    // 2. Combat probe scan locates the Abyssal Trace
    for i in 0..4 {
        let pid = 999_100_i64 + i;
        ctx.active_scanner_probes.write().unwrap().insert(pid, eve_server::router::ActiveScannerProbe {
            probe_id: pid, probe_type_id: 30028, owner_char_id: char2, ship_id: ship2, launcher_module_id: 0,
            solar_system_id: origin_sys, position: [0.0, 0.0, 0.0], destination: [0.0, 0.0, 0.0],
            scan_range_m: 4.0 * 149_597_870_700.0, range_step: 4, base_sensor_strength: 50.0,
            max_flight_time_s: 3600.0, launched_at_unix: ctx.now_unix(), state: 1,
        });
    }
    let scan_rep = ctx.dispatch_rpc(&mut sess2, "scanMgr", None, "RequestScans", &[PyRep::Real(4.0 * 149_597_870_700.0), PyRep::Real(40.0)], &kw, None).await.expect("RequestScans finds trace");
    let PyRep::List(scan_rows) = scan_rep else { panic!("Expected PyRep::List of scan results from combat probes"); };
    assert!(scan_rows.iter().any(|r| r.dict_get("targetID").and_then(|v| v.as_i64()) == Some(trace_id) && r.dict_get("targetClass").and_then(|v| v.as_str()) == Some("AbyssalTrace")));

    // 3. Char 1 enters pocket at t=0, clears Room 1 NPCs; Char 2 enters 300s later and shares original 20m collapse deadline
    let enter1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw, None).await.expect("Char 1 enters Abyssal pocket");
    let abyssal_sys = enter1.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let pocket_id = enter1.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(sess1.solar_system_id, Some(abyssal_sys));

    let abyss_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let lethal = DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 };
    let r1_npcs = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.get(&char1).unwrap().rooms[0].npc_ball_ids.clone()
    });
    for npc_id in r1_npcs {
        let _ = abyss_handle.apply_damage_with_attacker(npc_id, lethal, Some(char1)).await;
    }

    ctx.advance_secs(300);
    let _ = origin_handle.set_ball_position(ship2, DVec3::new(600.0, 0.0, 0.0)).await;
    let enter2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw, None).await.expect("Char 2 joins fleet Abyssal pocket");
    assert_eq!(enter2.dict_get("pocketID").and_then(|v| v.as_i64()), Some(pocket_id));
    let rem_timer = enter2.dict_get("timerSeconds").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!((895..=900).contains(&rem_timer), "Late joiner shares original 20m pocket collapse timer (got {rem_timer})");
    assert_eq!(sess2.solar_system_id, Some(abyssal_sys));

    // 4. R-ABY-FLEET-01: Both fleet Frigates clear Rooms 1, 2, and 3 and both transit conduits and exit
    for room in 1..=3_i64 {
        if room > 1 {
            let room_npcs = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
                reg.abyssal_pockets.get(&char1).unwrap().rooms[(room - 1) as usize].npc_ball_ids.clone()
            });
            for npc_id in room_npcs {
                let _ = abyss_handle.apply_damage_with_attacker(npc_id, lethal, Some(char1)).await;
            }
        }
        let gate_id = 1_895_000_000 + pocket_id * 10 + room;
        let gate_pos = abyss_handle.get_ball_snapshot(gate_id).await.unwrap().unwrap().1.position;
        let _ = abyss_handle.set_ball_position(ship1, gate_pos + DVec3::new(100.0, 0.0, 0.0)).await;
        let _ = abyss_handle.set_ball_position(ship2, gate_pos + DVec3::new(-100.0, 0.0, 0.0)).await;
        if room < 3 {
            let g1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gate_id)], &kw, None).await.expect("Char 1 activates transfer gate");
            let g2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gate_id)], &kw, None).await.expect("Char 2 activates transfer gate without being stranded");
            assert_eq!(g1.dict_get("room").and_then(|v| v.as_i64()), Some(room + 1));
            assert_eq!(g2.dict_get("room").and_then(|v| v.as_i64()), Some(room + 1));
        } else {
            let ex1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(gate_id)], &kw, None).await.expect("Char 1 exits room 3");
            let ex2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(gate_id)], &kw, None).await.expect("Char 2 exits room 3");
            assert_eq!(ex1.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(origin_sys as i64));
            assert_eq!(ex2.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(origin_sys as i64));
        }
    }
    assert!(origin_handle.get_ball_snapshot(trace_id).await.unwrap().is_none(), "Trace despawned after last fleet member exits");
    ctx.advance_secs(905); // t=1205s (> 20m, < 30m): un-entered trace stays alive
    assert!(origin_handle.get_ball_snapshot(unent_trace_id).await.unwrap().is_some(), "Un-entered trace remains active past 20m before 30m lifetime");
    ctx.advance_secs(600); // t=1805s (> 30m): un-entered trace expires via cluster tick
    assert!(origin_handle.get_ball_snapshot(unent_trace_id).await.unwrap().is_none(), "Un-entered trace despawned automatically at 30m expiry");
}

#[tokio::test]
async fn test_abyssal_timer_collapse_and_pod_death() {
    // parity: feat:abyssal.timer-collapse-death, feat:navigation.filament-abyssal
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let origin_sys = 30_000_002_u32; // Lashesih (0.8 high-sec)
    let amarr_station = 60_008_494_i64; // Amarr VIII (system 30002187)
    let (char_id, char2_id) = (90_000_001_i64, 90_000_002_i64);
    let (ship_id, ship2_id, fil_id) = (100_000_001_i64, 100_000_002_i64, 100_000_121_i64);

    let mut sess1 = SessionState::new(883_321);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID 1");
    let mut sess2 = SessionState::new(883_322);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char2_id)], &kw, None).await.expect("SelectCharacterID 2");

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char_id, ship_id), (char2_id, ship2_id)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = origin_sys; ch.active_ship_id = sid; ch.station_id = None; ch.structure_id = None;
                ch.balance = eve_db::IskCents::from_isk(10_000_000);
                if cid == char_id { ch.home_station_id = amarr_station; }
            }
            snap.items.insert(sid, eve_db::InventoryItemRecord {
                item_id: sid, type_id: 587, owner_id: cid, location_id: origin_sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.items.insert(fil_id, eve_db::InventoryItemRecord {
            item_id: fil_id, type_id: 47761, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 3, singleton: false, ..Default::default()
        });
        snap.fleets.insert(99_102, eve_db::FleetRecord { fleet_id: 99_102, boss_id: char_id, members: vec![char_id, char2_id], ..Default::default() });
    });
    sess1.solar_system_id = Some(origin_sys); sess1.ship_id = Some(ship_id); sess1.station_id = None; sess1.structure_id = None;
    sess2.solar_system_id = Some(origin_sys); sess2.ship_id = Some(ship2_id); sess2.station_id = None; sess2.structure_id = None;

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let prox_anchor_pos = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == origin_sys)
        .map(|s| s.position)
        .or_else(|| ctx.sde.stargates().values().find(|g| g.from_solar_system_id == origin_sys).map(|g| g.position))
        .unwrap_or(DVec3::ZERO);
    // RF-05: Within 1,000 km of station/stargate -> AbyssalEntranceDeployment rejected!
    let _ = origin_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: prox_anchor_pos + DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let prox_err = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)], &kw, None)
        .await.expect_err("Abyssal filament deployment within 1,000 km of station must be rejected");
    assert_eq!(prox_err.code, "ProximityRestriction");

    // Move > 1,000 km away from stations/stargates and deploy/enter via real RPCs
    for sid in [ship_id, ship2_id] {
        let _ = origin_handle.spawn_ball_with_combat(Ball {
            id: sid, type_id: 587, position: DVec3::new(100_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, ctx.build_ship_combat_state(char_id, sid, 587)).await;
    }
    let now = ctx.now_unix();
    let initial_balance = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(ship_id, eve_server::services::combat_crimewatch::InsuranceContractState {
            ship_id, ship_type_id: 587, owner_id: char_id, level_name: "Basic".into(), level_fraction: 1.0,
            cost_isk: 0.0, payout_isk: 10_000_000.0, start_unix: now, end_unix: now + 100_000,
        });
    });

    let dep = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)], &kw, None).await.expect("Deploy");
    let trace_id = dep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent1 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw, None).await.expect("Enter 1");
    let abyssal_sys = ent1.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    ctx.advance_secs(300);
    ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw, None).await.expect("Late joiner Enter 2");

    // Advance +800s (total 1100s < 1200s): pocket still active
    ctx.advance_secs(800);
    let (mid_sys, mid_ship) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id).unwrap();
        (ch.solar_system_id, ch.active_ship_id)
    });
    assert_eq!((mid_sys, mid_ship), (abyssal_sys, ship_id), "Pocket intact before 1200s expiry");

    // Advance +105s (total 1205s >= 1200s): cluster tick via ctx.advance_secs collapses pocket & pods BOTH pilots!
    ctx.advance_secs(105);
    let (cur_sys, cur_station, cur_ship, cur_sys2, cur_ship2) = ctx.db.with_snapshot(|s| {
        let ch1 = s.characters.get(&char_id).unwrap();
        let ch2 = s.characters.get(&char2_id).unwrap();
        (ch1.solar_system_id, ch1.station_id, ch1.active_ship_id, ch2.solar_system_id, ch2.active_ship_id)
    });
    assert_eq!((cur_sys, cur_station), (30_002_187, Some(amarr_station)), "Pilot 1 clone-respawned at actual Amarr home station (RF-02)");
    assert_ne!(cur_ship, ship_id, "Pilot 1 ship destroyed and replaced with starter corvette");
    assert_ne!(cur_sys2, abyssal_sys, "Late-joining Pilot 2 podded at shared pocket collapse deadline");
    assert_ne!(cur_ship2, ship2_id, "Late-joining Pilot 2 ship destroyed at shared collapse deadline");

    let final_balance = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(final_balance, initial_balance, "No insurance payout on Abyssal collapse death");
    assert!(!eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)));
    let kms = ctx.db.with_snapshot(|s| s.killmails.values().filter(|k| k.victim_character_id == char_id).cloned().collect::<Vec<_>>());
    assert_eq!(kms.len(), 2, "Expected 2 killmails (ship and capsule)");
    assert!(kms.iter().all(|k| k.attacker_character_id == 0 && k.attacker_corp_id == 1_000_298), "Death logged to Abyssal Environment");
    assert!(kms.iter().any(|k| k.ship_type_id == 670), "Capsule destruction logged");
}
