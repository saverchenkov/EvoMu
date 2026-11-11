//! Behavioral edge-case unit tests for `sp6_lifecycle::disconnect_hold`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode, TargetLock, WarpPhase};
use eve_sde::{DogmaAttributeType, InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_sde_with_siege() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    sde.attributes.insert(
        851,
        DogmaAttributeType {
            attribute_id: 851,
            name: "siegeModeWarpStatus".into(),
            default_value: 0.0,
            high_is_good: false,
            stackable: false,
            published: true,
        },
    );
    let mut attrs = HashMap::new();
    attrs.insert(851, 1.0);
    sde.types.insert(
        20_280,
        InvType {
            type_id: 20_280,
            group_id: 515,
            category_id: 7,
            name: "Siege Module I".into(),
            mass: 100.0,
            volume: 100.0,
            capacity: 0.0,
            radius: 1.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: attrs,
        },
    );
    sde
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        Arc::new(make_sde_with_siege()),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emergency_warp_blocking_tactical_engagement_and_hold_timers_cover_all_sources() {
    let ctx = test_ctx();
    let now = ctx.now_unix_secs();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_id = 700_001_i64;

    // 1. Clean ship: unblocked, no tactical engagement, hold_until == 0
    assert!(!is_disconnect_emergency_warp_blocked(&ctx, &sys, ship_id));
    assert!(!has_tactical_engagement_on_disconnect(&ctx, ship_id));
    assert_eq!(compute_disconnect_hold_until_unix(&ctx, sys_id, ship_id, now), 0);

    // 2. Active immobilizing module in actor_state.active_module_cycles blocks emergency warp
    {
        let mut st = sys.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: 9001,
            module_type_id: 20_280,
            owner_char_id: None,
            effect_name: "siege".into(),
            target_id: None,
            repeat: 0,
            duration_s: 300.0,
            elapsed_s: 10.0,
            cap_need_per_cycle: 0.0,
            speed_boost_factor: 1.0,
            linked_turrets: Vec::new(),
            ammo_type_id: None,
            damage_vector: Default::default(),
            cap_drain_target: 0.0,
            cap_transfer_target: 0.0,
            nosferatu_blood_raider_override: false,
            shield_boost_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            remote_shield_amount: 0.0,
            remote_armor_amount: 0.0,
            remote_hull_amount: 0.0,
            remote_cap_amount: 0.0,
            remote_optimal: 0.0,
            remote_falloff: 0.0,
            mutadaptive_spool_step: 0.0,
            mutadaptive_spool_max: 0.0,
            current_spool_cycles: 0,
            cap_booster_inject_amount: 0.0,
            is_asb: false,
            asb_fallback_cap_need: 0.0,
            is_aar: false,
            aar_paste_rate: 0,
            aar_base_armor_repair: 0.0,
            is_rah: false,
            rah_shift_amount: 0.0,
            is_adc: false,
            ehe_hull_resists: None,
            target_velocity_multiplier: None,
            immobilize_self: true,
            self_disallow_assistance: false,
            self_ewar_immune: false,
            self_disable_mwd_mjd: false,
            self_mass_multiplier: None,
            is_auto_targeter: false,
            auto_target_max_range: 0.0,
            auto_target_max_locks: 0,
            auto_target_scan_res: 0.0,
            crystal_volatility_chance: 0.0,
            crystal_volatility_damage: 0.0,
            is_overloaded: false,
            pending_overloaded: None,
            rack_idx: 0,
            heat_damage_per_cycle: 0.0,
            reactivation_delay_s: 0.0,
            is_mwd_or_mjd: false,
            warp_scramble_points: 0,
            disables_target_mwd_mjd: false,
            warp_core_stab_bonus: 0,
            hic_bubble_radius_m: None,
            hic_velocity_multiplier: None,
            hic_sig_multiplier: None,
            hic_mass_addition: None,
            mjd_jump_distance_m: None,
            mjfg_field_params: None,
            grants_interdiction_nullification: false,
            effective_max_capacitor: None,
            max_effective_range_m: None,
            lance_beam_dir: None,
            grappler_optimal_m: 0.0,
            grappler_falloff_m: 0.0,
            grappler_speed_factor_pct: 0.0,
            started_at_unix: now, ..Default::default() });
    }
    assert!(is_disconnect_emergency_warp_blocked(&ctx, &sys, ship_id));
    sys.actor_state.write().unwrap().active_module_cycles.clear();

    // 3. Lance caster lock triggers nav lock & compute_disconnect_hold_until_unix
    let db_key = Arc::as_ptr(&ctx.db) as usize;
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((db_key, ship_id), now + 240);
    assert!(is_disconnect_emergency_warp_blocked(&ctx, &sys, ship_id));
    assert!(has_tactical_engagement_on_disconnect(&ctx, ship_id));
    assert!(compute_disconnect_hold_until_unix(&ctx, sys_id, ship_id, now) >= now + 240);
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .clear();

    // 4. Clone vat bay activated ship blocks warp & adds NPC_TIMER_SECS hold
    ctx.tactical_state
        .write()
        .unwrap()
        .clone_vat_bay_activated_ships
        .insert((db_key, ship_id));
    assert!(is_disconnect_emergency_warp_blocked(&ctx, &sys, ship_id));
    assert!(has_tactical_engagement_on_disconnect(&ctx, ship_id));
    assert_eq!(
        compute_disconnect_hold_until_unix(&ctx, sys_id, ship_id, now),
        now + NPC_TIMER_SECS
    );
    ctx.tactical_state
        .write()
        .unwrap()
        .clone_vat_bay_activated_ships
        .clear();

    // 5. Fitted active siege module in snap.module_active_states triggers immobilizing check
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_010,
            InventoryItemRecord {
                item_id: 700_010,
                type_id: 20_280,
                owner_id: 1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(700_010);
    });
    assert!(is_disconnect_emergency_warp_blocked(&ctx, &sys, ship_id));
    assert!(has_tactical_engagement_on_disconnect(&ctx, ship_id));
    assert_eq!(
        compute_disconnect_hold_until_unix(&ctx, sys_id, ship_id, now),
        now + NPC_TIMER_SECS
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collect_expired_and_finalize_disconnected_ships_handle_filtering_extension_and_cleanup() {
    let ctx = test_ctx();
    let now = ctx.now_unix_secs();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // Empty returns empty
    assert!(collect_expired_disconnected_ships(&ctx, now).is_empty());

    let c_warping = 92_000_001_i64;
    let c_stationary = 92_000_002_i64;
    let c_held = 92_000_003_i64;
    let c_already_removed = 92_000_004_i64;

    let sh_warping = 710_001_i64;
    let sh_stationary = 710_002_i64;
    let sh_held = 710_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for sh in [sh_warping, sh_stationary, sh_held] {
            snap.items.insert(
                sh,
                InventoryItemRecord {
                    item_id: sh,
                    type_id: 587,
                    owner_id: 1,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.sp6_lifecycle.disconnected_ships.insert(
            c_warping,
            DisconnectedShipEntry {
                char_id: c_warping,
                ship_id: sh_warping,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 100,
                remove_at_unix: now - 10,
                emergency_warp_dest: Some([1e9, 0.0, 0.0]),
                last_position: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: Some([0.0, 0.0, 0.0]),
            },
        );
        snap.sp6_lifecycle.disconnected_ships.insert(
            c_stationary,
            DisconnectedShipEntry {
                char_id: c_stationary,
                ship_id: sh_stationary,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 100,
                remove_at_unix: now - 10,
                emergency_warp_dest: None,
                last_position: [100.0, 200.0, 300.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        snap.sp6_lifecycle.disconnected_ships.insert(
            c_held,
            DisconnectedShipEntry {
                char_id: c_held,
                ship_id: sh_held,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 100,
                remove_at_unix: now - 5,
                emergency_warp_dest: None,
                last_position: [50.0, 60.0, 70.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        snap.sp6_lifecycle.disconnected_ships.insert(
            c_already_removed,
            DisconnectedShipEntry {
                char_id: c_already_removed,
                ship_id: 710_004,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now - 200,
                remove_at_unix: now - 100,
                emergency_warp_dest: None,
                last_position: [0.0, 0.0, 0.0],
                removed_from_space: true,
                pre_warp_position: None,
            },
        );
    });

    // Hold c_held via clone_vat_bay_activated_ships
    let db_key = Arc::as_ptr(&ctx.db) as usize;
    ctx.tactical_state
        .write()
        .unwrap()
        .clone_vat_bay_activated_ships
        .insert((db_key, sh_held));

    // Spawn ball only for sh_stationary to test ball-present vs ball-absent finalization
    let _ = sys
        .spawn_ball(Ball {
            id: sh_stationary,
            type_id: 587,
            position: DVec3::new(999.0, 888.0, 777.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // only_non_warping = true expires only c_stationary (c_held gets extended, c_warping skipped)
    let non_warping = collect_expired_disconnected_ships_filtered(&ctx, now, true);
    assert_eq!(non_warping.len(), 1);
    assert_eq!(non_warping[0].char_id, c_stationary);

    // c_held's remove_at_unix was extended to now + NPC_TIMER_SECS
    let held_remove = ctx.db.with_snapshot(|s| {
        s.sp6_lifecycle.disconnected_ships[&c_held].remove_at_unix
    });
    assert!(held_remove >= now + NPC_TIMER_SECS);

    // collect_expired_disconnected_ships expires c_warping now
    let warping_expired = collect_expired_disconnected_ships(&ctx, now);
    assert_eq!(warping_expired.len(), 1);
    assert_eq!(warping_expired[0].char_id, c_warping);

    // Finalize both: sh_stationary uses ball position (999,888,777); sh_warping falls back to last_position
    finalize_expired_disconnected_ship(&ctx, non_warping.into_iter().next().unwrap());
    finalize_expired_disconnected_ship(&ctx, warping_expired.into_iter().next().unwrap());

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions.get(&sh_stationary), Some(&[999.0, 888.0, 777.0]));
        assert_eq!(snap.item_positions.get(&sh_warping), Some(&[0.0, 0.0, 0.0]));
        assert!(snap.sp6_lifecycle.disconnected_ships[&c_stationary].removed_from_space);
        assert!(snap.sp6_lifecycle.disconnected_ships[&c_warping].removed_from_space);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_disconnected_ship_entry_covers_emergency_warp_obstacle_deflection_committed_warp_and_scram() {
    let ctx = test_ctx();
    let now = ctx.now_unix_secs();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Emergency warp with nearby immobile structure triggers collision deflection
    let c1 = 93_000_001_i64;
    let sh1 = 720_001_i64;
    let raw_dest = super::super::compute_emergency_warp_destination(DVec3::ZERO, c1, sh1);
    let raw_dir = raw_dest.normalize();
    // Place an immobile ball directly in front of the raw emergency warp direction
    let _ = sys
        .spawn_ball(Ball {
            id: 720_999,
            type_id: 35832,
            position: raw_dir * 500.0,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 1_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: sh1,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let entry1 = build_disconnected_ship_entry(
        &ctx,
        &sys,
        c1,
        sh1,
        587,
        sys_id,
        DVec3::ZERO,
        3.0,
        now,
    )
    .await;
    assert!(entry1.emergency_warp_dest.is_some());
    assert_eq!(entry1.pre_warp_position, Some([0.0, 0.0, 0.0]));
    assert!(entry1.remove_at_unix >= now + super::super::UNAGGRESSED_LOGOFF_REMOVE_SECS);
    let cs1 = sys.get_combat_state(sh1).await.unwrap().unwrap();
    assert!(cs1.is_emergency_warping);

    // 2. Committed mid-tunnel warp preserves committed destination and pre_warp_position = None
    let c2 = 93_000_002_i64;
    let sh2 = 720_002_i64;
    let committed_dest = DVec3::new(5_000_000.0, 0.0, 0.0);
    let _ = sys
        .spawn_ball(Ball {
            id: sh2,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: committed_dest,
                warp_speed_au_s: 3.0,
                total_distance: 5_000_000.0,
                phase: WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        })
        .await;
    let entry2 = build_disconnected_ship_entry(
        &ctx,
        &sys,
        c2,
        sh2,
        587,
        sys_id,
        DVec3::new(10_000.0, 0.0, 0.0),
        3.0,
        now,
    )
    .await;
    assert_eq!(entry2.emergency_warp_dest, Some([5_000_000.0, 0.0, 0.0]));
    assert_eq!(entry2.pre_warp_position, None);

    // 3. Highsec criminal + aligning stops alignment and suppresses emergency warp
    let c3 = 93_000_003_i64;
    let sh3 = 720_003_i64;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, 0.9);
    });
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(c3)
        .or_default()
        .criminal_flag_until_unix = now + 600;
    let _ = sys
        .spawn_ball(Ball {
            id: sh3,
            type_id: 587,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.5,
            mode: MovementMode::Warp {
                origin: DVec3::new(20_000.0, 0.0, 0.0),
                destination: DVec3::new(9_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 8_980_000.0,
                phase: WarpPhase::Aligning,
                elapsed_warp_time: 0.5,
            },
        })
        .await;
    let entry3 = build_disconnected_ship_entry(
        &ctx,
        &sys,
        c3,
        sh3,
        587,
        sys_id,
        DVec3::new(20_000.0, 0.0, 0.0),
        3.0,
        now,
    )
    .await;
    assert_eq!(entry3.emergency_warp_dest, None);
    assert_eq!(entry3.pre_warp_position, None);
    assert!(entry3.remove_at_unix >= now + 600);

    // 4. Abyssal system (32_000_001) + target lock sets NPC_TIMER_SECS and suppresses warp
    let abyss_sys = 32_000_001_u32;
    let ab_handle = ctx.systems.get_or_spawn(abyss_sys);
    let _ = ab_handle
        .spawn_ball(Ball {
            id: 720_004,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = ab_handle
        .add_target_lock(
            720_004,
            TargetLock {
                target_id: 999_999,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    let entry4 = build_disconnected_ship_entry(
        &ctx,
        &ab_handle,
        93_000_004,
        720_004,
        587,
        abyss_sys,
        DVec3::ZERO,
        3.0,
        now,
    )
    .await;
    assert_eq!(entry4.emergency_warp_dest, None);
    assert!(entry4.remove_at_unix >= now + NPC_TIMER_SECS);
}
