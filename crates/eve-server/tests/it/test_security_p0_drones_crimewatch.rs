//! Integration tests for crimewatch timer notifications and drone lifecycle/control.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneSpaceState,
};
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    common::pin_clock(&ctx);
    ctx
}

fn seed_item(
    ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64,
    location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default()
        });
    });
}

#[tokio::test]
async fn test_crimewatch_states_timer_updates() {
    let ctx = new_ctx();
    let char_id = 90_000_001i64;
    let now = ctx.now_unix();
    let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
    let _ = ctx.db.acquire_online_lock(1, 7111, Some(char_id), true, now);

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 300;
        st.criminal_flag_until_unix = now + 900;
    }

    let _ = ctx.drain_notifications_for_char(char_id);
    eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(
        &ctx, char_id, now,
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.len(), 6);
    assert!(notifs.iter().any(|n| n.event == "OnWeaponsTimerUpdate" && n.args == vec![PyRep::Integer(101), PyRep::Integer(unix_to_ft(now + 60))]));
    assert!(notifs.iter().any(|n| n.event == "OnPvpTimerUpdate" && n.args == vec![PyRep::Integer(201), PyRep::Integer(unix_to_ft(now + 300))]));
    assert!(notifs.iter().any(|n| n.event == "OnNpcTimerUpdate" && n.args == vec![PyRep::Integer(400), PyRep::None]));
    assert!(notifs.iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args == vec![PyRep::Integer(301), PyRep::Integer(unix_to_ft(now + 900))]));
    assert!(notifs.iter().any(|n| n.event == "OnDisapprovalTimerUpdate" && n.args == vec![PyRep::Integer(500), PyRep::None]));
    assert!(notifs.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates" && n.args == vec![PyRep::List(vec![]), PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(char_id)])]));

    // Re-sweep at same timestamp must be de-duplicated (no redundant notification queueing)
    eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, char_id, now);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "Redundant timer sweep must be de-duplicated");

    // Advance time past all timers -> active to idle transition
    let exp_now = now + 1000;
    eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, char_id, exp_now);
    let exp_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(exp_notifs.len(), 4);
    assert!(exp_notifs.iter().any(|n| n.event == "OnWeaponsTimerUpdate" && n.args == vec![PyRep::Integer(100), PyRep::None]));
    assert!(exp_notifs.iter().any(|n| n.event == "OnPvpTimerUpdate" && n.args == vec![PyRep::Integer(200), PyRep::None]));
    assert!(exp_notifs.iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args == vec![PyRep::Integer(300), PyRep::None]));
    assert!(exp_notifs.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates" && n.args == vec![PyRep::List(vec![PyRep::Integer(char_id)]), PyRep::List(vec![]), PyRep::List(vec![])]));

    // Pilot must be pruned from crimewatch_timer_states once all timers are idle
    let is_tracked = with_combat_runtime(&ctx, |rt| rt.crimewatch_timer_states.contains_key(&char_id));
    assert!(!is_tracked, "Pilot state must be pruned when all timers return to idle");

    // Subsequent sweeps for an idle pilot must NOT recreate or re-notify
    eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, char_id, exp_now + 10);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    assert!(!with_combat_runtime(&ctx, |rt| rt.crimewatch_timer_states.contains_key(&char_id)));
}

#[tokio::test]
async fn test_drones_recall_reconnect_abandon_control() {
    // parity: feat:drones.recall-drones, feat:drones.reconnect-drones, rpc:entity.CmdAbandonDrone, rpc:entity.CmdReconnectToDrones
    eve_testkit::attest(&[
        "feat:drones.recall-drones",
        "feat:drones.reconnect-drones",
        "rpc:entity.CmdAbandonDrone",
        "rpc:entity.CmdReconnectToDrones",
    ]);

    let ctx = new_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7110);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 626,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let d1 = 850_001i64;
    let d2 = 850_002i64;
    let d3 = 850_003i64;
    for &did in &[d1, d2, d3] {
        seed_item(&ctx, did, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: did,
                type_id: 2464,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        with_mut_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.insert(
                did,
                DroneSpaceState {
                    ball_id: did,
                    drone_item_id: did,
                    type_id: 2464,
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1000.0,
                    optimal_range: 5000.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 0.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
        });
    }

    // 1. Abandon ONLY d2
    let abandon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdAbandonDrone",
            &[PyRep::List(vec![PyRep::Integer(d2)])],
            &kw,
            None,
        )
        .await
        .expect("CmdAbandonDrone");
    assert_eq!(abandon_res, PyRep::Dict(Vec::new()));

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.get(&d1).unwrap().is_abandoned);
        assert!(rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
        assert!(!rt.active_drones_in_space.get(&d3).unwrap().is_abandoned);
    });

    // 2. Reconnect ONLY d2
    let reconn_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdReconnectToDrones",
            &[PyRep::List(vec![PyRep::Integer(d2)])],
            &kw,
            None,
        )
        .await
        .expect("CmdReconnectToDrones");
    assert_eq!(reconn_res, PyRep::Dict(vec![]));

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
    });

    // 3. Stargate jump / dock abandons drones in space
    eve_server::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, sys_id);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space.get(&d1).unwrap().is_abandoned);
        assert!(rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
        assert!(rt.active_drones_in_space.get(&d3).unwrap().is_abandoned);
        assert_eq!(rt.active_drones_in_space.get(&d1).unwrap().original_owner_char_id, char_id);
    });

    // Reconnect d1
    let reconn_d1 = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdReconnectToDrones",
            &[PyRep::List(vec![PyRep::Integer(d1)])],
            &kw,
            None,
        )
        .await
        .expect("CmdReconnectToDrones d1");
    assert_eq!(reconn_d1, PyRep::Dict(vec![]));
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.get(&d1).unwrap().is_abandoned);
        assert!(rt.active_drones_in_space.get(&d2).unwrap().is_abandoned);
    });

    // 4. Return to bay
    let recall_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdReturnBay",
            &[PyRep::List(vec![PyRep::Integer(d1)])],
            &kw,
            None,
        )
        .await
        .expect("CmdReturnBay");
    assert_eq!(recall_res, PyRep::Dict(vec![]));
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&d1));
    });
}

#[tokio::test]
async fn test_drone_mine_repeatedly_and_shipping_storage() {
    // parity: rpc:entity.CmdMineRepeatedly, rpc:entity.DisableBehaviorDebugging, rpc:entity.OpenShippingUnitStorage
    eve_testkit::attest(&[
        "rpc:entity.CmdMineRepeatedly",
        "rpc:entity.DisableBehaviorDebugging",
        "rpc:entity.OpenShippingUnitStorage",
    ]);

    let ctx = new_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7111);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let other_char_id = 90_000_002i64;
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 626,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. Behavior debugging GM check
    let err_non_gm = ctx
        .dispatch_rpc(&mut sess, "entity", None, "EnableBehaviorDebugging", &[], &kw, None)
        .await
        .expect_err("Non-GM must fail");
    assert_eq!(err_non_gm.code, "InsufficientRole");

    sess.role |= eve_db::service_roles::ROLE_GMS;
    let ok_gm = ctx
        .dispatch_rpc(&mut sess, "entity", None, "EnableBehaviorDebugging", &[], &kw, None)
        .await
        .expect("GM can enable behavior debugging");
    assert_eq!(ok_gm.dict_get("behaviorDebuggingEnabled").and_then(|v| v.as_bool()), Some(true));

    let disable_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "DisableBehaviorDebugging",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GM can disable behavior debugging");
    assert_eq!(disable_res.dict_get("behaviorDebuggingEnabled").and_then(|v| v.as_bool()), Some(false));

    // 2. Shipping storage
    let unit_id = 860_001i64;
    let cargo_item = 860_002i64;
    seed_item(&ctx, unit_id, 24445, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, cargo_item, 34, other_char_id, unit_id, InventoryFlag::Cargo, 100, false);

    let _ = sys_handle
        .spawn_ball(Ball {
            id: unit_id,
            type_id: 24445,
            position: DVec3::new(2000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    sess.role = 0;
    let access_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "OpenShippingUnitStorage",
            &[PyRep::Integer(unit_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Unauthorized caller denied");
    assert_eq!(access_err.code, "AccessDenied");

    sess.role |= eve_db::service_roles::ROLE_GMS;
    let open_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "OpenShippingUnitStorage",
            &[PyRep::Integer(unit_id)],
            &kw,
            None,
        )
        .await
        .expect("Authorized open");
    assert_eq!(open_res.dict_get("storageOpened").and_then(|v| v.as_bool()), Some(true));

    let bind = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(15)]), PyRep::None], &kw, None,
    ).await.expect("bind invbroker");
    let mgr = match bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
        PyRep::Integer(i) => i as u64,
        _ => panic!("unexpected bind rep"),
    };
    let inv_rep = ctx.dispatch_rpc(
        &mut sess, "invbroker", Some(mgr), "GetInventoryFromId",
        &[PyRep::Integer(unit_id), PyRep::Integer(0)], &kw, None,
    ).await.expect("GetInventoryFromId");
    let inv_bid = inv_rep.as_i64().expect("inv bound id") as u64;
    let unit_items = ctx.dispatch_rpc(&mut sess, "invbroker", Some(inv_bid), "List", &[], &kw, None)
        .await.expect("List shipping unit storage");
    assert!(matches!(unit_items, PyRep::ObjectEx { .. } | PyRep::List(_)));

    // 3. CmdMineRepeatedly
    let miner_drone_id = 860_010i64;
    let asteroid_id = 860_020i64;
    seed_item(&ctx, miner_drone_id, 10250, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            miner_drone_id,
            DroneSpaceState {
                ball_id: miner_drone_id,
                drone_item_id: miner_drone_id,
                type_id: 10250,
                group_id: 101,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let _ = sys_handle
        .spawn_ball(Ball {
            id: miner_drone_id,
            type_id: 10250,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 0.5,
            mass: 10_000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id,
        solar_system_id: sys_id,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 100_000,
        initial_quantity: 100_000,
        position: (5000.0, 0.0, 0.0),
        radius: 100.0,
    });
    let _ = sys_handle
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 100_000,
                remaining_units: 100_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: asteroid_id,
                type_id: 1230,
                position: DVec3::new(5000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    let alien_drone_id = 860_011i64;
    seed_item(&ctx, alien_drone_id, 2464, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            alien_drone_id,
            DroneSpaceState {
                ball_id: alien_drone_id,
                drone_item_id: alien_drone_id,
                type_id: 2464,
                group_id: 100,
                owner_char_id: other_char_id,
                original_owner_char_id: other_char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let alien_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(alien_drone_id)]), PyRep::Integer(asteroid_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Non-owned drone rejected");
    assert_eq!(alien_err.code, "InvalidDroneOwner");

    let mine_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(miner_drone_id)]), PyRep::Integer(asteroid_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMineRepeatedly");
    assert_eq!(mine_res, PyRep::Dict(vec![]));

    let (initial_ast_qty, next_tick) = with_combat_runtime(&ctx, |rt| {
        let miner_st = rt.active_drone_miners.get(&miner_drone_id).unwrap();
        assert_eq!(miner_st.asteroid_id, asteroid_id);
        assert_eq!(rt.active_drones_in_space.get(&miner_drone_id).unwrap().activity_state, 2);
        (
            ctx.db.with_snapshot(|s| s.asteroids.get(&asteroid_id).map(|a| a.quantity)).unwrap_or(0),
            miner_st.next_cycle_unix,
        )
    });

    // Step repeating cycle 1
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_tick).await;
    let after_cycle1_qty = ctx.db.with_snapshot(|s| s.asteroids.get(&asteroid_id).map(|a| a.quantity)).unwrap_or(0);
    assert!(after_cycle1_qty < initial_ast_qty, "Asteroid quantity must decrement in snapshot");
    assert!(!ctx.db.get_mining_ledger(char_id).is_empty(), "Mining ledger must record mined ore");

    // Step repeating cycle 2
    let next_tick_2 = with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners.get(&miner_drone_id).map(|m| m.next_cycle_unix)
    }).unwrap_or(0);
    assert!(next_tick_2 > next_tick);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_tick_2).await;
    let after_cycle2_qty = ctx.db.with_snapshot(|s| s.asteroids.get(&asteroid_id).map(|a| a.quantity)).unwrap_or(0);
    assert!(after_cycle2_qty < after_cycle1_qty);
}

#[tokio::test]
async fn test_drone_mine_repeatedly_duplicate_ids_dedup_and_reject_active() {
    let ctx = new_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7119);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 626, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 0.5, mass: 1_000_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let drone_id = 860_050i64;
    let ast_id = 860_060i64;
    seed_item(&ctx, drone_id, 10250, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(drone_id, DroneSpaceState {
            ball_id: drone_id, drone_item_id: drone_id, type_id: 10250, group_id: 101,
            owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
            solar_system_id: sys_id, target_id: None, activity_state: 0, assist_char_id: None,
            guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
            max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
            shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
            web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
    });

    let _ = sys_handle.spawn_ball(Ball {
        id: drone_id, type_id: 10250, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 0.5, mass: 10_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ast_id, solar_system_id: sys_id, belt_id: 40_000_001, type_id: 1230,
        quantity: 50_000, initial_quantity: 50_000, position: (3000.0, 0.0, 0.0), radius: 50.0,
    });
    let _ = sys_handle.spawn_asteroid(
        eve_server::system_actor::AsteroidState {
            asteroid_id: ast_id, belt_id: 40_000_001, type_id: 1230, initial_units: 50_000,
            remaining_units: 50_000, unit_volume: 0.1, depleted: false, respawn_at_tick: None,
        },
        Ball {
            id: ast_id, type_id: 1230, position: DVec3::new(3000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
    ).await;

    // Call with duplicate IDs [drone_id, drone_id, drone_id]
    let res = ctx.dispatch_rpc(
        &mut sess, "entity", None, "CmdMineRepeatedly",
        &[PyRep::List(vec![PyRep::Integer(drone_id), PyRep::Integer(drone_id), PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
        &kw, None,
    ).await.expect("CmdMineRepeatedly with duplicate IDs");
    assert_eq!(res, PyRep::Dict(vec![]));

    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drone_miners.len()), 1, "Duplicate drone IDs must be de-duplicated to 1 miner");

    // Calling again while already mining must fail
    let err = ctx.dispatch_rpc(
        &mut sess, "entity", None, "CmdMineRepeatedly",
        &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
        &kw, None,
    ).await.expect_err("Already mining drone must be rejected");
    assert_eq!(err.code, "DroneAlreadyMining");
}
