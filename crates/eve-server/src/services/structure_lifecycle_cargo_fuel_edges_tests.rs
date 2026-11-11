//! Behavioral edge-case unit tests for `services::structure_lifecycle_cargo_fuel`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::BoundObjectEntry;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_upwell_ops::StructureExtendedState;
use eve_db::{
    corp_roles, ContractRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::sync::Arc;

const FUEL_BLOCK_TYPE: u32 = 4247;
const FUEL_GROUP_ID: u32 = 1136;
const STRONTIUM_TYPE: u32 = 16_273;
const CITADEL_TYPE: u32 = 35_832;
const FLEX_TYPE: u32 = 35_841;
const SERVICE_MOD_TYPE: u32 = 35_892;

fn make_sde_type(type_id: u32, group_id: u32, category_id: u32, radius: f64, attrs: Vec<(u16, f64)>) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type {type_id}"),
        mass: 1_000_000.0,
        volume: 100.0,
        capacity: 10_000.0,
        radius,
        base_price: 1000.0,
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
        default_attributes: attrs.into_iter().collect(),
    }
}

fn test_ctx() -> ServerContext {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        FUEL_BLOCK_TYPE,
        make_sde_type(FUEL_BLOCK_TYPE, FUEL_GROUP_ID, 4, 1.0, vec![]),
    );
    sde.types.insert(
        STRONTIUM_TYPE,
        make_sde_type(STRONTIUM_TYPE, 423, 4, 1.0, vec![]),
    );
    sde.types.insert(
        CITADEL_TYPE,
        make_sde_type(
            CITADEL_TYPE,
            1657,
            life::CATEGORY_STRUCTURE,
            5_000.0,
            vec![
                (life::ATTR_RIG_SIZE, 2.0),
                (life::ATTR_SHIELD_CAPACITY, 100_000.0),
                (life::ATTR_STRUCTURE_FULL_POWER_HP_MULT, 1.5),
            ],
        ),
    );
    sde.types.insert(
        FLEX_TYPE,
        make_sde_type(
            FLEX_TYPE,
            2017,
            life::CATEGORY_STRUCTURE,
            2_500.0,
            vec![(life::ATTR_SHIELD_CAPACITY, 50_000.0)],
        ),
    );
    sde.types.insert(
        SERVICE_MOD_TYPE,
        make_sde_type(
            SERVICE_MOD_TYPE,
            1321,
            66,
            1.0,
            vec![
                (life::ATTR_SERVICE_FUEL_GROUP, f64::from(FUEL_GROUP_ID)),
                (life::ATTR_SERVICE_FUEL_PER_HOUR, 10.0),
                (life::ATTR_SERVICE_FUEL_ONLINE, 100.0),
                (life::ATTR_SERVICE_FULL_POWER_HP_MULT, 1.2),
            ],
        ),
    );
    ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(user_id: u64, char_id: i64, corp_id: i64, role: u64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(10);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.role = role;
    s.corp_role = corp_role;
    s
}

#[test]
fn is_pilot_docked_or_controlling_structure_checks_station_session_snapshot_and_control() {
    let ctx = test_ctx();
    let char_id = ctx.db.with_snapshot(|s| *s.characters.keys().next().unwrap());
    let st_id = 1_030_000_000_001_i64;
    let mut ext = StructureExtendedState::default();

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(st_id);
    });

    ctx.db.with_snapshot(|snap| {
        // Docked in station overrides structure check
        let mut s_station = make_session(1, char_id, 1_000_009, 0, 0);
        s_station.station_id = Some(60_003_760);
        s_station.structure_id = Some(st_id);
        assert_eq!(
            is_pilot_docked_or_controlling_structure(snap, &ext, &s_station, char_id, st_id),
            (false, false)
        );

        // Session structure_id matches
        let mut s_struct = make_session(1, char_id, 1_000_009, 0, 0);
        s_struct.structure_id = Some(st_id);
        assert_eq!(
            is_pilot_docked_or_controlling_structure(snap, &ext, &s_struct, char_id, st_id),
            (true, false)
        );

        // Session structure_id None falls back to snap.characters[char_id].structure_id
        let s_none = make_session(1, char_id, 1_000_009, 0, 0);
        assert_eq!(
            is_pilot_docked_or_controlling_structure(snap, &ext, &s_none, char_id, st_id),
            (true, false)
        );

        // Controlling pilot detected from ext.active_pilot_by_structure
        ext.active_pilot_by_structure.insert(st_id, char_id);
        assert_eq!(
            is_pilot_docked_or_controlling_structure(snap, &ext, &s_station, char_id, st_id),
            (false, true)
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jettison_structure_fuel_covers_guards_explicit_items_quantity_modes_and_power_change() {
    let ctx = test_ctx();
    let (char_id, uid, corp_id) = ctx.db.with_snapshot(|s| {
        let (&cid, ch) = s.characters.iter().next().unwrap();
        (cid, ch.user_id, ch.corporation_id)
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 10, Some(char_id), false, ctx.now_unix());

    let st_id = 1_030_000_000_010_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(st_id);
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: CITADEL_TYPE,
                name: "Test Astrahus".into(),
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                state: life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 30,
                shield_hp: 180_000.0,
                ..Default::default()
            },
        );
        // Online service module so power_before == FullPower
        snap.items.insert(
            900_001,
            InventoryItemRecord {
                item_id: 900_001,
                type_id: SERVICE_MOD_TYPE,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(900_001, true);
        snap.module_active_states.insert(900_001);
        // Two fuel block stacks + one Strontium stack + one non-fuel item in Cargo
        snap.items.insert(
            900_010,
            InventoryItemRecord {
                item_id: 900_010,
                type_id: FUEL_BLOCK_TYPE,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_011,
            InventoryItemRecord {
                item_id: 900_011,
                type_id: FUEL_BLOCK_TYPE,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_012,
            InventoryItemRecord {
                item_id: 900_012,
                type_id: STRONTIUM_TYPE,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_013,
            InventoryItemRecord {
                item_id: 900_013,
                type_id: FUEL_BLOCK_TYPE,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_014,
            InventoryItemRecord {
                item_id: 900_014,
                type_id: FUEL_BLOCK_TYPE,
                owner_id: corp_id + 1,
                location_id: st_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_015,
            InventoryItemRecord {
                item_id: 900_015,
                type_id: FUEL_BLOCK_TYPE,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Unauthenticated character -> error
    let mut unauth = SessionState::new(9);
    assert!(handle_jettison_structure_fuel(&ctx, &mut unauth, &[])
        .await
        .is_err());

    // 2. Missing structure ID
    let mut s_nodock = make_session(uid, char_id, corp_id, 0, corp_roles::ROLE_DIRECTOR);
    let err_missing = handle_jettison_structure_fuel(&ctx, &mut s_nodock, &[])
        .await
        .unwrap_err();
    assert_eq!(err_missing.code, "MissingStructureId");

    // 3. Bound object resolution -> StructureNotFound when pointing at non-existent structure
    ctx.bound_objects.write().unwrap().insert(
        1,
        BoundObjectEntry {
            bound_id: 1,
            session_id: 10,
            user_id: uid,
            character_id: char_id,
            service: "structure".into(),
            object_id: 999_999_999,
            oid_str: "st".into(),
            ref_id: 1,
        },
    );
    let err_not_found = handle_jettison_structure_fuel(&ctx, &mut s_nodock, &[])
        .await
        .unwrap_err();
    assert_eq!(err_not_found.code, "StructureNotFound");
    ctx.bound_objects.write().unwrap().clear();

    // 4. Ownership violation (different corp)
    let mut s_other_corp = make_session(uid, char_id, corp_id + 99, 0, corp_roles::ROLE_DIRECTOR);
    s_other_corp.structure_id = Some(st_id);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().corporation_id = corp_id + 99);
    let err_owner = handle_jettison_structure_fuel(&ctx, &mut s_other_corp, &[PyRep::Integer(st_id)])
        .await
        .unwrap_err();
    assert_eq!(err_owner.code, "StructureOwnershipViolation");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().corporation_id = corp_id);

    // 5. NotDockedInStructure when station_id is set
    let mut s_station = make_session(uid, char_id, corp_id, 0, corp_roles::ROLE_DIRECTOR);
    s_station.station_id = Some(60_003_760);
    let err_nodock = handle_jettison_structure_fuel(&ctx, &mut s_station, &[PyRep::Integer(st_id)])
        .await
        .unwrap_err();
    assert_eq!(err_nodock.code, "NotDockedInStructure");

    // 6. InsufficientRoles
    let mut s_no_role = make_session(uid, char_id, corp_id, 0, 0);
    s_no_role.structure_id = Some(st_id);
    let err_role = handle_jettison_structure_fuel(&ctx, &mut s_no_role, &[PyRep::Integer(st_id)])
        .await
        .unwrap_err();
    assert_eq!(err_role.code, "InsufficientRoles");

    // 7. Explicit item errors: ItemNotFound, ItemNotInFuelBay, ItemOwnershipViolation, ItemNotFuel
    let mut s_dir = make_session(uid, char_id, corp_id, 0, corp_roles::ROLE_DIRECTOR);
    s_dir.structure_id = Some(st_id);
    for (bad_id, expected_code) in [
        (999_888, "ItemNotFound"),
        (900_015, "ItemNotInFuelBay"),
        (900_014, "ItemOwnershipViolation"),
        (900_013, "ItemNotFuel"),
    ] {
        let e = handle_jettison_structure_fuel(
            &ctx,
            &mut s_dir,
            &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(bad_id)])],
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, expected_code, "for item {bad_id}");
    }

    // 8. Quantity mode validation errors: InvalidArgument, ItemNotFuel, InsufficientStructureFuel
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx,
            &mut s_dir,
            &[PyRep::Integer(st_id), PyRep::Integer(-1), PyRep::Integer(1)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx,
            &mut s_dir,
            &[PyRep::Integer(st_id), PyRep::Integer(587), PyRep::Integer(1)]
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFuel"
    );
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx,
            &mut s_dir,
            &[PyRep::Integer(st_id), PyRep::Integer(i64::from(FUEL_BLOCK_TYPE)), PyRep::Integer(-5)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx,
            &mut s_dir,
            &[PyRep::Integer(st_id), PyRep::Integer(i64::from(STRONTIUM_TYPE)), PyRep::Integer(0)]
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientStructureFuel"
    );

    // 9. Explicit single item ID inference (Strontium 900_012 -> non-block does not reduce fuel_blocks)
    let rep_stront =
        handle_jettison_structure_fuel(&ctx, &mut s_dir, &[PyRep::Integer(900_012)])
            .await
            .unwrap();
    if let PyRep::KeyVal(entries) = rep_stront {
        assert_eq!(
            entries.iter().find(|(k, _)| *k == "remainingFuelBlocks").map(|(_, v)| v.clone()),
            Some(PyRep::Integer(30))
        );
    }

    // 10. Quantity mode (2 args): jettison 15 fuel blocks across stack 900_010 (10, removed) and 900_011 (5 left of 20 -> 15)
    let rep_partial = handle_jettison_structure_fuel(
        &ctx,
        &mut s_dir,
        &[PyRep::Integer(st_id), PyRep::Integer(15)],
    )
    .await
    .unwrap();
    if let PyRep::KeyVal(entries) = rep_partial {
        assert_eq!(
            entries.iter().find(|(k, _)| *k == "remainingFuelBlocks").map(|(_, v)| v.clone()),
            Some(PyRep::Integer(15))
        );
    }

    // 11. Advance last_fuel_tick_unix by 1 hour so depleting remaining fuel to 0 triggers power drop -> OnStructureServiceChanged
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle
            .entry(st_id)
            .or_default()
            .last_fuel_tick_unix = Some(ctx.now_unix().saturating_sub(3_601));
        save_ext_state(snap, &ext);
    });
    let rep_last = handle_jettison_structure_fuel(
        &ctx,
        &mut s_dir,
        &[PyRep::Tuple(vec![PyRep::Integer(900_011)])],
    )
    .await
    .unwrap();
    if let PyRep::KeyVal(entries) = rep_last {
        assert_eq!(
            entries.iter().find(|(k, _)| *k == "remainingFuelBlocks").map(|(_, v)| v.clone()),
            Some(PyRep::Integer(0))
        );
    }
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnStructureServiceChanged"));

    // 12. Destroyed structure error
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&st_id).unwrap().state = life::STATE_DESTROYED.into();
    });
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut s_dir, &[PyRep::Integer(st_id)])
            .await
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );
}

#[test]
fn structure_deliver_covers_all_validation_guards_and_delivery_transfer() {
    let ctx = test_ctx();
    let (char_id, recip_id, corp_id) = ctx.db.with_snapshot(|s| {
        let mut keys: Vec<i64> = s.characters.keys().copied().collect();
        keys.sort_unstable();
        let c1 = keys[0];
        let c2 = keys.get(1).copied().unwrap_or(c1);
        (c1, c2, s.characters[&c1].corporation_id)
    });
    let st_id = 1_030_000_000_020_i64;
    let mut s = make_session(1, char_id, corp_id, 0, 0);

    // Missing args
    assert_eq!(
        handle_structure_deliver(&ctx, &mut s, &[]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_structure_deliver(&ctx, &mut s, &[PyRep::Integer(st_id)])
            .unwrap_err()
            .code,
        "MissingItemId"
    );
    assert_eq!(
        handle_structure_deliver(&ctx, &mut s, &[PyRep::Integer(st_id), PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "MissingRecipientId"
    );

    // NotDockedInStructure
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(1), PyRep::Integer(recip_id)]
        )
        .unwrap_err()
        .code,
        "NotDockedInStructure"
    );

    s.structure_id = Some(st_id);
    s.ship_id = Some(555_001);
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_001), PyRep::Integer(recip_id)]
        )
        .unwrap_err()
        .code,
        "CannotDeliverActiveShip"
    );

    // Boarded ship in character snapshot (when session.ship_id is None)
    s.ship_id = None;
    let saved_active_ship = ctx.db.with_snapshot(|snap| snap.characters[&char_id].active_ship_id);
    if saved_active_ship > 0 {
        assert_eq!(
            handle_structure_deliver(
                &ctx,
                &mut s,
                &[PyRep::Integer(st_id), PyRep::Integer(saved_active_ship), PyRep::Integer(recip_id)]
            )
            .unwrap_err()
            .code,
            "CannotDeliverActiveShip"
        );
    }

    // RecipientNotFound
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_002), PyRep::Integer(999_999_999)]
        )
        .unwrap_err()
        .code,
        "RecipientNotFound"
    );

    // ItemNotFound
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_002), PyRep::Integer(recip_id)]
        )
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // ItemNotInStructureHangar, ItemLocked, CannotDeliverNonEmptyContainer, then Success
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            555_002,
            InventoryItemRecord {
                item_id: 555_002,
                type_id: 23,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Cargo, // wrong flag
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            555_003,
            InventoryItemRecord {
                item_id: 555_003,
                type_id: 34,
                owner_id: char_id,
                location_id: 555_002,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            7001,
            ContractRecord {
                contract_id: 7001,
                status: "Outstanding".into(),
                item_ids: vec![555_002],
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_002), PyRep::Integer(recip_id)]
        )
        .unwrap_err()
        .code,
        "ItemNotInStructureHangar"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&555_002).unwrap().flag = InventoryFlag::Hangar;
    });
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_002), PyRep::Integer(recip_id)]
        )
        .unwrap_err()
        .code,
        "ItemLocked"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.clear();
    });
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_002), PyRep::Integer(recip_id)]
        )
        .unwrap_err()
        .code,
        "CannotDeliverNonEmptyContainer"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&555_003);
    });
    assert_eq!(
        handle_structure_deliver(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Integer(555_002), PyRep::Integer(recip_id)]
        )
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&555_002];
        assert_eq!(it.owner_id, recip_id);
        assert_eq!(it.flag, InventoryFlag::Deliveries);
        assert_eq!(it.version, 2);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cargo_dropoff_covers_space_range_acl_flex_state_locks_and_specialized_holds() {
    let ctx = test_ctx();
    let (char_id, corp_id) = ctx.db.with_snapshot(|s| {
        let (&cid, ch) = s.characters.iter().next().unwrap();
        (cid, ch.corporation_id)
    });
    let sys_id = 30_000_142_u32;
    let ship_id = 600_001_i64;
    let st_id = 1_030_000_000_030_i64;
    let flex_id = 1_030_000_000_031_i64;

    let mut s = make_session(1, char_id, corp_id, 0, 0);
    s.solar_system_id = Some(sys_id);

    // MustBeInSpace when docked
    s.station_id = Some(60_003_760);
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[]).await.unwrap_err().code,
        "MustBeInSpace"
    );
    s.station_id = None;

    // NoActiveShip
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[]).await.unwrap_err().code,
        "NoActiveShip"
    );
    s.ship_id = Some(ship_id);

    // MissingStructureId & NoItemsSpecified
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[]).await.unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[PyRep::Integer(st_id)])
            .await
            .unwrap_err()
            .code,
        "NoItemsSpecified"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: CITADEL_TYPE,
                name: "Dropoff Citadel".into(),
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                state: life::STATE_HULL_REINFORCED.into(),
                quantum_core_installed: true,
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.structures.insert(
            flex_id,
            UpwellStructureRecord {
                structure_id: flex_id,
                type_id: FLEX_TYPE,
                name: "Ansiblex".into(),
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                state: life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: false,
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.items.insert(
            610_001,
            InventoryItemRecord {
                item_id: 610_001,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            610_002,
            InventoryItemRecord {
                item_id: 610_002,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            610_003,
            InventoryItemRecord {
                item_id: 610_003,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Hangar, // invalid for cargo dropoff
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // StructureNotFound (wrong id)
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[PyRep::Integer(999_777), PyRep::Integer(610_001)])
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    // StructureNotDockable (FLEX)
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[PyRep::Integer(flex_id), PyRep::Integer(610_001)])
            .await
            .unwrap_err()
            .code,
        "StructureNotDockable"
    );

    // StructureNotOperational (HullReinforced)
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[PyRep::Integer(st_id), PyRep::Integer(610_001)])
            .await
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );

    // Set operational & test ShipNotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().state = life::STATE_SHIELD_VULNERABLE.into();
    });
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[PyRep::Integer(st_id), PyRep::Integer(610_001)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship outside range (> radius 5000 + 2500 = 7500m) -> OutsideCargoDropoffRange
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(9_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_cargo_dropoff(&ctx, &mut s, &[PyRep::Integer(st_id), PyRep::Integer(610_001)])
            .await
            .unwrap_err()
            .code,
        "OutsideCargoDropoffRange"
    );

    // Move ship within range -> ItemNotInShipCargo for Hangar item 610_003
    sys.try_set_ball_position(ship_id, DVec3::new(6_000.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&s.character_id.unwrap()) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(610_003)])]
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotInShipCargo"
    );

    // ItemLocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            7002,
            ContractRecord {
                contract_id: 7002,
                status: "Outstanding".into(),
                item_ids: vec![610_001],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut s,
            &[PyRep::Integer(st_id), PyRep::Tuple(vec![PyRep::Integer(610_001)])]
        )
        .await
        .unwrap_err()
        .code,
        "ItemLocked"
    );
    ctx.db.with_mut_snapshot(|snap| snap.contracts.clear());

    // Success for Cargo + SpecializedOreHold
    assert_eq!(
        handle_cargo_dropoff(
            &ctx,
            &mut s,
            &[
                PyRep::Integer(st_id),
                PyRep::List(vec![PyRep::Integer(610_001), PyRep::Integer(610_002)])
            ]
        )
        .await
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        for iid in [610_001, 610_002] {
            let it = &snap.items[&iid];
            assert_eq!(it.location_id, st_id);
            assert_eq!(it.flag, InventoryFlag::Deliveries);
            assert_eq!(it.version, 2);
        }
    });
}
