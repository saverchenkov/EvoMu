//! Unit tests covering `agents_mission_delivery::verify_and_consume_mission_delivery_items`.

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};

#[test]
fn mission_delivery_covers_courier_and_non_courier_validation_and_partial_consumption() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 90_000_001i64;
    let agent_station = 60_003_760i64;
    let dest_station = 60_003_761i64;
    let ship_id = 99_100_100i64;

    // 1. Courier validation: missing destination, wrong station, insufficient quantity
    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap, char_id, 10, 34, 100, true, false, Some(agent_station), Some(agent_station),
            Some(ship_id), None, Some(1),
        )
        .expect_err("missing dest")
        .code,
        "MissingDestinationStation"
    );

    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            10,
            34,
            100,
            true,
            false,
            Some(agent_station),
            Some(agent_station),
            Some(ship_id),
            Some(dest_station),
            Some(1),
        )
        .expect_err("not at dest")
        .code,
        "CourierNotAtDestinationStation"
    );

    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            10,
            34,
            100,
            true,
            false,
            Some(dest_station),
            Some(agent_station),
            Some(ship_id),
            Some(dest_station),
            Some(99_100_200),
        )
        .expect_err("missing cargo")
        .code,
        "MissionIncomplete"
    );

    // Seed two tagged courier stacks (80 + 50 = 130 units; consume 100 -> 1st removed, 2nd becomes 30)
    snap.items.insert(
        99_100_201,
        InventoryItemRecord {
            item_id: 99_100_201,
            type_id: 34,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 80,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    snap.item_custom_names
        .insert(99_100_201, format!("courier_cargo:{char_id}:10"));

    snap.items.insert(
        99_100_202,
        InventoryItemRecord {
            item_id: 99_100_202,
            type_id: 34,
            owner_id: char_id,
            location_id: dest_station,
            flag: InventoryFlag::Hangar,
            quantity: 50,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    snap.item_custom_names
        .insert(99_100_202, format!("courier_cargo:{char_id}:10"));

    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        10,
        34,
        100,
        true,
        false,
        Some(dest_station),
        Some(agent_station),
        Some(ship_id),
        Some(dest_station),
        Some(99_100_200),
    )
    .expect("courier delivery consumes across cargo and hangar");
    assert!(!snap.items.contains_key(&99_100_201));
    assert_eq!(snap.items.get(&99_100_202).map(|i| i.quantity), Some(30));

    // 2. Non-courier delivery excludes remaining tagged courier cargo and checks station
    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            11,
            34,
            10,
            false,
            false,
            Some(dest_station),
            Some(agent_station),
            Some(ship_id),
            None,
            None,
        )
        .expect_err("must dock at agent station")
        .code,
        "MissionIncomplete"
    );

    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            11,
            34,
            10,
            false,
            false,
            Some(agent_station),
            Some(agent_station),
            Some(ship_id),
            None,
            None,
        )
        .expect_err("borrowed courier cargo excluded")
        .code,
        "MissionIncomplete"
    );

    // Non-courier multi-stack hangar + ore hold consumption
    snap.items.insert(
        99_100_301,
        InventoryItemRecord {
            item_id: 99_100_301,
            type_id: 1230, // Veldspar
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 40,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    snap.items.insert(
        99_100_302,
        InventoryItemRecord {
            item_id: 99_100_302,
            type_id: 1230,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::SpecializedOreHold,
            quantity: 60,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        12,
        1230,
        75,
        false,
        true,
        Some(agent_station),
        Some(agent_station),
        Some(ship_id),
        None,
        None,
    )
    .expect("mining mission consumes hangar + ore hold");
    assert!(!snap.items.contains_key(&99_100_301));
    assert_eq!(snap.items.get(&99_100_302).map(|i| i.quantity), Some(25));
}

#[test]
fn courier_delivery_locked_ancestor_gid_boundary_and_deterministic_order() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 90_000_011i64;
    let other_char = 90_000_012i64;
    let agent_station = 60_003_760i64;
    let dest_station = 60_003_761i64;
    let ship_id = 99_200_001i64;
    let other_ship = 99_200_002i64;
    let impounded_ship = 99_200_003i64;
    let mission_id = 42u32;
    let gid = 99_200_100i64;

    // Ancestor ship is impounded -> items in its cargo MUST be excluded by !is_item_or_ancestor_locked_or_impounded
    snap.items.insert(
        impounded_ship,
        InventoryItemRecord {
            item_id: impounded_ship,
            type_id: 587,
            owner_id: char_id,
            location_id: dest_station,
            flag: InventoryFlag::Impounded,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );
    snap.items.insert(
        gid + 1,
        InventoryItemRecord {
            item_id: gid + 1,
            type_id: 34,
            owner_id: char_id,
            location_id: impounded_ship,
            flag: InventoryFlag::Cargo,
            quantity: 50,
            singleton: false,
            ..Default::default()
        },
    );
    snap.item_custom_names
        .insert(gid + 1, format!("courier_cargo:{char_id}:{mission_id}"));

    // Granted before gid (gid - 1), wrong owner, wrong type, zero qty, wrong mission tag, wrong ship,
    // wrong hold (OreHold not accepted for courier), wrong station hangar
    for (id, owner, tid, loc, flag, qty, tag) in [
        (gid - 1, char_id, 34, dest_station, InventoryFlag::Hangar, 50, format!("courier_cargo:{char_id}:{mission_id}")),
        (gid + 2, other_char, 34, dest_station, InventoryFlag::Hangar, 50, format!("courier_cargo:{char_id}:{mission_id}")),
        (gid + 3, char_id, 35, dest_station, InventoryFlag::Hangar, 50, format!("courier_cargo:{char_id}:{mission_id}")),
        (gid + 4, char_id, 34, dest_station, InventoryFlag::Hangar, 0, format!("courier_cargo:{char_id}:{mission_id}")),
        (gid + 5, char_id, 34, dest_station, InventoryFlag::Hangar, 50, format!("courier_cargo:{char_id}:999")),
        (gid + 6, char_id, 34, other_ship, InventoryFlag::Cargo, 50, format!("courier_cargo:{char_id}:{mission_id}")),
        (gid + 7, char_id, 34, ship_id, InventoryFlag::SpecializedOreHold, 50, format!("courier_cargo:{char_id}:{mission_id}")),
        (gid + 8, char_id, 34, agent_station, InventoryFlag::Hangar, 50, format!("courier_cargo:{char_id}:{mission_id}")),
    ] {
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: tid,
                owner_id: owner,
                location_id: loc,
                flag,
                quantity: qty,
                singleton: false,
                ..Default::default()
            },
        );
        snap.item_custom_names.insert(id, tag);
    }

    // Even querying with active_ship_id = impounded_ship sees 0 valid units
    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            mission_id,
            34,
            10,
            true,
            false,
            Some(dest_station),
            Some(agent_station),
            Some(impounded_ship),
            Some(dest_station),
            Some(gid),
        )
        .unwrap_err()
        .code,
        "MissionIncomplete"
    );

    // granted_courier_item_id = None -> invalid even if matching items exist in Hangar
    snap.items.insert(
        gid, // exact equality item_id == gid
        InventoryItemRecord {
            item_id: gid,
            type_id: 34,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 20,
            singleton: false,
            ..Default::default()
        },
    );
    snap.item_custom_names
        .insert(gid, format!("courier_cargo:{char_id}:{mission_id}"));
    snap.items.insert(
        gid + 20,
        InventoryItemRecord {
            item_id: gid + 20,
            type_id: 34,
            owner_id: char_id,
            location_id: dest_station,
            flag: InventoryFlag::Hangar,
            quantity: 30,
            singleton: false,
            ..Default::default()
        },
    );
    snap.item_custom_names
        .insert(gid + 20, format!("courier_cargo:{char_id}:{mission_id}"));

    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            mission_id,
            34,
            20,
            true,
            false,
            Some(dest_station),
            Some(agent_station),
            Some(ship_id),
            Some(dest_station),
            None,
        )
        .unwrap_err()
        .code,
        "MissionIncomplete"
    );

    // Exact gid boundary succeeds and consumes lowest item_id (`gid` = 20) first, leaving `gid + 20` at 15!
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        mission_id,
        34,
        35,
        true,
        false,
        Some(dest_station),
        Some(agent_station),
        Some(ship_id),
        Some(dest_station),
        Some(gid),
    )
    .expect("exact gid boundary & sorted consumption succeed");
    assert!(!snap.items.contains_key(&gid));
    assert!(!snap.item_custom_names.contains_key(&gid));
    assert_eq!(snap.items.get(&(gid + 20)).map(|i| i.quantity), Some(15));
    // Impounded & excluded items must remain untouched
    assert_eq!(snap.items.get(&(gid + 1)).map(|i| i.quantity), Some(50));
    assert_eq!(snap.items.get(&(gid - 1)).map(|i| i.quantity), Some(50));
}

#[test]
fn non_courier_delivery_excludes_fitted_non_empty_locked_and_handles_trit_and_none_station() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 90_000_021i64;
    let agent_station = 60_003_760i64;
    let other_station = 60_003_762i64;
    let ship_id = 99_300_001i64;

    // 1. Fitted module in active ship HiSlot0 matching req_tid=3100 is excluded (!is_fitted_slot_flag)
    snap.items.insert(
        99_300_010,
        InventoryItemRecord {
            item_id: 99_300_010,
            type_id: 3100,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );
    // Locked item in hangar excluded
    snap.items.insert(
        99_300_011,
        InventoryItemRecord {
            item_id: 99_300_011,
            type_id: 2000,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Locked,
            quantity: 5,
            singleton: false,
            ..Default::default()
        },
    );
    // Non-empty container (type 23) at hangar excluded (!is_container_non_empty), plus child item inside it
    snap.items.insert(
        99_300_012,
        InventoryItemRecord {
            item_id: 99_300_012,
            type_id: 23,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );
    snap.items.insert(
        99_300_013,
        InventoryItemRecord {
            item_id: 99_300_013,
            type_id: 2000,
            owner_id: char_id,
            location_id: 99_300_012, // inside non-empty container -> hangar condition false AND container is non-empty!
            flag: InventoryFlag::Cargo,
            quantity: 10,
            singleton: false,
            ..Default::default()
        },
    );

    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            51,
            23,
            1,
            false,
            false,
            Some(agent_station),
            Some(agent_station),
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err()
        .code,
        "MissionIncomplete"
    );

    // An empty sibling container of type 23 IS consumed while non-empty 99_300_012 remains intact
    snap.items.insert(
        99_300_014,
        InventoryItemRecord {
            item_id: 99_300_014,
            type_id: 23,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        51,
        23,
        1,
        false,
        false,
        Some(agent_station),
        Some(agent_station),
        Some(ship_id),
        None,
        None,
    )
    .expect("empty container eligible while non-empty excluded");
    assert!(snap.items.contains_key(&99_300_012));
    assert!(!snap.items.contains_key(&99_300_014));

    // 2. Fitted item still untouched even on is_mining_mission=true; ship cargo non-trit rejected when !is_mining_mission
    snap.items.insert(
        99_300_015,
        InventoryItemRecord {
            item_id: 99_300_015,
            type_id: 3100,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            singleton: false,
            ..Default::default()
        },
    );
    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            52,
            3100,
            1,
            false,
            false,
            Some(agent_station),
            Some(agent_station),
            Some(ship_id),
            None,
            None,
        )
        .unwrap_err()
        .code,
        "MissionIncomplete"
    );
    assert!(snap.items.contains_key(&99_300_010));

    // 3. req_tid == 34 (Tritanium) allows consuming from active_ship_id Cargo/OreHold even when is_mining_mission == false!
    snap.items.insert(
        99_300_020,
        InventoryItemRecord {
            item_id: 99_300_020,
            type_id: 34,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 40,
            singleton: false,
            ..Default::default()
        },
    );
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        53,
        34,
        40,
        false,
        false,
        Some(agent_station),
        Some(agent_station),
        Some(ship_id),
        None,
        None,
    )
    .expect("tritanium (type 34) accepted from ship cargo even on non-mining mission");
    assert!(!snap.items.contains_key(&99_300_020));

    // 4. agent_station_id == None skips docked station check and consumes Hangar item in any station;
    //    req_units == 0 succeeds as no-op.
    snap.items.insert(
        99_300_021,
        InventoryItemRecord {
            item_id: 99_300_021,
            type_id: 2000,
            owner_id: char_id,
            location_id: other_station,
            flag: InventoryFlag::Hangar,
            quantity: 8,
            singleton: false,
            ..Default::default()
        },
    );
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        54,
        2000,
        5,
        false,
        false,
        None,
        None,
        Some(ship_id),
        None,
        None,
    )
    .expect("None agent_station_id bypasses station check and consumes Hangar");
    assert_eq!(snap.items.get(&99_300_021).map(|i| i.quantity), Some(3));
    assert_eq!(snap.items.get(&99_300_011).map(|i| i.quantity), Some(5)); // locked hangar item still untouched

    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        55,
        99_999,
        0,
        false,
        false,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("zero required units succeeds");
}

#[test]
fn delivery_kills_locked_courier_multi_stack_rem_nonempty_fitted_and_active_ship_id_mutants() {
    let mut snap = WorldStateSnapshot::default();
    let char_id = 90_000_077_i64;
    let agent_station = 60_003_760_i64;
    let dest_station = 60_003_761_i64;
    let active_ship = 99_700_001_i64;
    let spare_ship = 99_700_002_i64;

    // 1. Courier lock & `!is_item_or_ancestor_locked_or_impounded` (L54) + multi-stack rem math (L86, L95)
    let c1 = 99_700_101_i64;
    let c2 = 99_700_102_i64;
    let c_locked = 99_700_103_i64;
    for (id, qty, locked) in [(c1, 4_i32, false), (c2, 5_i32, false), (c_locked, 10_i32, true)] {
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 34,
                owner_id: char_id,
                location_id: dest_station,
                flag: InventoryFlag::Hangar,
                quantity: qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(id, format!("courier_cargo:{char_id}:77"));
        if locked {
            snap.contracts.insert(
                99_700_901,
                eve_db::ContractRecord {
                    contract_id: 99_700_901,
                    status: "Outstanding".to_string(),
                    item_ids: vec![id],
                    ..Default::default()
                },
            );
        }
    }
    // Locked item alone cannot satisfy qty = 15 (only 4 + 5 = 9 valid units exist)
    assert_eq!(
        verify_and_consume_mission_delivery_items(
            &mut snap,
            char_id,
            77,
            34,
            10,
            true,
            false,
            Some(dest_station),
            Some(agent_station),
            Some(active_ship),
            Some(dest_station),
            Some(c1),
        )
        .unwrap_err()
        .code,
        "MissionIncomplete"
    );
    // Consuming 7 from 4 + 5 leaves c1 consumed, c2 at 2 (kills `rem += take` -> 0 and `rem /= take` -> 4),
    // and locked item completely untouched.
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        77,
        34,
        7,
        true,
        false,
        Some(dest_station),
        Some(agent_station),
        Some(active_ship),
        Some(dest_station),
        Some(c1),
    )
    .expect("two valid courier stacks must satisfy qty 7");
    assert!(!snap.items.contains_key(&c1));
    assert_eq!(snap.items.get(&c2).map(|i| i.quantity), Some(2));
    assert_eq!(snap.items.get(&c_locked).map(|i| i.quantity), Some(10));

    // 2. Non-courier invalid item exclusions (L114-L117):
    //    - negative quantity item (kills L114 `>` -> `<` and `&&` -> `||`)
    //    - contract/impound locked item (kills L115 `!` deletion and `&&` -> `||`)
    //    - fitted item on active_ship (kills L116 `!` deletion and `&&` -> `||`)
    //    - non-empty container item (kills L117 `!` deletion and `&&` -> `||`)
    let neg_id = 99_700_201_i64;
    let locked_non_courier = 99_700_202_i64;
    let fitted_id = 99_700_203_i64;
    let container_id = 99_700_204_i64;
    let child_id = 99_700_205_i64;

    snap.items.insert(
        neg_id,
        InventoryItemRecord {
            item_id: neg_id,
            type_id: 4051,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: -5,
            ..Default::default()
        },
    );
    snap.items.insert(
        locked_non_courier,
        InventoryItemRecord {
            item_id: locked_non_courier,
            type_id: 4052,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 5,
            ..Default::default()
        },
    );
    snap.contracts.insert(
        99_700_902,
        eve_db::ContractRecord {
            contract_id: 99_700_902,
            status: "Outstanding".to_string(),
            item_ids: vec![locked_non_courier],
            ..Default::default()
        },
    );
    snap.items.insert(
        fitted_id,
        InventoryItemRecord {
            item_id: fitted_id,
            type_id: 4053,
            owner_id: char_id,
            location_id: active_ship,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );
    snap.items.insert(
        container_id,
        InventoryItemRecord {
            item_id: container_id,
            type_id: 4054,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 2,
            singleton: false,
            ..Default::default()
        },
    );
    snap.items.insert(
        child_id,
        InventoryItemRecord {
            item_id: child_id,
            type_id: 34,
            owner_id: char_id,
            location_id: container_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            ..Default::default()
        },
    );

    for (m_id, tid) in [(78_u32, 4051_u32), (79, 4052), (80, 4053), (81, 4054)] {
        assert_eq!(
            verify_and_consume_mission_delivery_items(
                &mut snap,
                char_id,
                m_id,
                tid,
                1,
                false,
                true,
                Some(agent_station),
                Some(agent_station),
                Some(active_ship),
                None,
                None,
            )
            .unwrap_err()
            .code,
            "MissionIncomplete",
            "excluded item for type {tid} must not satisfy delivery"
        );
    }

    // Valid stack coexists with negative-quantity stack (kills L114 `>` -> `==` / `<`)
    let valid_mix = 99_700_206_i64;
    snap.items.insert(
        valid_mix,
        InventoryItemRecord {
            item_id: valid_mix,
            type_id: 4051,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 5,
            ..Default::default()
        },
    );
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        82,
        4051,
        5,
        false,
        false,
        Some(agent_station),
        Some(agent_station),
        Some(active_ship),
        None,
        None,
    )
    .expect("valid positive stack satisfies 5 units alongside negative stack");
    assert!(!snap.items.contains_key(&valid_mix));
    assert_eq!(snap.items.get(&neg_id).map(|i| i.quantity), Some(-5));

    // 3. `ConsumeExactOptions.active_ship_id` (L156) protects the currently boarded active ship
    //    even if matching type exists in Hangar; spare_ship (higher ID) is consumed instead.
    snap.items.insert(
        active_ship,
        InventoryItemRecord {
            item_id: active_ship,
            type_id: 606,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            ..Default::default()
        },
    );
    snap.items.insert(
        spare_ship,
        InventoryItemRecord {
            item_id: spare_ship,
            type_id: 606,
            owner_id: char_id,
            location_id: agent_station,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            ..Default::default()
        },
    );
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        83,
        606,
        1,
        false,
        false,
        Some(agent_station),
        Some(agent_station),
        Some(active_ship),
        None,
        None,
    )
    .expect("consumes spare hull while active_ship_id remains protected");
    assert!(
        snap.items.contains_key(&active_ship),
        "active_ship_id must not be consumed"
    );
    assert!(!snap.items.contains_key(&spare_ship));

    // 4. Non-courier multi-stack `rem -= take` (L167-L168): 4 + 5 + 5 with req=7 consumes d1 completely,
    //    leaves d2 at 2 and d3 untouched at 5.
    let d1 = 99_700_301_i64;
    let d2 = 99_700_302_i64;
    let d3 = 99_700_303_i64;
    for (id, qty) in [(d1, 4_i32), (d2, 5_i32), (d3, 5_i32)] {
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 4060,
                owner_id: char_id,
                location_id: agent_station,
                flag: InventoryFlag::Hangar,
                quantity: qty,
                ..Default::default()
            },
        );
    }
    verify_and_consume_mission_delivery_items(
        &mut snap,
        char_id,
        84,
        4060,
        7,
        false,
        false,
        Some(agent_station),
        Some(agent_station),
        Some(active_ship),
        None,
        None,
    )
    .expect("non-courier multi-stack qty 7");
    assert!(!snap.items.contains_key(&d1));
    assert_eq!(snap.items.get(&d2).map(|i| i.quantity), Some(2));
    assert_eq!(snap.items.get(&d3).map(|i| i.quantity), Some(5));
}
