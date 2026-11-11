//! Unit tests for `fitting_saved_multifit` (`insuranceSvc.GetInsurancePrice`, `insuranceSvc.InsureShip`, `insuranceSvc.GetContractForShip`, `dynamicItemService.GetDynamicItemInfo`, `repackagingSvc.RepackageItems`, `ship.AssembleShip`, `repairSvc.RepairAll`, `invbroker.SetLabel`).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "fitting_saved_multifit_extra_unit_tests.rs"]
mod extra_unit_tests;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_multifit_insurance_dynamic_item_repackage_assemble_repair_all_and_set_label() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_600_001i64;
    let station_id = 60_003_760i64;
    let active_ship_id = 96_600_100i64;
    let spare_ship_id = 96_600_200i64;
    let abyssal_mod_id = 96_600_300i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "MultifitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_f64_isk(100_000_000.0);
        ch.active_ship_id = active_ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            active_ship_id,
            InventoryItemRecord {
                item_id: active_ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 10.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            spare_ship_id,
            InventoryItemRecord {
                item_id: spare_ship_id,
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
            abyssal_mod_id,
            InventoryItemRecord {
                item_id: abyssal_mod_id,
                type_id: 47702, // Abyssal 1MN Afterburner
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(6, 28.5), (30, 6_500.0)]),
                source_mutaplasmid_id: Some(47740),
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(active_ship_id);

    // 1. Insurance: GetInsurancePrice, InsureShip (fraction 1.0 = Platinum), GetContractForShip
    let price = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "insuranceSvc",
        "GetInsurancePrice",
        &[PyRep::Integer(active_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetInsurancePrice ok");
    assert!(matches!(price, PyRep::Real(p) if p > 0.0));

    let insure_res = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(active_ship_id), PyRep::Real(1.0)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("InsureShip ok");
    assert_ne!(insure_res, PyRep::None);

    let contract_res = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "insuranceSvc",
        "GetContractForShip",
        &[PyRep::Integer(active_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetContractForShip ok");
    assert_ne!(contract_res, PyRep::None);

    // 2. dynamicItemService.GetDynamicItemInfo
    let dyn_info = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "dynamicItemService",
        "GetDynamicItemInfo",
        &[PyRep::Integer(abyssal_mod_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetDynamicItemInfo ok");
    assert_ne!(dyn_info, PyRep::None);

    // 3. RepackageItems & AssembleShip on spare_ship_id
    let repack_res = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(spare_ship_id)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("RepackageItems ok");
    assert_ne!(repack_res, PyRep::None);
    assert!(!ctx.db.with_snapshot(|s| s.items.get(&spare_ship_id).unwrap().singleton));

    let assemble_res = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "ship",
        "AssembleShip",
        &[PyRep::Integer(spare_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("AssembleShip ok");
    assert_ne!(assemble_res, PyRep::None);
    assert!(ctx.db.with_snapshot(|s| s.items.get(&spare_ship_id).unwrap().singleton));

    // 4. repairSvc.RepairAll
    let repair_res = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "repairSvc",
        "RepairAll",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("RepairAll ok");
    assert_ne!(repair_res, PyRep::None);
    let dmg_after = ctx.db.with_snapshot(|s| {
        s.items
            .get(&active_ship_id)
            .and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied())
            .unwrap_or(0.0)
    });
    assert_eq!(dmg_after, 0.0);

    // 5. invbroker.SetLabel
    let label_res = handle_ships_and_services(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "SetLabel",
        &[
            PyRep::Integer(active_ship_id),
            PyRep::String("My Insurable Rifter".into()),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("SetLabel ok");
    assert_ne!(label_res, PyRep::None);
    let saved_name = ctx
        .db
        .with_snapshot(|s| s.item_custom_names.get(&active_ship_id).cloned());
    assert_eq!(saved_name.as_deref(), Some("My Insurable Rifter"));
}
