//! Comprehensive edge, error, and happy-path unit tests for `structure_lifecycle_cargo_fuel`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::BoundObjectEntry;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::{IndustryJobRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jettison_structure_fuel_guards_explicit_items_bound_fallback_and_low_power_notify() {
    let mut ctx = test_ctx();
    seed_astrahus_type(&mut ctx, 35832, false);
    clone_type_with(
        &mut ctx.sde,
        35832,
        35894,
        1321,
        life::CATEGORY_STRUCTURE,
        100.0,
        0.0,
        &[
            (life::ATTR_SERVICE_FUEL_PER_HOUR, 5.0),
            (life::ATTR_SERVICE_FULL_POWER_HP_MULT, 1.0),
        ],
    );
    let sys_id = 30_000_142_u32;
    let char_id = 91_800_001_i64;
    let corp_id = 98_800_001_i64;
    let ship_id = 1_088_000_001_i64;
    let sid = 1_088_000_010_i64;
    let dead_sid = 1_088_000_011_i64;
    let fuel_tid = 4247_u32;

    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Fuel Astra".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(),
                fuel_blocks: 30,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            dead_sid,
            UpwellStructureRecord {
                structure_id: dead_sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Dead Astra".into(),
                position: [1_001_000.0, 0.0, 0.0],
                state: life::STATE_DESTROYED.into(),
                fuel_blocks: 10,
                ..Default::default()
            },
        );
        // Online service module so dropping fuel_blocks to 0 triggers power_before != power_after -> OnStructureServiceChanged!
        snap.items.insert(
            88_001,
            InventoryItemRecord {
                item_id: 88_001,
                type_id: 35894,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(88_001, true);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(sid);
        }
        snap.items.insert(
            88_002,
            InventoryItemRecord {
                item_id: 88_002,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::StructureFuel,
                quantity: 30,
                singleton: false,
                ..Default::default()
            },
        );
        // Non-fuel item in fuel bay & wrong-owner fuel item
        snap.items.insert(
            88_003,
            InventoryItemRecord {
                item_id: 88_003,
                type_id: 34, // Tritanium
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::StructureFuel,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            88_004,
            InventoryItemRecord {
                item_id: 88_004,
                type_id: fuel_tid,
                owner_id: 98_999_999,
                location_id: sid,
                flag: InventoryFlag::StructureFuel,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            88_005,
            InventoryItemRecord {
                item_id: 88_005,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 1. is_pilot_docked_or_controlling_structure branches
    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(sid);
        let mut ext = load_ext_state(snap);
        // station_id overrides DB character.structure_id -> (false, false)
        sess.station_id = Some(60_003_760);
        assert_eq!(
            structure_lifecycle_cargo_fuel::is_pilot_docked_or_controlling_structure(snap, &ext, &sess, char_id, sid),
            (false, false)
        );
        // without station_id & session.structure_id=None, DB character.structure_id -> (true, false)
        sess.station_id = None;
        assert_eq!(
            structure_lifecycle_cargo_fuel::is_pilot_docked_or_controlling_structure(snap, &ext, &sess, char_id, sid),
            (true, false)
        );
        // active_pilot_by_structure -> controlling_here = true
        snap.characters.get_mut(&char_id).unwrap().structure_id = None;
        ext.active_pilot_by_structure.insert(sid, char_id);
        assert_eq!(
            structure_lifecycle_cargo_fuel::is_pilot_docked_or_controlling_structure(snap, &ext, &sess, char_id, sid),
            (false, true)
        );
        save_ext_state(snap, &ext);
    });

    // 2. MissingStructureId
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &[]).await.unwrap_err().code,
        "MissingStructureId"
    );

    // 3. StructureDestroyed
    sess.structure_id = Some(dead_sid);
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(dead_sid)])
            .await
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );

    // 4. InsufficientRoles (strip member_roles)
    sess.structure_id = Some(sid);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.clear();
    });
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(sid)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRoles"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
    });

    // 5. Explicit item error paths: ItemNotFound, ItemNotInFuelBay, ItemOwnershipViolation, ItemNotFuel
    for (iid, code) in [
        (9_999_999, "ItemNotFound"),
        (88_005, "ItemNotInFuelBay"),
        (88_004, "ItemOwnershipViolation"),
        (88_003, "ItemNotFuel"),
    ] {
        assert_eq!(
            handle_jettison_structure_fuel(
                &ctx,
                &mut sess,
                &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(iid)])],
            )
            .await
            .unwrap_err()
            .code,
            code
        );
    }

    // 6. Quantity / type errors: InvalidArgument, ItemNotFuel, InsufficientStructureFuel
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(i64::from(fuel_tid)), PyRep::Integer(-5)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(34), PyRep::Integer(5)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFuel"
    );
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InsufficientStructureFuel"
    );
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(999)])
            .await
            .unwrap_err()
            .code,
        "InsufficientStructureFuel"
    );

    // 7. Bound-object fallback with 2-arg [sid_omitted -> via bound_objects, qty] or 1-arg single item_id -> drains remaining fuel & emits OnStructureServiceChanged
    ctx.bound_objects.write().unwrap().insert(
        9_001,
        BoundObjectEntry {
            bound_id: 9_001,
            session_id: sess.session_id,
            user_id: char_id as u64,
            character_id: char_id,
            service: "structure".into(),
            object_id: sid,
            oid_str: format!("N=1:{sid}"),
            ref_id: 9_001,
        },
    );
    sess.structure_id = None; // Controlling via active_pilot_by_structure + bound_objects resolves sid!
    ctx.db.provision_gm_account("fuel_mgr", "pw", char_id, "FuelMgr");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(sid);
            ch.active_ship_id = ship_id;
        }
    });
    let char_uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(char_uid, sess.session_id, Some(char_id), true, ctx.now_unix())
        .expect("acquire fuel_mgr online lock");
    let _ = ctx.drain_notifications_for_char(char_id);
    // Single-int item_id [88_002] resolves sid from snap.items[&88_002].location_id & jettisons all 30 blocks!
    let jet_res = handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(88_002)])
        .await
        .expect("single-item-id jettison");
    assert_eq!(
        jet_res.dict_get("remainingFuelBlocks").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnStructureServiceChanged"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structure_deliver_and_cargo_dropoff_all_error_and_hold_branches() {
    let mut ctx = test_ctx();
    seed_astrahus_type(&mut ctx, 35832, false);
    clone_type_with(
        &mut ctx.sde,
        35832,
        35894,
        1321,
        life::CATEGORY_STRUCTURE,
        100.0,
        0.0,
        &[
            (life::ATTR_SERVICE_FUEL_PER_HOUR, 5.0),
            (life::ATTR_SERVICE_FULL_POWER_HP_MULT, 1.0),
        ],
    );
    // FLEX structure (Ansiblex Jump Bridge, group 1408)
    clone_type_with(
        &mut ctx.sde,
        35832,
        35841,
        1408,
        life::CATEGORY_STRUCTURE,
        80_000.0,
        0.0,
        &[],
    );
    let sys_id = 30_000_142_u32;
    let char_id = 91_800_010_i64;
    let recip_id = 91_800_011_i64;
    let corp_id = 98_800_010_i64;
    let ship_id = 1_088_000_101_i64;
    let saved_ship_id = 1_088_000_102_i64;
    let sid = 1_088_000_110_i64;
    let flex_sid = 1_088_000_111_i64;
    let reinf_sid = 1_088_000_112_i64;

    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id, true);
    seed_char_corp_ship(&ctx, recip_id, corp_id, sys_id, 1_088_000_199, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = saved_ship_id;
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Delivery Astra".into(),
                position: [2_000_000_000.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            flex_sid,
            UpwellStructureRecord {
                structure_id: flex_sid,
                type_id: 35841,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Ansiblex".into(),
                position: [2_000_000_000.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            reinf_sid,
            UpwellStructureRecord {
                structure_id: reinf_sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Reinforced Astra".into(),
                position: [2_000_000_000.0, 0.0, 0.0],
                state: life::STATE_HULL_REINFORCED.into(),
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        // Saved ship, missing item, wrong-location item, locked item
        snap.items.insert(
            saved_ship_id,
            InventoryItemRecord {
                item_id: saved_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            88_101,
            InventoryItemRecord {
                item_id: 88_101,
                type_id: 34,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Deliveries, // not Hangar!
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            88_102,
            InventoryItemRecord {
                item_id: 88_102,
                type_id: 34,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            55_001,
            IndustryJobRecord {
                job_id: 55_001,
                installer_id: char_id,
                facility_id: sid,
                blueprint_item_id: 88_102,
                blueprint_type_id: 34,
                activity_id: 1,
                runs: 1,
                product_type_id: 34,
                status: "Active".into(),
                ..Default::default()
            },
        );
        // SpecializedGasHold item & wrong-flag ship item
        snap.items.insert(
            88_103,
            InventoryItemRecord {
                item_id: 88_103,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedGasHold,
                quantity: 25,
                ..Default::default()
            },
        );
        snap.items.insert(
            88_104,
            InventoryItemRecord {
                item_id: 88_104,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    // 1. handle_structure_deliver error paths
    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 0);
    assert_eq!(
        handle_structure_deliver(&ctx, &mut sess, &[]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_structure_deliver(&ctx, &mut sess, &[PyRep::Integer(sid)]).unwrap_err().code,
        "MissingItemId"
    );
    assert_eq!(
        handle_structure_deliver(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(88_102)])
            .unwrap_err()
            .code,
        "MissingRecipientId"
    );
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(88_102), PyRep::Integer(recip_id)],
        )
        .unwrap_err()
        .code,
        "NotDockedInStructure"
    );
    sess.structure_id = Some(sid);
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(saved_ship_id), PyRep::Integer(recip_id)],
        )
        .unwrap_err()
        .code,
        "CannotDeliverActiveShip"
    );
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(88_102), PyRep::Integer(999_999)],
        )
        .unwrap_err()
        .code,
        "RecipientNotFound"
    );
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(999_888), PyRep::Integer(recip_id)],
        )
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(88_101), PyRep::Integer(recip_id)],
        )
        .unwrap_err()
        .code,
        "ItemNotInStructureHangar"
    );
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut sess,
            &[PyRep::Integer(sid), PyRep::Integer(88_102), PyRep::Integer(recip_id)],
        )
        .unwrap_err()
        .code,
        "ItemLocked"
    );

    // 2. handle_cargo_dropoff error paths & single-integer itemID happy path
    sess.structure_id = None;
    sess.ship_id = None;
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_103), PyRep::Integer(ship_id), PyRep::Integer(sid)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut sess, &[]).await.unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::List(vec![]), PyRep::Integer(ship_id), PyRep::Integer(sid)],
        )
        .await
        .unwrap_err()
        .code,
        "NoItemsSpecified"
    );
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_103), PyRep::Integer(ship_id), PyRep::Integer(999_777)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureNotFound"
    );
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_103), PyRep::Integer(ship_id), PyRep::Integer(flex_sid)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureNotDockable"
    );
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_103), PyRep::Integer(ship_id), PyRep::Integer(reinf_sid)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureNotOperational"
    );
    // ShipNotInSpace before ball spawned
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_103), PyRep::Integer(ship_id), PyRep::Integer(sid)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball within range, test ItemNotInShipCargo & ItemLocked, then single-int happy path
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(2_000_005_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_104), PyRep::Integer(ship_id), PyRep::Integer(sid)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotInShipCargo"
    );
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut sess,
            &[PyRep::Integer(88_103), PyRep::Integer(ship_id), PyRep::Integer(sid)],
        )
        .await
        .expect("single-int gas hold dropoff"),
        PyRep::List(vec![PyRep::Integer(88_103)])
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&88_103].location_id, sid);
        assert_eq!(snap.items[&88_103].flag, InventoryFlag::Deliveries);
    });
}
