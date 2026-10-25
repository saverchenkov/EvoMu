//! Integration tests for drone return-to-bay scoop tick / prevalidation (`BF-3`) and shipping storage proximity (`BF-4`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
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
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn test_drones_return_bay_prevalidation_and_tick_scoop() {
    // parity: rpc:entity.CmdReturnBay
    eve_testkit::attest(&["rpc:entity.CmdReturnBay"]);

    let ctx = new_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7115);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());

    seed_item(&ctx, ship_id, 626, char_id, sys_id as i64, InventoryFlag::None, 1, true); // Vexor
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 626,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 195.0,
            inertia: 0.56,
            mass: 10_000_000.0,
            radius: 145.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Drone 1: mobile drone close (1,000 m)
    let mobile_close = 891_001i64;
    seed_item(&ctx, mobile_close, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: mobile_close,
            type_id: 2464,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 0.5,
            mass: 10_000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Drone 2: sentry drone far away (10,000 m)
    let sentry_far = 891_002i64;
    seed_item(&ctx, sentry_far, 28213, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: sentry_far,
            type_id: 28213,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            mobile_close,
            DroneSpaceState {
                ball_id: mobile_close,
                drone_item_id: mobile_close,
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
        rt.active_drones_in_space.insert(
            sentry_far,
            DroneSpaceState {
                ball_id: sentry_far,
                drone_item_id: sentry_far,
                type_id: 28213,
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
                is_sentry: true,
                bandwidth_used: 25.0,
                max_velocity: 0.0,
                optimal_range: 50000.0,
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

    // 1. Pre-validation: Calling CmdReturnBay with both drones must fail because sentry is out of range,
    // and mobile drone must NOT be scooped!
    let pre_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdReturnBay",
            &[PyRep::List(vec![PyRep::Integer(mobile_close), PyRep::Integer(sentry_far)])],
            &kw,
            None,
        )
        .await
        .expect_err("Mixed sentry request must fail prevalidation");
    assert_eq!(pre_err.code, "SentryDroneOutOfScoopRange");

    // Both drones must still be in space
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space.contains_key(&mobile_close), "Mobile drone must not be scooped on error");
        assert!(rt.active_drones_in_space.contains_key(&sentry_far), "Sentry drone must not be scooped");
    });
    ctx.db.with_snapshot(|snap| {
        assert_ne!(snap.items.get(&mobile_close).unwrap().flag, InventoryFlag::DroneBay);
    });

    // 2. Returning mobile drone from > 2,500 m
    // Move mobile drone to 6,000 m
    let _ = sys_handle
        .spawn_ball(Ball {
            id: mobile_close,
            type_id: 2464,
            position: DVec3::new(6000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 0.5,
            mass: 10_000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = ctx.drain_notifications_for_char(char_id);
    let ret_res = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdReturnBay",
            &[PyRep::List(vec![PyRep::Integer(mobile_close)])],
            &kw,
            None,
        )
        .await
        .expect("Mobile drone returns");
    // Not immediately scooped -> empty dict in RPC return
    assert_eq!(ret_res, PyRep::Dict(vec![]));

    // Drone state must be returning (activity_state = 4)
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space.get(&mobile_close).unwrap().activity_state, 4);
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnDroneStateChange"));

    // 3. Cluster tick scoops returning drone when within 2,500 m
    // Move drone ball within 2,000 m
    let _ = sys_handle
        .spawn_ball(Ball {
            id: mobile_close,
            type_id: 2464,
            position: DVec3::new(2000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 0.5,
            mass: 10_000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    eve_server::sp6_lifecycle::step_returning_drones(&ctx).await;

    // Drone must now be scooped into bay
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&mobile_close), "Returning drone must be scooped on tick");
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&mobile_close).unwrap().flag, InventoryFlag::DroneBay);
        assert_eq!(snap.items.get(&mobile_close).unwrap().location_id, ship_id);
    });

    let tick_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(tick_notifs.iter().any(|n| n.event == "OnDroneStateChange"));
    assert!(tick_notifs.iter().any(|n| n.event == "OnItemChange"));

    // 4. Drones left behind on warp stay owned and cannot be stolen by third parties (R-DRN-01)
    let warp_drone = 892_020i64;
    seed_item(&ctx, warp_drone, 2464, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(warp_drone, DroneSpaceState {
            ball_id: warp_drone, drone_item_id: warp_drone, type_id: 2464, group_id: 100,
            owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
            solar_system_id: sys_id, target_id: None, activity_state: 1, assist_char_id: None,
            guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
            max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
            shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
            web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
    });
    let _ = sys_handle.spawn_ball(Ball {
        id: warp_drone, type_id: 2464, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 0.5, mass: 10_000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    eve_server::sp6_lifecycle::disconnect_drones_for_ship_on_warp_sync(&ctx, char_id, ship_id, sys_id);

    with_combat_runtime(&ctx, |rt| {
        let d = rt.active_drones_in_space.get(&warp_drone).expect("drone still in space");
        assert!(!d.is_abandoned, "Drone must NOT be abandoned on warp");
        assert_eq!(d.owner_char_id, char_id, "Drone owner must be preserved");
        assert_eq!(d.parent_ship_id, 0, "Drone parent ship must be cleared");
    });

    let third_char = 90_000_003i64;
    let third_ship = 100_000_003i64;
    let mut third_sess = SessionState::new(7129);
    common::login_pilot(&ctx, &mut third_sess, "demo", "demo-secret").await;
    third_sess.character_id = Some(third_char);
    third_sess.ship_id = Some(third_ship);
    third_sess.solar_system_id = Some(sys_id);
    seed_item(&ctx, third_ship, 587, third_char, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = sys_handle.spawn_ball(Ball {
        id: third_ship, type_id: 587, position: DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let scoop_err = ctx.dispatch_rpc(
        &mut third_sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(warp_drone)], &kw, None,
    ).await.expect_err("Third party scoop must be rejected");
    assert_eq!(scoop_err.code, "DroneOwnedByOther");
}

#[tokio::test]
async fn test_shipping_storage_proximity_and_fleetmate_transfer() {
    // parity: rpc:entity.OpenShippingUnitStorage
    eve_testkit::attest(&["rpc:entity.OpenShippingUnitStorage"]);

    let ctx = new_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7116);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());

    seed_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_250_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. OpenShippingUnitStorage on a ship (587) must fail with InvalidTarget
    let other_char_id = 90_000_002i64;
    let ship_target = 892_001i64;
    seed_item(&ctx, ship_target, 587, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_target,
            type_id: 587,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_250_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let target_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "OpenShippingUnitStorage",
            &[PyRep::Integer(ship_target)],
            &kw,
            None,
        )
        .await
        .expect_err("Calling OpenShippingUnitStorage on ship must fail");
    assert_eq!(target_err.code, "InvalidTarget");

    // 2. Open container (Giant Freight Container, type 24445) at 2,000 m
    let container_id = 892_002i64;
    seed_item(&ctx, container_id, 24445, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: container_id,
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

    // Put them in the same fleet so caller is authorized
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            5001,
            eve_db::FleetRecord {
                fleet_id: 5001,
                boss_id: other_char_id,
                members: vec![char_id, other_char_id],
                ..Default::default()
            },
        );
    });

    let open_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "OpenShippingUnitStorage",
            &[PyRep::Integer(container_id)],
            &kw,
            None,
        )
        .await
        .expect("Fleetmate open container succeeds");
    assert_eq!(open_ok.dict_get("storageOpened").and_then(|v| v.as_bool()), Some(true));

    // Bind invbroker
    let bind = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(15)]), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind invbroker");
    let mgr = match bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
        PyRep::Integer(i) => i as u64,
        _ => panic!("unexpected bind rep"),
    };

    // Access within 2,000 m succeeds
    let inv_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetInventoryFromId in range succeeds");
    assert!(inv_ok.as_i64().is_some());

    // 3. Move caller > 5,000 m away (to 10,000 m): GetInventoryFromId must fail
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_250_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("Out of range inventory access must fail");
    assert_eq!(range_err.code, "InventoryNotAccessible");

    // 4. Move ship back in range (2,000 m)
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(2000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_250_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Seed item in caller ship cargo
    let test_item_id = 892_005i64;
    seed_item(&ctx, test_item_id, 34, char_id, ship_id, InventoryFlag::Cargo, 100, false);

    // Moving item into container while in fleet succeeds
    let add_to_container = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "Add",
            &[PyRep::Integer(test_item_id), PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(100)],
            &kw,
            None,
        )
        .await
        .expect("Moving item into shipping unit while in fleet must succeed");
    assert!(matches!(add_to_container, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&test_item_id).expect("item in DB");
        assert_eq!(it.location_id, container_id);
    });

    // Moving item out of container back to ship while in fleet succeeds
    let take_from_container = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "Add",
            &[PyRep::Integer(test_item_id), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(100)],
            &kw,
            None,
        )
        .await
        .expect("Taking item from shipping unit while in fleet must succeed");
    assert!(matches!(take_from_container, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&test_item_id).expect("item in DB");
        assert_eq!(it.location_id, ship_id);
    });

    // Move back into container for leave-fleet test
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "Add",
            &[PyRep::Integer(test_item_id), PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(100)],
            &kw,
            None,
        )
        .await
        .expect("Put item back into container");

    // NF-DEADLOCK-ALSC: Fleetmate take from group-448 ALSC in space must not deadlock and logs action
    let alsc_id = 892_010i64;
    let alsc_item = 892_011i64;
    seed_item(&ctx, alsc_id, 17365, other_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, alsc_item, 34, other_char_id, alsc_id, InventoryFlag::None, 25, false);
    let _ = sys_handle.spawn_ball(Ball {
        id: alsc_id, type_id: 17365, position: DVec3::new(1500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = ctx.dispatch_rpc(&mut sess, "entity", None, "OpenShippingUnitStorage", &[PyRep::Integer(alsc_id)], &kw, None).await.expect("open ALSC");
    let alsc_take = ctx.dispatch_rpc(
        &mut sess, "invbroker", Some(mgr), "Add",
        &[PyRep::Integer(alsc_item), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(25)], &kw, None,
    ).await.expect("Fleetmate take from ALSC must succeed without deadlock");
    assert!(matches!(alsc_take, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
    let log_res = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr), "ALSCLogGet", &[PyRep::Integer(alsc_id)], &kw, None)
        .await
        .expect("ALSCLogGet");
    let Some(PyRep::List(entries)) = log_res.dict_get("entries") else { panic!("expected entries list"); };
    assert!(entries.iter().any(|e| e.dict_get("action").and_then(|v| v.as_str()) == Some("ALSCActionTake") && e.dict_get("actorID").and_then(|v| v.as_i64()) == Some(char_id)));

    // NF-FLEET-SHIP-CARGO-THEFT: Fleetmate cannot steal cargo from fleetmate ship (flag 5)
    let alt_cargo_item = 892_012i64;
    seed_item(&ctx, alt_cargo_item, 34, other_char_id, ship_target, InventoryFlag::Cargo, 10, false);
    let theft_err = ctx.dispatch_rpc(
        &mut sess, "invbroker", Some(mgr), "Add",
        &[PyRep::Integer(alt_cargo_item), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(10)], &kw, None,
    ).await.expect_err("Taking cargo from fleetmate ship cargo must fail");
    assert_eq!(theft_err.code, "ItemOwnershipViolation");

    // 5. Caller leaves the fleet: authorization must be revoked
    sess.fleet_id = Some(5001);
    let _ = ctx
        .dispatch_rpc(&mut sess, "fleetMgr", None, "LeaveFleet", &[], &kw, None)
        .await
        .expect("Leave fleet");

    // Attempting to take item out of container after leaving fleet must be rejected
    let take_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "Add",
            &[PyRep::Integer(test_item_id), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(100)],
            &kw,
            None,
        )
        .await
        .expect_err("Taking item from container after leaving fleet must fail");
    assert_eq!(take_err.code, "ItemOwnershipViolation");

    // Attempting to add item into container after leaving fleet must be rejected
    let seed_item2 = 892_006i64;
    seed_item(&ctx, seed_item2, 34, char_id, ship_id, InventoryFlag::Cargo, 50, false);
    let add_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "Add",
            &[PyRep::Integer(seed_item2), PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(50)],
            &kw,
            None,
        )
        .await
        .expect_err("Adding item into container after leaving fleet must fail");
    assert_eq!(add_err.code, "DestinationAccessDenied");

    // Binding inventory after leaving fleet must also fail
    let inv_leave_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("GetInventoryFromId after leaving fleet must fail");
    assert_eq!(inv_leave_err.code, "InventoryNotAccessible");
}
