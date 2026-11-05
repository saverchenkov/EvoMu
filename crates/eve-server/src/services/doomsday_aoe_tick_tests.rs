use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use eve_db::IskCents;
use eve_destiny::{Ball, Ballpark, CombatState, DVec3, MovementMode};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn test_read_and_mutate_superweapon_runtime_and_subcapital_check() {
    let (mut ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    assert!(read_superweapon_runtime(&ctx.db).is_none());

    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id,
            ship_id,
            suppressed_until_unix: 1000,
            orig_warp_scramble_strength: 0,
        });
    });
    let loaded = read_superweapon_runtime(&ctx.db).expect("blob should exist");
    assert_eq!(loaded.warp_suppressions.len(), 1);

    // Clearing all entries removes the blob key
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.warp_suppressions.clear();
    });
    assert!(read_superweapon_runtime(&ctx.db).is_none());

    // Subcapital check for GTFO
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .insert(600, make_fighter_type(600, 25, 6, 10_000.0, vec![])); // Cruiser -> subcap
    sde_mut
        .types
        .insert(670, make_fighter_type(670, 29, 6, 100.0, vec![])); // Capsule -> excluded
    sde_mut
        .types
        .insert(11567, make_fighter_type(11567, 30, 6, 1_000_000.0, vec![])); // Titan -> excluded
    assert!(is_subcapital_ship_for_gtfo(&ctx.sde, 600));
    assert!(!is_subcapital_ship_for_gtfo(&ctx.sde, 670));
    assert!(!is_subcapital_ship_for_gtfo(&ctx.sde, 11567));
    let _ = char_id;
}

#[test]
fn test_step_bosonic_doomsday_in_ballpark_immobility_and_warp_suppression() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let target_ship_id = 700_000_001_i64;
    let mut bp = Ballpark::new(sys_id);
    bp.balls.insert(
        ship_id,
        Ball {
            id: ship_id,
            type_id: 11_567,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 80.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 5_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.combat_states.insert(ship_id, CombatState::new());
    bp.balls.insert(
        target_ship_id,
        Ball {
            id: target_ship_id,
            type_id: 600,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.combat_states.insert(target_ship_id, CombatState::new());

    let now = 1_700_000_000_u64;
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [5_000.0, 0.0, 0.0],
                sphere_radius_m: 10_000.0,
            },
            started_at_unix: now,
            warmup_secs: 15,
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: now + 30,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: false,
        });
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id,
            ship_id: target_ship_id,
            suppressed_until_unix: now + 60,
            orig_warp_scramble_strength: 0,
        });
    });

    let mut scrams = HashMap::new();
    let mut destroyed = Vec::new();
    // During immobility window: max_velocity = 0, scram = 100
    step_bosonic_doomsday_in_ballpark(
        &mut bp,
        &mut scrams,
        Some(&ctx.sde),
        Some(&ctx.db),
        sys_id,
        now + 10,
        &mut destroyed,
    );
    assert_eq!(bp.balls.get(&ship_id).unwrap().max_velocity, 0.0);
    assert_eq!(scrams.get(&ship_id), Some(&100));
    assert_eq!(
        scrams.get(&target_ship_id),
        Some(&GTFO_WARP_SUPPRESSION_STRENGTH)
    );

    // After immobility & warp suppression expire: originals restored
    step_bosonic_doomsday_in_ballpark(
        &mut bp,
        &mut scrams,
        Some(&ctx.sde),
        Some(&ctx.db),
        sys_id,
        now + 70,
        &mut destroyed,
    );
    assert_eq!(bp.balls.get(&ship_id).unwrap().max_velocity, 80.0);
    assert_eq!(scrams.get(&ship_id), Some(&0));
    assert_eq!(scrams.get(&target_ship_id), Some(&0));
}

#[tokio::test]
async fn test_step_bosonic_doomsday_system_cone_gtfo_and_reaper() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let victim_char_id = 90_000_050_i64;
    let victim_ship_id = 700_000_050_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = victim_char_id;
        vic.user_id = 50;
        vic.name = "TargetPilot".into();
        vic.corporation_id = 1_000_009;
        vic.alliance_id = None;
        vic.station_id = None;
        vic.structure_id = None;
        vic.solar_system_id = sys_id;
        vic.active_ship_id = victim_ship_id;
        vic.balance = IskCents(0);
        vic.security_status = 0.0;
        snap.characters.insert(victim_char_id, vic);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 11_567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 80.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 5_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: victim_ship_id,
                type_id: 600,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
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
                ..CombatState::default()
            },
        )
        .await;

    let now = ctx.now_unix_secs();
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::Cone {
                origin_pos: [0.0, 0.0, 0.0],
                dir_vec: [1.0, 0.0, 0.0],
                cone_len_m: 30_000.0,
                tick_dmg: [1_000.0, 1_000.0, 1_000.0, 1_000.0],
                sig_radius_m: 100.0,
            },
            started_at_unix: now.saturating_sub(20),
            warmup_secs: 10,
            total_ticks: 2,
            ticks_applied: 0,
            immobilize_until_unix: now + 30,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::ReaperSweep {
                start_pt: [10_000.0, 0.0, -5_000.0],
                end_pt: [10_000.0, 0.0, 5_000.0],
                beam_radius_m: 5_000.0,
                sweep_dmg: [500.0, 500.0, 500.0, 500.0],
                sig_radius_m: 100.0,
                already_hit_balls: Vec::new(),
            },
            started_at_unix: now.saturating_sub(20),
            warmup_secs: 10,
            total_ticks: 2,
            ticks_applied: 0,
            immobilize_until_unix: now + 30,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [10_000.0, 0.0, 0.0],
                sphere_radius_m: 15_000.0,
            },
            started_at_unix: now.saturating_sub(20),
            warmup_secs: 15,
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: now + 30,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
    });

    step_bosonic_doomsday_system(&ctx, sys_id).await;

    // Victim should have been teleported and warp-suppressed by GTFO
    let rt = read_superweapon_runtime(&ctx.db).expect("warp suppression should persist");
    assert_eq!(rt.warp_suppressions.len(), 1);
    assert_eq!(rt.warp_suppressions[0].ship_id, victim_ship_id);
}
