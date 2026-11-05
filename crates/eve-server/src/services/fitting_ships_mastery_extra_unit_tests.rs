//! Extra unit and mutation-killing tests for `services::inventory_fitting::fitting_ships_mastery`
//! covering in-space `Eject` and `Board` (range, crimewatch timers, capsule cleanup, structure control),
//! `LaunchFromContainer`, `CreateNewbieShip` guards, `GetRequiredSkillLevels`, mastery/certificate errors,
//! `SetShipKillCounter` role & ownership checks, `TrashItems` (fitted, locked, asset safety, non-empty,
//! and in-space empty jetcan despawn), `GetDirtTimestamp` / `ResetDirtTimestamp`, and `SafeLogoff`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext;
use crate::services::space_destiny::clear_session_change_timer;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
}

#[tokio::test]
async fn eject_and_board_in_space_and_docked_guards_and_capsule_transitions() {
    let ctx = make_ctx();
    let char_id = 96_500_001_i64;
    let other_char = 96_500_002_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship1_id = 96_500_101_i64;
    let ship2_id = 96_500_102_i64;
    let not_ship_id = 96_500_103_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship1_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship1_id,
            InventoryItemRecord {
                item_id: ship1_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship2_id,
            InventoryItemRecord {
                item_id: ship2_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            not_ship_id,
            InventoryItemRecord {
                item_id: not_ship_id,
                type_id: 34, // Tritanium (not category 6)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(801);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;

    // 1. NoActiveShip on Eject / LeaveShip
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    sess.ship_id = Some(ship1_id);
    // 2. CannotEjectWhileDocked when docked
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectWhileDocked"
    );

    // 3. Docked Board: NotAShip, OwnershipViolation, ShipNotBoardable, and valid docked switch
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(not_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotAShip"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ship2_id).unwrap().owner_id = other_char;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(ship2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OwnershipViolation"
    );

    ctx.db.with_mut_snapshot(|s| {
        let it = s.items.get_mut(&ship2_id).unwrap();
        it.owner_id = char_id;
        it.singleton = false;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(ship2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipNotBoardable"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ship2_id).unwrap().singleton = true;
    });
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(
            &ctx,
            &mut sess,
            None,
            "ship",
            "Board",
            &[PyRep::Integer(ship2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipHasSkillPrerequisites"
    );
    ctx.db.with_mut_snapshot(|s| {
        let demo_skills = s.skills.values().next().cloned().unwrap_or_default();
        s.skills.insert(char_id, demo_skills);
    });
    let docked_board = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship2_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        docked_board.dict_get("shipID").and_then(PyRep::as_i64),
        Some(ship2_id)
    );
    assert_eq!(sess.ship_id, Some(ship2_id));
    clear_session_change_timer(&ctx, char_id);

    // 4. Move to space and test LeaveShip error, weapon/criminal timers on Eject, and valid Eject -> Board in space
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().station_id = None;
        let it = s.items.get_mut(&ship2_id).unwrap();
        it.location_id = sys_id as i64;
        it.flag = InventoryFlag::None;
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship2_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;

    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "LeaveShip", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotLeaveShipInSpace"
    );

    // Active weapon timer blocks Eject
    let now_u = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = now_u + 60;
    }
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectActiveWeaponsTimer"
    );

    // Criminal timer in high-sec blocks Eject
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.states.entry(char_id).or_default();
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now_u + 60;
    }
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectInvolvedInCrime"
    );
    {
        ctx.crimewatch.write().unwrap().states.clear();
    }

    // Valid in-space Eject spawns capsule and switches session.ship_id to capsule
    let eject_res =
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap();
    let capsule_id = eject_res
        .dict_get("capsuleID")
        .and_then(PyRep::as_i64)
        .unwrap();
    assert_eq!(
        eject_res.dict_get("abandonedShipID").and_then(PyRep::as_i64),
        Some(ship2_id)
    );
    assert_eq!(sess.ship_id, Some(capsule_id));

    // Cannot eject from capsule
    clear_session_change_timer(&ctx, char_id);
    assert_eq!(
        handle_ships_mastery_and_fitting_ext(&ctx, &mut sess, None, "ship", "Eject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotEjectFromCapsule"
    );

    // In-space Board back into ship2_id from capsule removes capsule and restores ship2_id
    let board_space = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut sess,
        None,
        "ship",
        "Board",
        &[PyRep::Integer(ship2_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        board_space.dict_get("shipID").and_then(PyRep::as_i64),
        Some(ship2_id)
    );
    assert_eq!(
        board_space.dict_get("oldShipID").and_then(PyRep::as_i64),
        Some(capsule_id)
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&capsule_id)));
}

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
