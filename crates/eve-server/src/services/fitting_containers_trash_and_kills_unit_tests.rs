//! Semantic unit tests split from `fitting_ships_mastery_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn launch_from_container_and_trash_items_and_kill_counter_edge_cases() {
    let ctx = make_ctx();
    let char_id = 96_500_010_i64;
    let sys_id = 30_000_142_u32;
    let carrier_id = 96_500_201_i64;
    let stored_ship_id = 96_500_202_i64;
    let jetcan_id = 96_500_203_i64;
    let jetcan_loot_id = 96_500_204_i64;
    let fitted_mod_id = 96_500_205_i64;
    let locked_item_id = 96_500_206_i64;
    let safety_item_id = 96_500_207_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 10;
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = carrier_id;
        snap.characters.insert(char_id, ch);

        for rec in [
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
            InventoryItemRecord {
                item_id: stored_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::ShipHangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
            InventoryItemRecord {
                item_id: jetcan_id,
                type_id: 23, // Cargo Container (jetcan)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
            InventoryItemRecord {
                item_id: jetcan_loot_id,
                type_id: 34,
                owner_id: char_id,
                location_id: jetcan_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
            InventoryItemRecord {
                item_id: fitted_mod_id,
                type_id: 484,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
            InventoryItemRecord {
                item_id: locked_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::Locked,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
            InventoryItemRecord {
                item_id: safety_item_id,
                type_id: 34,
                owner_id: char_id + 1, // owned by another character -> DeleteFailed
                location_id: carrier_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        ] {
            snap.items.insert(rec.item_id, rec);
        }
        snap.item_positions.insert(carrier_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(jetcan_id, [500.0, 0.0, 0.0]);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: carrier_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;

    let mut sess = SessionState::new(802);
    sess.user_id = Some(10);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(carrier_id);
    sess.role = 0; // non-GM

    // 1. LaunchFromContainer launches stored_ship_id into space at +300m
    let launched = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "LaunchFromContainer",
        &[PyRep::Integer(carrier_id), PyRep::Integer(stored_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        launched.dict_get("launchedItemID").and_then(PyRep::as_i64),
        Some(stored_ship_id)
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&stored_ship_id].location_id, sys_id as i64);
    });

    // 2. TrashItems guards: empty list, active ship, fitted module, locked item, asset safety item
    for (bad_args, expected_code) in [
        (vec![], "DeleteFailed"),
        (vec![PyRep::Integer(carrier_id)], "DeleteFailed"),
        (vec![PyRep::Integer(fitted_mod_id)], "CannotTrashFittedItem"),
        (vec![PyRep::Integer(locked_item_id)], "ItemLocked"),
        (vec![PyRep::Integer(safety_item_id)], "DeleteFailed"),
    ] {
        let err = handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "TrashItems",
            &bad_args,
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_code);
    }

    // 3. Trashing the only item inside an in-space jetcan also despawns the empty jetcan
    let trash_jetcan_loot = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "TrashItems",
        &[PyRep::Integer(jetcan_loot_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        trash_jetcan_loot.dict_get("deleted").and_then(PyRep::as_bool),
        Some(true)
    );
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&jetcan_loot_id));
        assert!(!s.items.contains_key(&jetcan_id));
    });

    // 4. SetShipKillCounter requires GM role; GetItemKillCountPlayer reads custom_names fallback
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "shipKillCounter",
            "SetShipKillCounter",
            &[PyRep::Integer(carrier_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.item_custom_names
            .insert(-8_900_000 - carrier_id, "12".into());
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "shipKillCounter",
            "GetItemKillCountPlayer",
            &[PyRep::Integer(carrier_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Integer(12)
    );

    // 5. Mastery invalid shipTypeID errors and SafeLogoff / AbortSafeLogoff
    let err1 = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "GetCurrCharMasteryLevel",
        &[PyRep::Integer(-1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    let err2 = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "GetCertificatesForShipByMasteryLevel",
        &[PyRep::Integer(-1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    let err3 = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "GetSkillsRequiredForMasteryLevel",
        &[PyRep::Integer(-1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err1.code, "InvalidShipTypeID");
    assert_eq!(err2.code, "InvalidShipTypeID");
    assert_eq!(err3.code, "InvalidShipTypeID");

    let _ = handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "SafeLogoff", &[], &kw)
        .await
        .unwrap();
    let _ = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "AbortSafeLogoff",
        &[],
        &kw,
    )
    .await
    .unwrap();
}
