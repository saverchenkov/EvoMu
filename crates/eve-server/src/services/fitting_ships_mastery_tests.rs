//! Unit tests for `fitting_ships_mastery` (`LeaveShip`, `CreateNewbieShip`, `GetRequiredSkillLevels`, `GetCurrCharMasteryLevel`, `GetCertificatesForShipByMasteryLevel`, `GetSkillsRequiredForMasteryLevel`, `GetItemKillCountPlayer`, `SetShipKillCounter`, `GetDamagedModulesAndRepairCost`, `GetShipFittingInfo`, `TrashItems`, `GetDirtTimestamp`, `ResetDirtTimestamp`).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn ships_mastery_leave_ship_corvette_mastery_killmarks_repair_simulation_trash_and_dirt() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_400_100i64;
    let trash_item_id = 96_400_200i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "MasteryPilot".into();
        ch.corporation_id = 1_000_009;
        ch.race_id = 2; // Minmatar -> Reaper
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 15.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            trash_item_id,
            InventoryItemRecord {
                item_id: trash_item_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
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
    session.ship_id = Some(ship_id);
    session.role = u64::MAX; // GM role for SetShipKillCounter

    // 1. GetRequiredSkillLevels, GetCurrCharMasteryLevel, GetCertificatesForShipByMasteryLevel, GetSkillsRequiredForMasteryLevel
    let req_skills = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetRequiredSkillLevels",
        &[PyRep::Integer(587)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetRequiredSkillLevels ok");
    assert_ne!(req_skills, PyRep::None);

    let mastery_lvl = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetCurrCharMasteryLevel",
        &[PyRep::Integer(587)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetCurrCharMasteryLevel ok");
    assert_eq!(mastery_lvl, PyRep::Integer(0));

    let certs_by_lvl = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetCertificatesForShipByMasteryLevel",
        &[PyRep::Integer(587), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetCertificatesForShipByMasteryLevel ok");
    assert_ne!(certs_by_lvl, PyRep::None);

    let skills_by_lvl = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetSkillsRequiredForMasteryLevel",
        &[PyRep::Integer(587), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetSkillsRequiredForMasteryLevel ok");
    assert_ne!(skills_by_lvl, PyRep::None);

    // 2. Kill counter & Repair cost & Dirt timestamp & GetShipFittingInfo
    let set_kills = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "shipKillCounter",
        "SetShipKillCounter",
        &[PyRep::Integer(ship_id), PyRep::Integer(7)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("SetShipKillCounter ok");
    assert_eq!(set_kills, PyRep::Integer(7));

    let get_kills = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "shipKillCounter",
        "GetItemKillCountPlayer",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetItemKillCountPlayer ok");
    assert_eq!(get_kills, PyRep::Integer(7));

    let repair_info = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "repairSvc",
        "GetDamagedModulesAndRepairCost",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetDamagedModulesAndRepairCost ok");
    assert_ne!(repair_info, PyRep::None);

    let sim_info = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetShipFittingInfo",
        &[
            PyRep::Integer(587),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(484),
                PyRep::Integer(27),
            ])]),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetShipFittingInfo ok");
    assert_ne!(sim_info, PyRep::None);

    let dirt_ts = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "ResetDirtTimestamp",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ResetDirtTimestamp ok");
    let got_dirt = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "GetDirtTimestamp",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetDirtTimestamp ok");
    assert_eq!(dirt_ts, got_dirt);

    // 3. TrashItems
    let _ = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(trash_item_id)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("TrashItems ok");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&trash_item_id)));

    // 4. LeaveShip -> Capsule -> remove old ship -> CreateNewbieShip
    let pod_res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "ship",
        "LeaveShip",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("LeaveShip ok");
    assert!(matches!(pod_res, PyRep::Integer(id) if id > 0));

    // Remove the Rifter from station so CreateNewbieShip issues a free corvette
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&ship_id);
    });
    let corvette_res = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "CreateNewbieShip",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("CreateNewbieShip ok");
    assert_ne!(corvette_res, PyRep::None);
}
