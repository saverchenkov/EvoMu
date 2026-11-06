//! Extra branch-coverage and mutation-killing unit tests for `doomsday_aoe_tick.rs`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::services::doomsday_aoe_tick::{
    apply_doomsday_capsuleer_crimewatch, is_subcapital_ship_for_gtfo, mutate_superweapon_runtime,
    read_superweapon_runtime, step_bosonic_doomsday_in_ballpark, step_bosonic_doomsday_system,
    PersistedSuperweaponCycle, PersistedWarpSuppression, SuperweaponCycleKind,
    GTFO_WARP_SUPPRESSION_STRENGTH,
};
use eve_db::{IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, Ballpark, CombatState, DVec3, MovementMode};
use eve_sde::SolarSystem;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn gtfo_subcapital_eligibility_all_excluded_groups_and_attributes() {
    let (mut ctx, _sess, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        for (idx, gid) in [29_u32, 30, 485, 513, 547, 659, 883, 902, 1538, 4594]
            .into_iter()
            .enumerate()
        {
            let tid = 980_000 + idx as u32;
            sde_mut
                .types
                .insert(tid, make_fighter_type(tid, gid, 6, 10_000.0, vec![]));
        }
        sde_mut
            .types
            .insert(980_100, make_fighter_type(980_100, 25, 18, 100.0, vec![]));
        sde_mut.types.insert(
            980_101,
            make_fighter_type(980_101, 25, 6, 10_000.0, vec![(1980, 1.0)]),
        );
        sde_mut.types.insert(
            980_102,
            make_fighter_type(980_102, 25, 6, 10_000.0, vec![(872, 1.0)]),
        );
        sde_mut.types.insert(
            980_103,
            make_fighter_type(980_103, 25, 6, 10_000.0, vec![(1980, 0.0), (872, 0.0)]),
        );
    }

    for idx in 0..10 {
        let tid = 980_000 + idx as u32;
        assert!(!is_subcapital_ship_for_gtfo(&ctx.sde, tid));
    }
    assert!(!is_subcapital_ship_for_gtfo(&ctx.sde, 980_100));
    assert!(!is_subcapital_ship_for_gtfo(&ctx.sde, 980_101));
    assert!(!is_subcapital_ship_for_gtfo(&ctx.sde, 980_102));
    assert!(is_subcapital_ship_for_gtfo(&ctx.sde, 980_103));
}

#[test]
fn apply_doomsday_capsuleer_crimewatch_nullsec_lowsec_pod_and_legal_targets() {
    let (mut ctx, _sess, attacker_id, attacker_ship_id, _sys_id) = make_fighter_ctx_and_session();
    let null_sys = 30_004_759_u32;
    let low_sys = 30_002_537_u32;
    let victim_id = 90_800_001_i64;
    let victim_ship = 780_000_001_i64;
    let now = 1_780_000_000_u64;

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let base_sys = sde_mut.solar_systems.values().next().cloned().unwrap();
        sde_mut.solar_systems.insert(
            null_sys,
            SolarSystem {
                solar_system_id: null_sys,
                security_status: -0.5,
                ..base_sys.clone()
            },
        );
        sde_mut.solar_systems.insert(
            low_sys,
            SolarSystem {
                solar_system_id: low_sys,
                security_status: 0.3,
                ..base_sys
            },
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut v = snap.characters.get(&attacker_id).cloned().unwrap();
        v.character_id = victim_id;
        v.user_id = 801;
        v.name = "Victim".into();
        v.corporation_id = 1_000_125;
        v.active_ship_id = victim_ship;
        v.solar_system_id = null_sys;
        v.balance = IskCents::ZERO;
        snap.characters.insert(victim_id, v);
    });

    // 1. Unknown ball id -> early return, no pvp timer
    apply_doomsday_capsuleer_crimewatch(
        &ctx,
        attacker_id,
        attacker_ship_id,
        null_sys,
        999_999_999,
        600,
        now,
    );
    assert_eq!(
        ctx.crimewatch
            .write()
            .unwrap()
            .get_or_create(attacker_id)
            .pvp_timer_until_unix,
        0
    );

    // 2. Nullsec aggression -> sets pvp_timer_until_unix = now + 900 on both, no suspect/criminal flag
    apply_doomsday_capsuleer_crimewatch(
        &ctx,
        attacker_id,
        attacker_ship_id,
        null_sys,
        victim_ship,
        600,
        now,
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        assert_eq!(
            cw.get_or_create(attacker_id).pvp_timer_until_unix,
            now + 900
        );
        assert_eq!(cw.get_or_create(victim_id).pvp_timer_until_unix, now + 900);
        assert!(!cw.get_or_create(attacker_id).is_suspect(now));
        assert!(!cw.get_or_create(attacker_id).is_criminal(now));
    }

    // 3. Lowsec aggression against suspect victim -> legal target, no suspect/criminal flag on attacker
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(victim_id)
        .suspect_flag_until_unix = now + 900;
    apply_doomsday_capsuleer_crimewatch(
        &ctx,
        attacker_id,
        attacker_ship_id,
        low_sys,
        victim_ship,
        600,
        now,
    );
    assert!(!ctx
        .crimewatch
        .write()
        .unwrap()
        .get_or_create(attacker_id)
        .is_suspect(now));

    // 4. Lowsec aggression against non-suspect capsule (type 670) -> criminal flag on attacker
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(victim_id)
        .suspect_flag_until_unix = 0;
    apply_doomsday_capsuleer_crimewatch(
        &ctx,
        attacker_id,
        attacker_ship_id,
        low_sys,
        victim_ship,
        670,
        now,
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(attacker_id);
        assert_eq!(st.criminal_flag_until_unix, now + 900);
        assert_eq!(st.suspect_flag_until_unix, 0);
    }

    // 5. Limited engagement makes target legal
    with_mut_combat_runtime(&ctx, |rt| {
        rt.add_limited_engagement(victim_id, attacker_id, now + 300);
    });
    apply_doomsday_capsuleer_crimewatch(
        &ctx,
        attacker_id,
        attacker_ship_id,
        low_sys,
        victim_ship,
        600,
        now + 10,
    );
}

#[test]
fn step_bosonic_doomsday_in_ballpark_none_db_cross_system_and_retention() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let other_sys = sys_id + 1;
    let mut bp = Ballpark::new(sys_id);
    let mut scrams = HashMap::new();
    let mut destroyed = Vec::new();

    // db_opt = None -> no-op
    step_bosonic_doomsday_in_ballpark(
        &mut bp,
        &mut scrams,
        Some(&ctx.sde),
        None,
        sys_id,
        1_000,
        &mut destroyed,
    );
    // No blob -> no-op
    step_bosonic_doomsday_in_ballpark(
        &mut bp,
        &mut scrams,
        Some(&ctx.sde),
        Some(&ctx.db),
        sys_id,
        1_000,
        &mut destroyed,
    );

    bp.balls.insert(
        ship_id,
        Ball {
            id: ship_id,
            type_id: 11_567,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 75.0,
            inertia: 1.0,
            mass: 1.0e9,
            radius: 5_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    let mut cs = CombatState::new();
    cs.is_tethered = true;
    bp.combat_states.insert(ship_id, cs);

    let now = 2_000_u64;
    mutate_superweapon_runtime(&ctx.db, |rt| {
        // Cycle in other_sys must not be modified when stepping sys_id
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id: other_sys,
            char_id,
            ship_id: ship_id + 99,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [0.0; 3],
                sphere_radius_m: 1_000.0,
            },
            started_at_unix: now,
            warmup_secs: 5,
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: now + 10,
            orig_max_velocity: 75.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: false,
        });
        // Cycle in sys_id with ticks_applied == total_ticks (1): once mobility is restored at exact boundary now + 10, it gets pruned!
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [0.0; 3],
                sphere_radius_m: 1_000.0,
            },
            started_at_unix: now,
            warmup_secs: 5,
            total_ticks: 1,
            ticks_applied: 1,
            immobilize_until_unix: now + 10,
            orig_max_velocity: 75.0,
            orig_warp_scramble_strength: 2,
            orig_disallow_assistance: true,
            mobility_restored: false,
        });
        // Warp suppression in other_sys retained; warp suppression in sys_id with orig_scram = 30 (> 25) enforces 30
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id: other_sys,
            ship_id: 888,
            suppressed_until_unix: now + 1,
            orig_warp_scramble_strength: 0,
        });
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id,
            ship_id,
            suppressed_until_unix: now + 10,
            orig_warp_scramble_strength: 30,
        });
    });

    // At now + 9 (before immobilize_until_unix): immobilized, tether broken, scram 100
    step_bosonic_doomsday_in_ballpark(
        &mut bp,
        &mut scrams,
        Some(&ctx.sde),
        Some(&ctx.db),
        sys_id,
        now + 9,
        &mut destroyed,
    );
    assert_eq!(bp.balls[&ship_id].max_velocity, 0.0);
    assert!(!bp.combat_states[&ship_id].is_tethered);
    assert!(bp.combat_states[&ship_id].disallow_assistance);

    // At exact boundary now + 10: mobility restored & cycle pruned (since ticks_applied == total_ticks),
    // warp suppression in sys_id expires and restores orig_warp_scramble_strength = 30
    step_bosonic_doomsday_in_ballpark(
        &mut bp,
        &mut scrams,
        Some(&ctx.sde),
        Some(&ctx.db),
        sys_id,
        now + 10,
        &mut destroyed,
    );
    assert_eq!(bp.balls[&ship_id].max_velocity, 75.0);
    assert_eq!(scrams.get(&ship_id), Some(&30));
    let rt = read_superweapon_runtime(&ctx.db).unwrap();
    assert_eq!(rt.active_cycles.len(), 1);
    assert_eq!(rt.active_cycles[0].sys_id, other_sys);
    assert_eq!(rt.warp_suppressions.len(), 1);
    assert_eq!(rt.warp_suppressions[0].sys_id, other_sys);
    let _ = GTFO_WARP_SUPPRESSION_STRENGTH;
}

#[tokio::test]
async fn step_bosonic_doomsday_system_warmup_cone_filters_reaper_dedup_and_gtfo_guards() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let struct_id = 1_000_000_000_999_i64;
    let out_of_cone_ball = 790_001_i64;
    let reaper_target_ball = 790_002_i64;
    let gtfo_tethered_ball = 790_003_i64;
    let gtfo_valid_ball = 790_004_i64;
    let gtfo_valid_char = 90_850_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Cone Target Citadel".into(),
                state: "ShieldVulnerable".into(),
                position: [5_000.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                ..Default::default()
            },
        );
        let mut vc = snap.characters.get(&char_id).cloned().unwrap();
        vc.character_id = gtfo_valid_char;
        vc.user_id = 854;
        vc.active_ship_id = gtfo_valid_ball;
        vc.solar_system_id = sys_id;
        vc.station_id = None;
        vc.structure_id = None;
        snap.characters.insert(gtfo_valid_char, vc);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let make_cs = || CombatState {
        shield_hp: 50_000.0,
        max_shield_hp: 50_000.0,
        armor_hp: 50_000.0,
        max_armor_hp: 50_000.0,
        structure_hp: 50_000.0,
        max_structure_hp: 50_000.0,
        signature_radius: 150.0,
        ..CombatState::default()
    };

    // Out-of-cone target at 90 degrees (0, 5000, 0) when cone points along +X (1, 0, 0)
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: out_of_cone_ball,
                type_id: 587,
                position: DVec3::new(0.0, 5_000.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            make_cs(),
        )
        .await;

    // Reaper target at (2000, 0, 0) for zero-length segment ReaperSweep ([2000, 0, 0] -> [2000, 0, 0])
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: reaper_target_ball,
                type_id: 587,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            make_cs(),
        )
        .await;

    // Tethered ship in GTFO sphere -> skipped!
    let mut tethered_cs = make_cs();
    tethered_cs.is_tethered = true;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: gtfo_tethered_ball,
                type_id: 587,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            tethered_cs,
        )
        .await;

    // Valid subcap ship in GTFO sphere with existing warp suppression entry -> teleported & suppression extended
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: gtfo_valid_ball,
                type_id: 587,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            make_cs(),
        )
        .await;

    for caster_id in [ship_id, ship_id + 1, ship_id + 2] {
        let ball = Ball {
            id: caster_id, type_id: 11_567, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 80.0, inertia: 1.0, mass: 1.0e9, radius: 5_000.0, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = sys.spawn_ball_with_combat(ball, make_cs()).await;
    }

    let now = ctx.now_unix_secs();
    mutate_superweapon_runtime(&ctx.db, |rt| {
        // 1. Cycle still in warmup (elapsed < warmup_secs) -> does not fire yet
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::Cone {
                origin_pos: [0.0, 0.0, 0.0],
                dir_vec: [1.0, 0.0, 0.0],
                cone_len_m: 20_000.0,
                tick_dmg: [1_000.0; 4],
                sig_radius_m: 100.0,
            },
            started_at_unix: now,
            warmup_secs: 60,
            total_ticks: 2,
            ticks_applied: 0,
            immobilize_until_unix: now + 60,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: false,
        });
        // 2. Zero-length ReaperSweep across 3 ticks -> hits reaper_target_ball only ONCE (already_hit_balls dedup)
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id: ship_id + 1,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::ReaperSweep {
                start_pt: [2_000.0, 0.0, 0.0],
                end_pt: [2_000.0, 0.0, 0.0],
                beam_radius_m: 1_000.0,
                sweep_dmg: [1_000.0, 0.0, 0.0, 0.0],
                sig_radius_m: 100.0,
                already_hit_balls: Vec::new(),
            },
            started_at_unix: now.saturating_sub(15),
            warmup_secs: 5,
            total_ticks: 3,
            ticks_applied: 0,
            immobilize_until_unix: now,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
        // 3. SpatialGtfo with pre-existing warp suppression for gtfo_valid_ball
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id: ship_id + 2,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [2_000.0, 0.0, 0.0],
                sphere_radius_m: 5_000.0,
            },
            started_at_unix: now.saturating_sub(15),
            warmup_secs: 5,
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: now,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id,
            ship_id: gtfo_valid_ball,
            suppressed_until_unix: now + 5,
            orig_warp_scramble_strength: 0,
        });
    });

    step_bosonic_doomsday_system(&ctx, sys_id).await;

    // Out-of-cone ball took 0 damage
    let out_cs = sys
        .get_combat_state(out_of_cone_ball)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out_cs.shield_hp, 50_000.0);

    // Reaper target took exactly 1 hit (not 3 hits)
    let reaper_cs = sys
        .get_combat_state(reaper_target_ball)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reaper_cs.shield_hp, 49_000.0);

    // Tethered ball was NOT teleported; valid ball WAS teleported and its suppression extended to now + 60
    let (_, tethered_b) = sys
        .get_ball_snapshot(gtfo_tethered_ball)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tethered_b.position, DVec3::new(2_000.0, 0.0, 0.0));
    let (_, valid_b) = sys
        .get_ball_snapshot(gtfo_valid_ball)
        .await
        .unwrap()
        .unwrap();
    assert!(valid_b.position.distance(DVec3::new(2_000.0, 0.0, 0.0)) > 100_000.0);
    let rt = read_superweapon_runtime(&ctx.db).unwrap();
    assert_eq!(rt.warp_suppressions.len(), 1);
    assert_eq!(rt.warp_suppressions[0].suppressed_until_unix, now + 60);
}
