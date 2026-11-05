use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use eve_db::{InventoryFlag, IskCents, UpwellStructureRecord};

#[tokio::test]
async fn test_repair_service_quotes_and_repair_in_station_and_structure() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = 60_003_760_i64;
    session.station_id = Some(station_id);

    // Seed ship in station hangar with hull/armor damage and a damaged fitted module
    let mod_id = 710_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.balance = IskCents::from_isk(10_000_000);
            c.station_id = Some(station_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = station_id;
            ship.flag = InventoryFlag::Hangar;
            ship.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 50.0);
            ship.dynamic_attributes.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 100.0);
        }
        let mut mod_item = make_fighter_item(mod_id, 34, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        mod_item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(mod_id, mod_item);
    });

    assert!(is_repairable_ship_child_flag(InventoryFlag::HiSlot0));
    assert!(is_repairable_ship_child_flag(InventoryFlag::DroneBay));
    assert!(!is_repairable_ship_child_flag(InventoryFlag::Cargo));

    // GetRepairQuotes for ship (includes child module)
    let quotes = handle_repair_get_quotes(
        &ctx,
        &session,
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .unwrap();
    if let PyRep::Dict(entries) = quotes {
        assert_eq!(entries.len(), 1);
        let (_, ship_rows) = &entries[0];
        assert!(matches!(ship_rows, PyRep::List(ref rows) if rows.len() == 2));
    } else {
        panic!("expected dict");
    }

    // RepairItemsInLocation in station
    let rep_res = handle_repair_items_in_location(
        &ctx,
        &session,
        false,
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .unwrap();
    assert_eq!(
        rep_res.dict_get("locationID").and_then(|v| v.as_i64()),
        Some(station_id)
    );
    let total_cost = rep_res
        .dict_get("totalCostISK")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(total_cost > 0.0);

    // Verify damage was cleared
    let rem_dmg = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&ship_id)
            .and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE))
            .copied()
            .unwrap_or(0.0)
    });
    assert_eq!(rem_dmg, 0.0);

    // Test Upwell structure repair path (free repair when docked at online structure)
    let struct_id = 1_000_000_000_555_i64;
    session.station_id = None;
    session.structure_id = Some(struct_id);
    ctx.db.with_mut_snapshot(|snap| {
        let owner_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1_000_009);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832, // Astrahus
                owner_corp_id: owner_corp,
                solar_system_id: sys_id,
                name: "Repair Citadel".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                fuel_blocks: 100,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = struct_id;
            ship.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 25.0);
        }
    });

    let struct_rep = handle_repair_items_in_location(
        &ctx,
        &session,
        true,
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
    )
    .unwrap();
    assert_eq!(
        struct_rep
            .dict_get("totalCostISK")
            .and_then(|v| v.as_f64()),
        Some(0.0)
    );
}
