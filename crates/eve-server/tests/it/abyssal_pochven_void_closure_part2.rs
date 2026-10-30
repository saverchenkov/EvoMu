//! Integration tests for Warp Vector mechanics (Part 2).

#![forbid(unsafe_code)]

use crate::common;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYS_ID: u32 = 30_000_142; // Jita (Empire K-space)

#[tokio::test]
async fn test_warp_vector_mechanics_and_celestial_bookmark_targets() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_604_i64;
    let vector_beacon_id = 700_000_801_i64;
    let other_ship_id = 700_000_802_i64;
    let far_beacon_id = 700_000_803_i64;

    let mut sess = SessionState::new(886_003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = JITA_SYS_ID;
        ch.active_ship_id = ship_id;
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter frigate
                owner_id: char_id,
                location_id: JITA_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(JITA_SYS_ID);
    sess.ship_id = Some(ship_id);

    // 1. Docked check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    sess.station_id = Some(60_003_760);
    let docked_err = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("UseVector while docked must fail");
    assert_eq!(docked_err.code, "ShipDocked");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    sess.station_id = None;

    // 2. Spawn ship ball in Jita
    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS_ID);
    let _ = jita_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            radius: 30.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 300.0,
            inertia: 1.0,
        })
        .await;

    // 3. Own ship target rejected
    let self_err = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("Targeting own ship must fail");
    assert_eq!(self_err.code, "InvalidArgument");

    // 4. Warp scrambled rejected
    jita_handle.try_add_mobile_warp_disruptor(999_001, DVec3::ZERO, 20_000.0);
    let scram_err = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(123)], &kw, None)
        .await
        .expect_err("UseVector while scrambled must fail");
    assert_eq!(scram_err.code, "WarpScrambled");
    jita_handle.try_remove_warp_bubble(999_001);

    // 5. Non-inventory / space ball IDs (missing ID, ship ball, container ball) rejected with InvalidVectorItem
    let missing_err = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(999_999_999)], &kw, None)
        .await
        .expect_err("Non-existent vector item must fail");
    assert_eq!(missing_err.code, "InvalidVectorItem");

    let _ = jita_handle
        .spawn_ball(Ball {
            id: other_ship_id,
            type_id: 587, // Rifter (category 6)
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            radius: 30.0,
            mass: 1_000_000.0,
            mode: MovementMode::Stop,
            speed_fraction: 0.0,
            max_velocity: 300.0,
            inertia: 1.0,
        })
        .await;
    let ship_err = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(other_ship_id)], &kw, None)
        .await
        .expect_err("Space ship ball must fail with InvalidVectorItem");
    assert_eq!(ship_err.code, "InvalidVectorItem");

    let _ = jita_handle
        .spawn_ball(Ball {
            id: far_beacon_id,
            type_id: 23,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            radius: 50.0,
            mass: 1_000.0,
            mode: MovementMode::Rigid,
            speed_fraction: 0.0,
            max_velocity: 0.0,
            inertia: 1.0,
        })
        .await;
    let ball_err = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(far_beacon_id)], &kw, None)
        .await
        .expect_err("Non-inventory space ball must fail with InvalidVectorItem");
    assert_eq!(ball_err.code, "InvalidVectorItem");

    // 6. Valid Warp Vector Data item (77_825) in ship cargo initiates authentic warp relative to star
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            vector_beacon_id,
            eve_db::InventoryItemRecord {
                item_id: vector_beacon_id,
                type_id: 77_825, // Warp Vector Data (group 4599)
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let _ = ctx.drain_notifications_for_char(char_id);
    let vec_res = ctx
        .dispatch_rpc(&mut sess, "warpVectorMgr", None, "UseVector", &[PyRep::Integer(vector_beacon_id)], &kw, None)
        .await
        .expect("UseVector with cargo Warp Vector Data");
    assert_eq!(vec_res.dict_get("vectorItemID"), Some(&PyRep::Integer(vector_beacon_id)));
    assert_eq!(vec_res.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));

    // Verify ship ball entered authentic warp movement mode without instant teleport
    let (_, ball_after_vec) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(
        ball_after_vec.position,
        DVec3::ZERO,
        "Ship must not instantly teleport to destination"
    );
    match ball_after_vec.mode {
        MovementMode::Warp { destination, .. } => {
            assert!(
                destination.length() >= 150_000.0,
                "Ship warp destination must be at least 150 km from ship"
            );
        }
        ref other => panic!("Ship ball must be in Warp movement mode, got: {other:?}"),
    }

    // Verify OnWarpStarted notification was emitted with 2 args [ship_id, type_id]
    let notifs = ctx.drain_notifications_for_char(char_id);
    let warp_notif = notifs
        .iter()
        .find(|n| n.event == "OnWarpStarted")
        .expect("OnWarpStarted notification must be emitted");
    assert_eq!(
        warp_notif.args,
        vec![PyRep::Integer(ship_id), PyRep::Integer(587)]
    );
}
