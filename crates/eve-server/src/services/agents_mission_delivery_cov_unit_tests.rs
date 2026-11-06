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
