//! Semantic unit tests split from `mobile_deployables_ext_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::*;

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::mobile_deployables::{
    write_deployable_state, GROUP_MOBILE_MICRO_JUMP_UNIT, GROUP_MOBILE_OBSERVATORY,
    TYPE_MOBILE_OBSERVATORY,
};
use crate::services::mobile_deployables_ext::{
    clear_ship_cloak_activated, flush_deployable_side_effects, handle_mmju_activate,
    record_ship_cloak_activated, step_ext_deployables_in_actor, step_system_deployables_sync,
    GROUP_MOBILE_SIPHON,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, CombatState, DVec3, MovementMode, SlimItemMeta};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mmju_activate_and_observatory_siphon_actor_ticks() {
    let ctx = test_ctx();
    let ls_sys = 30_002_537u32;
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let mmju_id = 771_001i64;
    let obs_id = 771_002i64;
    let siphon_id = 771_003i64;
    let pos_tower_id = 771_004i64;
    let mut sess = auth_session(1, char_id, 98_000_001, ls_sys);
    sess.ship_id = Some(ship_id);
    let now = ctx.now_unix();
    let origin = DVec3::new(2.0e10, 0.0, 0.0);

    let mmju_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == GROUP_MOBILE_MICRO_JUMP_UNIT)
        .map(|t| t.type_id)
        .unwrap_or(33478);
    let siphon_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.group_id == GROUP_MOBILE_SIPHON
                && t.default_attributes.get(&1928).copied().unwrap_or(0.0) > 0.0
        })
        .map(|t| t.type_id)
        .unwrap_or(33477);
    let pos_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 365)
        .map(|t| t.type_id)
        .unwrap_or(12235);
    let raw_mat_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 427 && t.volume > 0.0)
        .map(|t| t.type_id)
        .unwrap_or(16633);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ls_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, qty) in [
            (ship_id, 600u32, ls_sys as i64, 1),
            (mmju_id, mmju_tid, ls_sys as i64, 1),
            (obs_id, TYPE_MOBILE_OBSERVATORY, ls_sys as i64, 1),
            (siphon_id, siphon_tid, ls_sys as i64, 1),
            (pos_tower_id, pos_tid, ls_sys as i64, 1),
            (771_005i64, raw_mat_tid, pos_tower_id, 500),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: qty,
                    singleton: id != 771_005,
                    version: 1,
                    ..Default::default()
                },
            );
            if loc == ls_sys as i64 {
                snap.item_positions.insert(id, origin.to_array());
            }
        }
    });

    write_deployable_state(&ctx, |st| {
        for (id, tid, gid, a_start, a_dur) in [
            (
                mmju_id,
                mmju_tid,
                GROUP_MOBILE_MICRO_JUMP_UNIT,
                now - 120,
                60,
            ),
            (
                obs_id,
                TYPE_MOBILE_OBSERVATORY,
                GROUP_MOBILE_OBSERVATORY,
                now - 1_200,
                600,
            ),
            (siphon_id, siphon_tid, GROUP_MOBILE_SIPHON, now - 7_200, 60),
        ] {
            st.deployables.insert(
                id,
                mk_rec(
                    id,
                    tid,
                    gid,
                    char_id,
                    ls_sys,
                    origin.to_array(),
                    a_start,
                    a_dur,
                    now + 36_000,
                ),
            );
        }
    });

    // MMJU activate without ship ball in space fails ShipNotInSpace
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball within 5 km of MMJU -> activate starts spool -> step_system_deployables_sync jumps ship 100 km
    let sys = ctx.systems.get_or_spawn(ls_sys);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: origin + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::new(100.0, 0.0, 0.0),
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1.0e7,
                radius: 50.0,
                speed_fraction: 0.5,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::X,
                },
            },
            CombatState::new(),
        )
        .await;

    let act_res = handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id)
        .await
        .expect("mmju activate");
    assert!(act_res.dict_get("completeAtUnix").is_some());

    // Advance spool time so step_system_deployables_sync executes the 100 km jump
    write_deployable_state(&ctx, |st| {
        if let Some(entry) = st.mmju_spools.get_mut(&ship_id) {
            entry.complete_at_unix = now;
        }
    });
    step_system_deployables_sync(&ctx, ls_sys);
    let (_, moved_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((moved_ball.position - origin).length() > 90_000.0);

    // Test record_ship_cloak_activated, clear_ship_cloak_activated, and step_ext_deployables_in_actor (Observatory + Siphon)
    record_ship_cloak_activated(&ctx, ship_id);
    clear_ship_cloak_activated(&ctx, ship_id);

    let mut bp = Ballpark::new(ls_sys);
    let mut cloaked_cs = CombatState::new();
    cloaked_cs.is_cloaked = true;
    cloaked_cs.cloak_stabilization_remaining_s = 0.0;
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 600,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1.0e7,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(ship_id, cloaked_cs);
    bp.ball_slim_meta.insert(
        ship_id,
        SlimItemMeta {
            owner_id: char_id,
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    );

    for tick_offset in 0..20 {
        step_ext_deployables_in_actor(
            &mut bp,
            None,
            None,
            &ctx.sde,
            &ctx.db,
            Some(&ctx.mechanic_rng),
            ls_sys,
            now + tick_offset * 600,
        );
    }
    flush_deployable_side_effects(&ctx);

    // Verify Siphon stole raw moon material into siphon_id and wasted from pos_tower_id
    ctx.db.with_snapshot(|snap| {
        let tower_rem = snap.items[&771_005].quantity;
        assert!(tower_rem < 500);
        let stolen_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == siphon_id && i.type_id == raw_mat_tid)
            .map(|i| i.quantity)
            .sum();
        assert!(stolen_qty > 0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coverage_sweep_mmju_errors_observatory_cycles_siphon_overflow_and_sync() {
    let ctx = test_ctx();
    let ls_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.security_status < 0.45 && s.region_id != 10_000_070)
        .map(|(&id, _)| id)
        .unwrap_or(30_000_144);
    let char_id = 90_000_888_i64;
    let ship_id = 888_100_001_i64;
    let mmju_id = 888_100_002_i64;
    let obs_id = 888_100_003_i64;
    let siphon_id = 888_100_004_i64;
    let tower_id = 888_100_005_i64;
    let sess = auth_session(1, char_id, 98_000_001, ls_sys);
    let now = ctx.now_unix();

    // Find a cloak module type (group_id == 330) and armor plate or mass attr 796 item
    let cloak_tid = ctx.sde.types.iter().find(|(_, t)| t.group_id == 330).map(|(&id, _)| id).unwrap_or(11578);
    let poly_tid = ctx.sde.types.iter().find(|(_, t)| t.group_id == 974).map(|(&id, _)| id).unwrap_or(30303);

    // Seed MMJU (anchoring -> DeployableAnchoring, then anchored), Observatory, Siphon, POS tower
    write_deployable_state(&ctx, |st| {
        st.deployables.insert(
            mmju_id,
            crate::services::mobile_deployables::MobileDeployableRecord {
                item_id: mmju_id,
                type_id: 33475,
                group_id: GROUP_MOBILE_MICRO_JUMP_UNIT,
                owner_id: char_id,
                solar_system_id: ls_sys,
                anchor_start_unix: now,
                anchor_duration_secs: 60,
                anchor_completed: false,
                is_unanchored: false,
                expires_at_unix: now + 3600,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                position: [0.0, 0.0, 0.0],
                deployed_fleet_id: None,
            },
        );
        st.deployables.insert(
            obs_id,
            crate::services::mobile_deployables::MobileDeployableRecord {
                item_id: obs_id,
                type_id: TYPE_MOBILE_OBSERVATORY,
                group_id: GROUP_MOBILE_OBSERVATORY,
                owner_id: char_id,
                solar_system_id: ls_sys,
                anchor_start_unix: now - 1000,
                anchor_duration_secs: 60,
                anchor_completed: true,
                is_unanchored: false,
                expires_at_unix: now + 3600,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                position: [0.0, 0.0, 0.0],
                deployed_fleet_id: None,
            },
        );
        st.deployables.insert(
            siphon_id,
            crate::services::mobile_deployables::MobileDeployableRecord {
                item_id: siphon_id,
                type_id: 33477,
                group_id: GROUP_MOBILE_SIPHON,
                owner_id: char_id,
                solar_system_id: ls_sys,
                anchor_start_unix: now - 4000,
                anchor_duration_secs: 60,
                anchor_completed: true,
                is_unanchored: false,
                expires_at_unix: now + 3600,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                position: [500_000.0, 0.0, 0.0],
                deployed_fleet_id: None,
            },
        );
    });

    // Insert MMJU, Observatory, Siphon, POS tower + poly_tid stack + fallback cloak module on ship_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mmju_id,
            InventoryItemRecord {
                item_id: mmju_id,
                type_id: 33475,
                owner_id: char_id,
                location_id: ls_sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            obs_id,
            InventoryItemRecord {
                item_id: obs_id,
                type_id: TYPE_MOBILE_OBSERVATORY,
                owner_id: char_id,
                location_id: ls_sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            siphon_id,
            InventoryItemRecord {
                item_id: siphon_id,
                type_id: 33477,
                owner_id: char_id,
                location_id: ls_sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            tower_id,
            InventoryItemRecord {
                item_id: tower_id,
                type_id: 12235,
                owner_id: char_id,
                location_id: ls_sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(tower_id, [500_000.0, 0.0, 0.0]);
        snap.items.insert(
            888_100_010,
            InventoryItemRecord {
                item_id: 888_100_010,
                type_id: poly_tid,
                owner_id: char_id,
                location_id: tower_id,
                flag: InventoryFlag::Cargo,
                quantity: 5, // small stack -> removed when emptied (L1143)
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            888_100_011,
            InventoryItemRecord {
                item_id: 888_100_011,
                type_id: cloak_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut bp = Ballpark::new(ls_sys);
    let warp_mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000_000.0,
        phase: eve_destiny::WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    let ship_ball_warp = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: warp_mode,
    };
    let mmju_ball = Ball {
        id: mmju_id,
        type_id: 33475,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    bp.add_ball(ship_ball_warp.clone());
    bp.add_ball(mmju_ball.clone());
    let mut cs = CombatState::new();
    cs.warp_scramble_strength = 5;
    bp.combat_states.insert(ship_id, cs.clone());

    let sys_h = ctx.systems.get_or_spawn(ls_sys);
    sys_h.spawn_ball(ship_ball_warp).await.unwrap();
    sys_h.spawn_ball(mmju_ball).await.unwrap();
    sys_h.set_combat_state(ship_id, cs).await.unwrap();

    // 1. handle_mmju_activate error branches: DeployableAnchoring -> ShipInWarp -> OutOfRange -> ShipTooHeavy -> ShipCloaked -> WarpScrambled
    assert_eq!(handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id).await.unwrap_err().code, "DeployableAnchoring");
    write_deployable_state(&ctx, |st| {
        st.deployables.get_mut(&mmju_id).unwrap().anchor_completed = true;
    });
    assert_eq!(handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id).await.unwrap_err().code, "ShipInWarp");
    sys_h.remove_ball(ship_id).await;
    sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(50_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    assert_eq!(handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id).await.unwrap_err().code, "OutOfRange");
    sys_h.remove_ball(ship_id).await;
    sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 2_000_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    assert_eq!(handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id).await.unwrap_err().code, "ShipTooHeavy");
    sys_h.remove_ball(ship_id).await;
    sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    sys_h.try_update_combat_state(ship_id, |c| {
        c.is_cloaked = true;
    });
    assert_eq!(handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id).await.unwrap_err().code, "ShipCloaked");
    sys_h.try_update_combat_state(ship_id, |c| {
        c.is_cloaked = false;
        c.warp_scramble_strength = 5;
    });
    assert_eq!(handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id).await.unwrap_err().code, "WarpScrambled");

    // 2. step_ext_deployables_in_actor ticks Observatory (with active cloak cycle on ship + fallback on ship2) and Siphon (polymer transfer + stack remove)
    let ship2_id = 888_100_020_i64;
    let ship_bp = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ship2_bp = Ball { id: ship2_id, ..ship_bp.clone() };
    bp.add_ball(ship_bp);
    bp.add_ball(ship2_bp);
    let mut cloaked1 = CombatState::new();
    cloaked1.is_cloaked = true;
    cloaked1.cloak_stabilization_remaining_s = 0.0;
    bp.combat_states.insert(ship_id, cloaked1.clone());
    bp.combat_states.insert(ship2_id, cloaked1);
    bp.ball_slim_meta.insert(
        ship_id,
        SlimItemMeta { owner_id: char_id, char_id: Some(char_id), is_cloaked: true, ..Default::default() },
    );
    bp.ball_slim_meta.insert(
        ship2_id,
        SlimItemMeta { owner_id: char_id, char_id: Some(char_id), is_cloaked: true, ..Default::default() },
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            888_100_021,
            InventoryItemRecord {
                item_id: 888_100_021,
                type_id: cloak_tid,
                owner_id: char_id,
                location_id: ship2_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let mut cycles = vec![crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: 888_100_011,
        module_type_id: cloak_tid,
        owner_char_id: Some(char_id),
        effect_name: "effects.Cloak".into(),
        repeat: 1,
        duration_s: 5.0,
        ..Default::default()
    }];
    let hook_called = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hc = hook_called.clone();
    let hook: crate::system_actor::ModuleDeactivationHook = std::sync::Arc::new(move |_cid, _cyc| {
        hc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    record_ship_cloak_activated(&ctx, ship_id);
    record_ship_cloak_activated(&ctx, ship2_id);
    for tick in 0..30 {
        step_ext_deployables_in_actor(
            &mut bp,
            Some(&mut cycles),
            Some(&hook),
            &ctx.sde,
            &ctx.db,
            Some(&ctx.mechanic_rng),
            ls_sys,
            now + tick * 600,
        );
    }
    let _ = hook_called.load(std::sync::atomic::Ordering::Relaxed);
    flush_deployable_side_effects(&ctx);
    clear_ship_cloak_activated(&ctx, ship_id);

    // 3. Seed an MMJU spool that becomes blocked on step_system_deployables_sync (L1271-1275)
    write_deployable_state(&ctx, |st| {
        st.mmju_spools.insert(
            ship_id,
            crate::services::mobile_deployables::MmjuSpoolEntry {
                ship_id,
                char_id,
                unit_id: mmju_id,
                solar_system_id: ls_sys,
                started_unix: now - 20,
                complete_at_unix: now - 1,
                direction: [1.0, 0.0, 0.0],
            },
        );
    });
    sys_h.try_update_combat_state(ship_id, |c| {
        c.warp_scramble_strength = 5;
    });
    step_system_deployables_sync(&ctx, ls_sys);
}
