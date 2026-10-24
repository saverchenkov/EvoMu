//! Comprehensive Inventory, Ships, and Fitting integration test suite (Part 3: Asset Safety, SMA, Fleet Hangar, Jetcan, and Wreck Looting).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn new_sqlite_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    crate::common::pin_clock(&ctx);
    ctx
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

#[tokio::test]
async fn test_jetcan_wreck_sma_fleet_hangar_and_asset_safety() {
    // parity: feat:inventory.asset-safety-wraps, rpc:structureAssetSafety.MovePersonalAssetsToSafety, rpc:structureAssetSafety.GetItemsInSafetyForCharacter, rpc:structureAssetSafety.GetItemsInSafetyForCorp, rpc:structureAssetSafety.GetStructuresICanDeliverTo, rpc:structureAssetSafety.GetWrapNames, rpc:structureAssetSafety.MoveSafetyWrapToStructure, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:inventory.asset-safety-wraps",
        "rpc:structureAssetSafety.MovePersonalAssetsToSafety",
        "rpc:structureAssetSafety.GetItemsInSafetyForCharacter",
        "rpc:structureAssetSafety.GetItemsInSafetyForCorp",
        "rpc:structureAssetSafety.GetStructuresICanDeliverTo",
        "rpc:structureAssetSafety.GetWrapNames",
        "rpc:structureAssetSafety.MoveSafetyWrapToStructure",
    ]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7104);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;

    let structure_id = 1_040_000_001i64;
    let safety_item_id = 820_010i64;
    seed_item(&ctx, safety_item_id, 34, char_id, structure_id, InventoryFlag::Hangar, 10_000, false);
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Asset Safety Test Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let wrap_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("MovePersonalAssetsToSafety");
    let wrap_id = wrap_rep.dict_get("wrapID").and_then(|v| v.as_i64()).unwrap();
    assert!(wrap_id > 0);
    assert_eq!(wrap_rep.dict_get("structureID").and_then(|v| v.as_i64()), Some(structure_id));
    let wraps_list = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "GetItemsInSafetyForCharacter",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetItemsInSafetyForCharacter");
    assert!(matches!(wraps_list, PyRep::List(ref l) if !l.is_empty()));
    let _ = ctx
        .dispatch_rpc(&mut sess, "structureAssetSafety", None, "GetItemsInSafetyForCorp", &[], &kw, None)
        .await
        .expect("GetItemsInSafetyForCorp");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "GetStructuresICanDeliverTo",
            &[PyRep::Integer(sys_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetStructuresICanDeliverTo");
    let _ = ctx
        .dispatch_rpc(&mut sess, "structureAssetSafety", None, "GetWrapNames", &[], &kw, None)
        .await
        .expect("GetWrapNames");

    ctx.advance_virtual_clock(6 * 86_400);
    let deliv_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_id), PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("MoveSafetyWrapToStructure after 5d cooldown");
    assert_eq!(deliv_rep.dict_get("deliveredItemCount").and_then(|v| v.as_i64()), Some(1));
}

#[tokio::test]
async fn test_jetcan_wreck_sma_and_fleet_hangar() {
    // parity: feat:ships.ship-maintenance-bay, feat:ships.fleet-hangar, feat:ships.ship-config, rpc:ship.BoardStoredShip, rpc:ship.LaunchFromContainer, rpc:eject.LaunchShipOrContainerFromWreckOrContainer, rpc:ship.GetShipConfiguration, rpc:charUnboundMgr.SelectCharacterID, rpc:ship.Board
    eve_testkit::attest(&[
        "feat:ships.ship-maintenance-bay",
        "feat:ships.fleet-hangar",
        "feat:ships.ship-config",
        "rpc:ship.BoardStoredShip",
        "rpc:ship.LaunchFromContainer",
        "rpc:eject.LaunchShipOrContainerFromWreckOrContainer",
        "rpc:ship.GetShipConfiguration",
    ]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7113);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let spare_rifter_id = 820_001i64;
    seed_item(&ctx, spare_rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let orca_id = 820_020i64;
    seed_item(&ctx, orca_id, 28606, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(orca_id)], &kw, None)
        .await
        .expect("Board Orca");

    let store_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "StoreVessel",
            &[PyRep::Integer(spare_rifter_id), PyRep::Integer(orca_id)],
            &kw,
            None,
        )
        .await
        .expect("StoreVessel Rifter into Orca SMA");
    assert_eq!(store_rep.dict_get("flagID").and_then(|v| v.as_i64()), Some(90));

    let ore_for_orca = 820_021i64;
    seed_item(&ctx, ore_for_orca, 1228, char_id, station_id, InventoryFlag::Hangar, 50, false);
    let sma_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(ore_for_orca),
                PyRep::Integer(orca_id),
                PyRep::Integer(90),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Non-ship item in SMA must fail");
    assert_eq!(sma_err.code, "CannotStoreCargoInSMA");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(ore_for_orca),
            PyRep::Integer(orca_id),
            PyRep::Integer(155),
        ],
        &kw,
        None,
    )
    .await
    .expect("Add item to Orca Fleet Hangar (flag 155)");
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "ConfigureShip",
        &[PyRep::Integer(orca_id)],
        &kw,
        None,
    )
    .await
    .expect("ConfigureShip");
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "SetShipPassword",
        &[PyRep::Integer(orca_id), PyRep::String("orca-bay".into())],
        &kw,
        None,
    )
    .await
    .expect("SetShipPassword");
    let _ = ctx
        .dispatch_rpc(&mut sess, "ship", None, "GetShipConfiguration", &[PyRep::Integer(orca_id)], &kw, None)
        .await
        .expect("GetShipConfiguration");

    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&orca_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(
            Ball {
                id: orca_id,
                type_id: 28606,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 150_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    let launch_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "LaunchFromContainer",
            &[PyRep::Integer(spare_rifter_id)],
            &kw,
            None,
        )
        .await
        .expect("LaunchFromContainer");
    assert_eq!(launch_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(sys_id as i64));

    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "ScoopToSMA",
        &[PyRep::Integer(spare_rifter_id), PyRep::Integer(orca_id)],
        &kw,
        None,
    )
    .await
    .expect("ScoopToSMA");

    let jet_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Jettison",
            &[PyRep::List(vec![PyRep::Integer(ore_for_orca)])],
            &kw,
            None,
        )
        .await
        .expect("Jettison ore into space");
    let jetcan_id = match &jet_rep {
        PyRep::Tuple(t) => match t.first() {
            Some(PyRep::List(l)) => l.first().and_then(|v| v.as_i64()).unwrap(),
            Some(v) => v.as_i64().unwrap(),
            None => panic!("empty tuple"),
        },
        _ => jet_rep.dict_get("jetcanID").and_then(|v| v.as_i64()).unwrap(),
    };

    let ore_second = 820_022i64;
    seed_item(&ctx, ore_second, 1228, char_id, orca_id, InventoryFlag::Cargo, 10, false);
    ctx.advance_virtual_clock(50);
    let cd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Jettison",
            &[PyRep::List(vec![PyRep::Integer(ore_second)])],
            &kw,
            None,
        )
        .await
        .expect_err("Jettison within 180s must fail with JettisonCooldownActive");
    assert_eq!(cd_err.code, "JettisonCooldownActive");

    let wreck_id = 820_030i64;
    let wreck_loot_id = 820_031i64;
    let wreck_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 186)
        .map(|t| t.type_id)
        .unwrap_or(26561);
    seed_item(&ctx, wreck_id, wreck_tid, 90_000_002, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, wreck_loot_id, 34, 90_000_002, wreck_id, InventoryFlag::Cargo, 500, false);
    let loot_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(wreck_loot_id),
                PyRep::Integer(orca_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Loot foreign wreck");
    assert_eq!(loot_rep.dict_get("suspectFlagged").and_then(|v| v.as_bool()), Some(true));
    let cw_notifs = ctx.drain_notifications_for_char(char_id);
    let flag_upd = cw_notifs
        .iter()
        .find(|n| n.event == "OnSystemCriminalFlagUpdates")
        .expect("OnSystemCriminalFlagUpdates after foreign wreck loot");
    assert_eq!(flag_upd.id_type, "solarsystemid");
    assert_eq!(
        flag_upd.args,
        vec![
            PyRep::List(vec![]),
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::List(vec![]),
        ]
    );
    assert!(cw_notifs.iter().all(|n| n.event != "OnSuspectsAndCriminalsUpdate"));
    assert!(jetcan_id > 0);
}
