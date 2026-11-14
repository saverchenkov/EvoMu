//! Semantic unit tests split from `structure_lifecycle_rpc_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::structure_lifecycle_rpc_extra_unit_tests::*;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_lifecycle_rpc::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test(flavor = "current_thread")]
async fn fitting_rigs_weapons_charges_quantum_core_and_fuel_online_guards() {
    let mut ctx = test_ctx();
    let st_tid = 35832_u32;
    let rig_ok_tid = 918_011_u32;
    let rig_bad_tid = 918_012_u32;
    let weap_tid = 918_013_u32;
    let ammo_tid = 918_014_u32;
    let svc_tid = 918_015_u32;
    clone_type_with(
        &mut ctx.sde,
        rig_ok_tid,
        1564,
        66,
        100.0,
        0.0,
        &[(life::ATTR_RIG_SIZE, 2.0), (1298, 1657.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        rig_bad_tid,
        1564,
        66,
        100.0,
        0.0,
        &[(life::ATTR_RIG_SIZE, 3.0), (1298, 1657.0)],
    );
    clone_type_with(&mut ctx.sde, weap_tid, 1329, 66, 200.0, 50.0, &[(1298, 1657.0)]);
    clone_type_with(&mut ctx.sde, ammo_tid, 88, 8, 1.0, 0.0, &[]);
    clone_type_with(
        &mut ctx.sde,
        svc_tid,
        1415,
        66,
        500.0,
        0.0,
        &[(life::ATTR_SERVICE_FUEL_ONLINE, 100.0)],
    );

    let sys_id = 30000142_u32;
    let char_id = 918_301_i64;
    let corp_id = 98_918_301_i64;
    let ship_id = 918_401_i64;
    let sid = 10_000_918_001_i64;
    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id);
    seed_structure(&ctx, sid, st_tid, corp_id, sys_id, life::STATE_SHIELD_VULNERABLE, 20);

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, qty) in [
            (71_001, rig_bad_tid, 1),
            (71_002, rig_ok_tid, 1),
            (71_003, weap_tid, 2),
            (71_004, ammo_tid, 10),
            (71_005, svc_tid, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: sid,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: qty == 1,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 1);
    sess.structure_id = Some(sid);

    // 1. RigSizeMismatch
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, 71_001, sid, InventoryFlag::RigSlot0, None)
            .unwrap_err()
            .code,
        "RigSizeMismatch"
    );

    // 2. Fit rig, fail to move between rig slots (`CannotMoveFittedRig`), then unfit rig (destroys item!)
    assert!(handle_structure_fit_move(&ctx, &mut sess, 71_002, sid, InventoryFlag::RigSlot0, None).is_ok());
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, 71_002, sid, InventoryFlag::RigSlot1, None)
            .unwrap_err()
            .code,
        "CannotMoveFittedRig"
    );
    let unfit_rig = handle_structure_fit_move(&ctx, &mut sess, 71_002, sid, InventoryFlag::Hangar, None)
        .expect("unfit rig destroys it");
    let PyRep::KeyVal(kv_rig) = unfit_rig else { panic!("expected KeyVal") };
    assert_eq!(kv_rig.get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&71_002)));

    // 3. Ammo into empty weapon slot -> NoWeaponFitted; fit weapon from stack of 2 -> splits stack; load ammo -> loaded
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, 71_004, sid, InventoryFlag::HiSlot0, None)
            .unwrap_err()
            .code,
        "NoWeaponFitted"
    );
    assert!(handle_structure_fit_move(&ctx, &mut sess, 71_003, sid, InventoryFlag::HiSlot0, Some(true)).is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&71_003].quantity), 1);
    let load_ammo = handle_structure_fit_move(&ctx, &mut sess, 71_004, sid, InventoryFlag::HiSlot0, None)
        .expect("load ammo into fitted weapon");
    let PyRep::KeyVal(kv_ammo) = load_ammo else { panic!("expected KeyVal") };
    assert_eq!(kv_ammo.get("online"), Some(&PyRep::Bool(true)));

    // 4. Service module online checks InsufficientStructureFuelToOnline and QuantumCoreRequired
    assert!(handle_structure_fit_move(&ctx, &mut sess, 71_005, sid, InventoryFlag::ServiceSlot0, Some(false)).is_ok());
    assert_eq!(
        handle_structure_module_online(&ctx, &mut sess, sid, 71_005, true)
            .unwrap_err()
            .code,
        "InsufficientStructureFuelToOnline"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&sid).unwrap();
        st.fuel_blocks = 200;
        st.quantum_core_installed = false;
    });
    assert_eq!(
        handle_structure_module_online(&ctx, &mut sess, sid, 71_005, true)
            .unwrap_err()
            .code,
        "QuantumCoreRequired"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn jettison_explicit_ids_deliver_guards_and_dropoff_distance_and_specialized_holds() {
    let mut ctx = test_ctx();
    let st_tid = 918_020_u32;
    let fuel_tid = 4247_u32;
    let can_tid = 918_021_u32;
    seed_astrahus_type(&mut ctx, st_tid);
    clone_type_with(&mut ctx.sde, fuel_tid, 1136, 14, 5.0, 0.0, &[]);
    clone_type_with(&mut ctx.sde, can_tid, 12, 2, 10.0, 100.0, &[]);

    let sys_id = 30000142_u32;
    let char_id = 918_501_i64;
    let recip_id = 918_502_i64;
    let corp_id = 98_918_501_i64;
    let ship_id = 918_601_i64;
    let sid = 10_000_918_002_i64;
    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id);
    seed_char_corp_ship(&ctx, recip_id, corp_id, sys_id, ship_id + 1);
    seed_structure(&ctx, sid, st_tid, corp_id, sys_id, life::STATE_SHIELD_VULNERABLE, 60);

    ctx.db.with_mut_snapshot(|snap| {
        // Fuel stack in StructureFuel bay for explicit list jettison
        snap.items.insert(
            72_001,
            InventoryItemRecord {
                item_id: 72_001,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::StructureFuel,
                quantity: 15,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Non-empty container in Hangar -> CannotDeliverNonEmptyContainer
        snap.items.insert(
            72_002,
            InventoryItemRecord {
                item_id: 72_002,
                type_id: can_tid,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            72_003,
            InventoryItemRecord {
                item_id: 72_003,
                type_id: fuel_tid,
                owner_id: char_id,
                location_id: 72_002,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Ore in SpecializedOreHold on ship for CargoDropoff
        snap.items.insert(
            72_004,
            InventoryItemRecord {
                item_id: 72_004,
                type_id: fuel_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 1);
    sess.structure_id = Some(sid);

    // 1. Explicit list jettison `[72001]` from StructureFuel bay
    let jet = handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(72_001)])]).await.expect("explicit fuel item jettison");
    let PyRep::KeyVal(kv_jet) = jet else { panic!("expected KeyVal") };
    assert_eq!(kv_jet.get("remainingFuelBlocks"), Some(&PyRep::Integer(45)));
    assert_eq!(kv_jet.get("jettisonedItemIDs"), Some(&PyRep::List(vec![PyRep::Integer(72_001)])));

    // 2. Deliver active ship -> CannotDeliverActiveShip; non-empty container -> CannotDeliverNonEmptyContainer
    assert_eq!(handle_structure_deliver(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(ship_id), PyRep::Integer(recip_id)]).unwrap_err().code, "CannotDeliverActiveShip");
    assert_eq!(handle_structure_deliver(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(72_002), PyRep::Integer(recip_id)]).unwrap_err().code, "CannotDeliverNonEmptyContainer");

    // 3. Cargo dropoff: docked rejected (`MustBeInSpace`), out of range rejected, within range in SpecializedOreHold succeeds
    let drop_args = [
        PyRep::List(vec![PyRep::Integer(72_004)]),
        PyRep::Integer(ship_id),
        PyRep::Integer(sid),
    ];
    assert_eq!(handle_cargo_dropoff(&ctx, &mut sess, &drop_args).await.unwrap_err().code, "MustBeInSpace");
    sess.structure_id = None;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(2_000_050_000.0, 0.0, 0.0),
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
    assert_eq!(handle_cargo_dropoff(&ctx, &mut sess, &drop_args).await.unwrap_err().code, "OutsideCargoDropoffRange");

    sys_h.set_ball_position(ship_id, DVec3::new(2_000_005_000.0, 0.0, 0.0)).await;
    assert!(handle_cargo_dropoff(&ctx, &mut sess, &drop_args).await.is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&72_004].flag), InventoryFlag::Deliveries);
}
