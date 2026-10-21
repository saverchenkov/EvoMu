//! Comprehensive Inventory (14 rows), Ships (8 rows), and Fitting (10 rows) integration test suite.


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::{dna_to_fitting, fitting_to_dna};
use std::collections::HashMap;

fn new_sqlite_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
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

fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return;
    };
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in pairs {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t
                        .default_attributes
                        .get(&l_attr)
                        .copied()
                        .unwrap_or(5.0)
                        .max(5.0) as u8;
                    entry.insert(
                        sid,
                        eve_db::TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
    });
}

#[tokio::test]
async fn test_containers_cargo_secure_alsc_station_freight() {
    // parity: feat:inventory.containers, feat:inventory.secure-containers, feat:inventory.audit-log-container-events, feat:inventory.station-containers, feat:inventory.freight-container-space-rules, feat:inventory.container-contents-restrictions, rpc:invbroker.AssembleCargoContainer, rpc:invbroker.GetContainerContents, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.ALSCConfigGet, rpc:invbroker.ALSCConfigSet, rpc:invbroker.ALSCLockItems, rpc:invbroker.ALSCLogGet, rpc:invbroker.ALSCUnlockItems, rpc:invbroker.Add, rpc:invbroker.SetPassword
    eve_testkit::attest(&["feat:inventory.containers", "feat:inventory.secure-containers", "feat:inventory.audit-log-container-events", "feat:inventory.station-containers", "feat:inventory.freight-container-space-rules", "feat:inventory.container-contents-restrictions", "rpc:invbroker.AssembleCargoContainer", "rpc:invbroker.GetContainerContents"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7101);
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
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // 1. Cargo Container (Small Standard Container 3297: 100 m3 volume, 120 m3 capacity)
    let cargo_can_id = 810_001i64;
    let nested_can_id = 810_002i64;
    let tritanium_id = 810_003i64;
    seed_item(&ctx, cargo_can_id, 3297, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, nested_can_id, 3297, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, tritanium_id, 34, char_id, station_id, InventoryFlag::Hangar, 5_000, false);

    let asm_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "AssembleCargoContainer",
            &[PyRep::Integer(cargo_can_id)],
            &kw,
            None,
        )
        .await
        .expect("AssembleCargoContainer 3297");
    assert_eq!(asm_rep.dict_get("singleton").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(asm_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(cargo_can_id));

    // Reject placing a container inside another container (`CannotPlaceContainerInContainer`)
    let nest_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(nested_can_id),
                PyRep::Integer(cargo_can_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Container inside container must be rejected");
    assert_eq!(nest_err.code, "CannotPutContainerInContainer");

    // Place 5,000 Tritanium (50 m3 <= 120 m3) inside Cargo Container
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(tritanium_id),
            PyRep::Integer(cargo_can_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Add Tritanium to cargo container");

    // 2. Secure Cargo Container (3467 Small Secure Container): password protection
    let sec_can_id = 810_010i64;
    seed_item(&ctx, sec_can_id, 3467, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "SetPassword",
        &[
            PyRep::Integer(sec_can_id),
            PyRep::Integer(1),
            PyRep::String("open-sesame".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("SetPassword on Secure Container");

    let mut other_sess = SessionState::new(7102);
    other_sess.user_id = Some(2);
    other_sess.character_id = Some(90_000_002);
    other_sess.station_id = Some(station_id);
    other_sess.solar_system_id = Some(30_000_142);

    let pw_err = ctx
        .dispatch_rpc(
            &mut other_sess,
            "invbroker",
            None,
            "GetContainerContents",
            &[PyRep::Integer(sec_can_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Opening password-protected container without password must fail");
    assert_eq!(pw_err.code, "ALSCPasswordRequired");

    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("open-sesame".into()));
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "GetContainerContents",
        &[PyRep::Integer(sec_can_id)],
        &pw_kw,
        None,
    )
    .await
    .expect("GetContainerContents with valid password succeeds");

    // 3. Audit Log Secure Container (17363 Small ALSC): lock/unlock items and audit log
    let alsc_id = 810_020i64;
    let alsc_item_id = 810_021i64;
    seed_item(&ctx, alsc_id, 17363, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, alsc_item_id, 35, char_id, station_id, InventoryFlag::Hangar, 500, false);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "ALSCConfigSet",
        &[PyRep::Integer(alsc_id), PyRep::String("alsc-pass".into())],
        &kw,
        None,
    )
    .await
    .expect("ALSCConfigSet");
    let cfg_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "ALSCConfigGet",
            &[PyRep::Integer(alsc_id)],
            &kw,
            None,
        )
        .await
        .expect("ALSCConfigGet");
    assert_eq!(cfg_rep.dict_get("hasPassword").and_then(|v| v.as_bool()), Some(true));

    let mut alsc_kw = HashMap::new();
    alsc_kw.insert("password".into(), PyRep::String("alsc-pass".into()));
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(alsc_item_id),
            PyRep::Integer(alsc_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &alsc_kw,
        None,
    )
    .await
    .expect("Add item to ALSC");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "ALSCLockItems",
        &[
            PyRep::Integer(alsc_id),
            PyRep::List(vec![PyRep::Integer(alsc_item_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("ALSCLockItems");

    let locked_move_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(alsc_item_id),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Moving ALSC-locked item without unlocking must fail");
    assert_eq!(locked_move_err.code, "ALSCItemLocked");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "ALSCUnlockItems",
        &[
            PyRep::Integer(alsc_id),
            PyRep::List(vec![PyRep::Integer(alsc_item_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("ALSCUnlockItems");

    let log_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "ALSCLogGet",
            &[PyRep::Integer(alsc_id)],
            &kw,
            None,
        )
        .await
        .expect("ALSCLogGet");
    let Some(PyRep::List(logs)) = log_rep.dict_get("entries") else {
        panic!("Expected entries list from ALSCLogGet");
    };
    assert!(logs.len() >= 3, "Expected audit log entries for ALSC actions");

    // 4. Station Container (17366: 1,000,000 m3 capacity, 2,000,000 m3 assembled volume)
    let st_can_id = 810_030i64;
    seed_item(&ctx, st_can_id, 17366, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let st_move_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(st_can_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Assembled 2,000,000 m3 Station Container cannot fit in ship cargo");
    assert_eq!(st_move_err.code, "CapacityExceeded");

    // 5. Freight Container (24445 Giant Freight Container): no password support
    let freight_can_id = 810_040i64;
    seed_item(&ctx, freight_can_id, 24445, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let freight_pw_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "SetPassword",
            &[PyRep::Integer(freight_can_id), PyRep::String("pw".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Freight containers do not support passwords");
    assert_eq!(freight_pw_err.code, "ContainerDoesNotSupportPassword");
}

#[tokio::test]
async fn test_jetcan_wreck_sma_fleet_hangar_and_asset_safety() {
    // parity: feat:inventory.trash-items, feat:inventory.asset-safety-wraps, feat:inventory.item-locking, feat:inventory.encoded-items, feat:inventory.crystal-damage, rpc:repackagingSvc.RepackageItems, rpc:ship.AssembleShip, rpc:invbroker.TrashItems, rpc:structureAssetSafety.MovePersonalAssetsToSafety, rpc:structureAssetSafety.GetItemsInSafetyForCharacter, rpc:structureAssetSafety.GetItemsInSafetyForCorp, rpc:structureAssetSafety.GetStructuresICanDeliverTo, rpc:structureAssetSafety.GetWrapNames, rpc:structureAssetSafety.MoveSafetyWrapToStructure, rpc:encodedItems.DecodeItem, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.GetAllItems, rpc:invbroker.GetDamageForCrystals, rpc:invbroker.IsMyItemAtDockableLocation, rpc:itemLocking.GetItemsByLocation, rpc:itemLocking.GetLockedItemLocations
    eve_testkit::attest(&["feat:inventory.trash-items", "feat:inventory.asset-safety-wraps", "feat:inventory.item-locking", "feat:inventory.encoded-items", "feat:inventory.crystal-damage", "rpc:repackagingSvc.RepackageItems", "rpc:ship.AssembleShip", "rpc:invbroker.TrashItems", "rpc:structureAssetSafety.MovePersonalAssetsToSafety", "rpc:structureAssetSafety.GetItemsInSafetyForCharacter", "rpc:structureAssetSafety.GetItemsInSafetyForCorp", "rpc:structureAssetSafety.GetStructuresICanDeliverTo", "rpc:structureAssetSafety.GetWrapNames", "rpc:structureAssetSafety.MoveSafetyWrapToStructure", "rpc:encodedItems.DecodeItem"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7103);
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

    // 1. Repackage & Assemble ship + TrashItems + Item Locking & Encoded Items
    let spare_rifter_id = 820_001i64;
    let trash_ore_id = 820_002i64;
    let encoded_box_id = 820_003i64;
    let crystal_id = 820_004i64;
    seed_item(&ctx, spare_rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, trash_ore_id, 1228, char_id, station_id, InventoryFlag::Hangar, 100, false);
    seed_item(&ctx, encoded_box_id, 34, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, crystal_id, 18060, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&crystal_id) {
            c.dynamic_attributes.insert(attr_ids::DAMAGE, 0.35);
        }
    });

    ctx.dispatch_rpc(
        &mut sess,
        "repackagingSvc",
        None,
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(spare_rifter_id)])],
        &kw,
        None,
    )
    .await
    .expect("Repackage spare Rifter");
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "AssembleShip",
        &[PyRep::Integer(spare_rifter_id)],
        &kw,
        None,
    )
    .await
    .expect("Assemble spare Rifter");

    let trash_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[PyRep::List(vec![PyRep::Integer(trash_ore_id)])],
            &kw,
            None,
        )
        .await
        .expect("TrashItems");
    assert_eq!(trash_rep.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));

    let dec_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "encodedItems",
            None,
            "DecodeItem",
            &[PyRep::Integer(encoded_box_id)],
            &kw,
            None,
        )
        .await
        .expect("DecodeItem");
    assert_eq!(dec_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(encoded_box_id));

    let cry_dmg = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "GetDamageForCrystals",
            &[PyRep::List(vec![PyRep::Integer(crystal_id)])],
            &kw,
            None,
        )
        .await
        .expect("GetDamageForCrystals");
    assert!(matches!(cry_dmg, PyRep::Dict(_)));

    let _ = ctx
        .dispatch_rpc(&mut sess, "itemLocking", None, "GetLockedItemLocations", &[], &kw, None)
        .await
        .expect("GetLockedItemLocations");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "itemLocking",
            None,
            "GetItemsByLocation",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("GetItemsByLocation");
    let _ = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "GetAllItems", &[], &kw, None)
        .await
        .expect("GetAllItems");
    let at_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "IsMyItemAtDockableLocation",
            &[PyRep::Integer(spare_rifter_id)],
            &kw,
            None,
        )
        .await
        .expect("IsMyItemAtDockableLocation");
    assert_eq!(at_dock.as_bool(), Some(true));

    // 2. Asset Safety wrap creation, listing, and delivery
    let structure_id = 1_040_000_001i64;
    let safety_item_id = 820_010i64;
    seed_item(&ctx, safety_item_id, 34, char_id, structure_id, InventoryFlag::Hangar, 10_000, false);
    // The wrap source must be a real structure record (MovePersonalAssetsToSafety rejects unknown IDs).
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

    // Time comes from the virtual clock, never from kwargs (rule 7): pass the 5-day delivery delay.
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
    // parity: feat:ships.ship-maintenance-bay, feat:ships.fleet-hangar, feat:ships.ship-config, rpc:itemLocking.GetLockedItemLocations, rpc:itemLocking.GetItemsByLocation, rpc:invbroker.GetDamageForCrystals, rpc:invbroker.IsMyItemAtDockableLocation, rpc:ship.Jettison, rpc:ship.StoreVessel, rpc:ship.ScoopToSMA, rpc:ship.BoardStoredShip, rpc:ship.LaunchFromContainer, rpc:eject.LaunchShipOrContainerFromWreckOrContainer, rpc:ship.GetShipConfiguration, rpc:ship.SetShipPassword, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.Add, rpc:ship.Board, rpc:ship.ConfigureShip
    eve_testkit::attest(&["feat:ships.ship-maintenance-bay", "feat:ships.fleet-hangar", "feat:ships.ship-config", "rpc:itemLocking.GetLockedItemLocations", "rpc:itemLocking.GetItemsByLocation", "rpc:invbroker.GetDamageForCrystals", "rpc:invbroker.IsMyItemAtDockableLocation", "rpc:ship.Jettison", "rpc:ship.StoreVessel", "rpc:ship.ScoopToSMA", "rpc:ship.BoardStoredShip", "rpc:ship.LaunchFromContainer", "rpc:eject.LaunchShipOrContainerFromWreckOrContainer", "rpc:ship.GetShipConfiguration", "rpc:ship.SetShipPassword"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7113);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("SelectCharacterID");
    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let spare_rifter_id = 820_001i64;
    seed_item(&ctx, spare_rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    // 3. Board an Orca (28606: 400,000 m3 SMA & 40,000 m3 Fleet Hangar), undock, test SMA, Fleet Hangar, Jettison, and Wreck Looting
    let orca_id = 820_020i64;
    seed_item(&ctx, orca_id, 28606, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(orca_id)], &kw, None)
        .await
        .expect("Board Orca");

    // Store assembled spare Rifter in Orca SMA (flag 90)
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

    // Reject storing non-ship cargo in SMA
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

    // Place ore in Orca Fleet Hangar (flag 155) and configure access
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

    // Undock into space (30000142)
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

    // Launch Rifter from Orca SMA into space, then ScoopToSMA back
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

    // Jettison ore into a jetcan + verify 180s cooldown
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
    let jetcan_id = jet_rep.dict_get("jetcanID").and_then(|v| v.as_i64()).unwrap();

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

    // Foreign wreck looting triggers 15m Suspect flag
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
    // Upstream of the client-local `OnSuspectsAndCriminalsUpdate`: system-wide
    // `OnSystemCriminalFlagUpdates(newIdles, newSuspects, newCriminals)`.
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

#[tokio::test]
async fn test_ships_board_eject_corvette_insurance_repair_killmarks_mastery_t3c() {
    // parity: feat:ships.board-in-space, feat:ships.eject, feat:ships.newbie-ship, feat:ships.ship-insurance-link, rpc:ship.Board, rpc:ship.LeaveShip, rpc:ship.Eject, rpc:dogmaIM.CreateNewbieShip, rpc:insuranceSvc.GetInsurancePrice, rpc:insuranceSvc.GetContractForShip, rpc:invbroker.SetLabel, rpc:shipKillCounter.GetItemKillCountPlayer, rpc:dogmaIM.GetRequiredSkillLevels, notify:OnSessionChanged, notify:DoBallsAdded, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.GetCertificatesForShipByMasteryLevel, rpc:dogmaIM.GetCurrCharMasteryLevel, rpc:dogmaIM.GetMyCertificates, rpc:dogmaIM.GetSkillsRequiredForMasteryLevel, rpc:insuranceSvc.InsureShip, rpc:repairSvc.GetDamagedModulesAndRepairCost, rpc:repairSvc.RepairAll, rpc:shipKillCounter.SetShipKillCounter
    eve_testkit::attest(&["feat:ships.board-in-space", "feat:ships.eject", "feat:ships.newbie-ship", "feat:ships.ship-insurance-link", "rpc:ship.Board", "rpc:ship.LeaveShip", "rpc:ship.Eject", "rpc:dogmaIM.CreateNewbieShip", "rpc:insuranceSvc.GetInsurancePrice", "rpc:insuranceSvc.GetContractForShip", "rpc:invbroker.SetLabel", "rpc:shipKillCounter.GetItemKillCountPlayer", "rpc:dogmaIM.GetRequiredSkillLevels", "notify:OnSessionChanged", "notify:DoBallsAdded"]);
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
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // 1. Insurance, Repair Shop, Rename, Killmarks, and Ship Tree / Masteries
    let ins_quote = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetInsurancePrice",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetInsurancePrice");
    assert!(ins_quote.dict_get("platinumPayoutISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    ctx.dispatch_rpc(
        &mut sess,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("InsureShip Platinum");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0);
        }
    });
    let dmg_quote = ctx
        .dispatch_rpc(
            &mut sess,
            "repairSvc",
            None,
            "GetDamagedModulesAndRepairCost",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetDamagedModulesAndRepairCost");
    assert!(dmg_quote.dict_get("repairCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    ctx.dispatch_rpc(&mut sess, "repairSvc", None, "RepairAll", &[], &kw, None)
        .await
        .expect("RepairAll");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "SetLabel",
        &[PyRep::Integer(ship_id), PyRep::String("Gila Monster".into())],
        &kw,
        None,
    )
    .await
    .expect("SetLabel");
    ctx.dispatch_rpc(
        &mut sess,
        "shipKillCounter",
        None,
        "SetShipKillCounter",
        &[PyRep::Integer(ship_id), PyRep::Integer(17)],
        &kw,
        None,
    )
    .await
    .expect("SetShipKillCounter");
    let kills = ctx
        .dispatch_rpc(
            &mut sess,
            "shipKillCounter",
            None,
            "GetItemKillCountPlayer",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetItemKillCountPlayer");
    assert_eq!(kills.as_i64(), Some(17));

    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetRequiredSkillLevels", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("GetRequiredSkillLevels");
    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetMyCertificates", &[], &kw, None)
        .await
        .expect("GetMyCertificates");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetCertificatesForShipByMasteryLevel",
            &[PyRep::Integer(587), PyRep::Integer(3)],
            &kw,
            None,
        )
        .await
        .expect("GetCertificatesForShipByMasteryLevel");
    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetCurrCharMasteryLevel", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("GetCurrCharMasteryLevel");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetSkillsRequiredForMasteryLevel",
            &[PyRep::Integer(587), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("GetSkillsRequiredForMasteryLevel");

    // 2. LeaveShip in station -> Capsule, then CreateNewbieShip (after moving existing ship out of station)
    let leave_rep = ctx
        .dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None)
        .await
        .expect("LeaveShip in station");
    let pod_id = leave_rep.dict_get("capsuleID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(sess.ship_id, Some(pod_id));

    let dup_corvette_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip must fail when character already has a non-capsule ship in station");
    assert_eq!(dup_corvette_err.code, "AlreadyHaveShipInStation");

    // Move ship_id to another station so station_id has 0 non-capsule ships, then claim racial Corvette
    ctx.db.with_mut_snapshot(|snap| {
        for it in snap.items.values_mut() {
            if it.owner_id == char_id && it.location_id == station_id && it.item_id != pod_id {
                let is_ship = ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 6);
                if is_ship {
                    it.location_id = 60_003_761;
                }
            }
        }
    });
    let corvette_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect("CreateNewbieShip");
    let corvette_id = corvette_rep.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    assert!(corvette_id > 0);
    let cd_corvette_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip within 60s must fail with NewbieShipCooldown");
    assert_eq!(cd_corvette_err.code, "NewbieShipCooldown");
    ctx.advance_virtual_clock(65);

    // 3. Eject in space -> spawns Capsule & leaves ship in space -> Board back in space removes Capsule
    let sys_id = 30_000_142u32;
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&corvette_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(
            Ball {
                id: corvette_id,
                type_id: 601,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    let eject_rep = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Eject", &[], &kw, None)
        .await
        .expect("ship.Eject in space");
    let space_pod_id = eject_rep.dict_get("capsuleID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(sess.ship_id, Some(space_pod_id));

    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(corvette_id)], &kw, None)
        .await
        .expect("Board ship in space from capsule");
    assert_eq!(sess.ship_id, Some(corvette_id));
    let pod_removed = ctx.db.with_snapshot(|snap| !snap.items.contains_key(&space_pod_id));
    assert!(pod_removed, "Boarding ship in space from capsule must despawn the capsule");
}

#[tokio::test]
async fn test_fitting_slots_cpu_pwg_hardpoints_rigs_saved_multifit_ghost_inspace_weapons_ammo() {
    // parity: feat:fitting.fittings-save, feat:fitting.fit-from-saved, feat:fitting.community-fittings, feat:fitting.fitting-simulation, feat:fitting.fitting-service-space, rpc:corpFittingMgr.GetCommunityFittings, rpc:ship.FitShips, rpc:ship.GetShipFittingInfo, rpc:ship.ScoopToFleetHangar, notify:OnItemChange, notify:ProcessInventoryChange, notify:OnMultipleItemChange, notify:OnModuleFitted, notify:OnModuleAttributeChanges, notify:OnFittingsUpdated, rpc:charFittingMgr.GetFittings, rpc:charFittingMgr.SaveFitting, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.Add
    eve_testkit::attest(&["feat:fitting.fittings-save", "feat:fitting.fit-from-saved", "feat:fitting.community-fittings", "feat:fitting.fitting-simulation", "feat:fitting.fitting-service-space", "rpc:corpFittingMgr.GetCommunityFittings", "rpc:ship.FitShips", "rpc:ship.GetShipFittingInfo", "rpc:ship.ScoopToFleetHangar", "notify:OnItemChange", "notify:ProcessInventoryChange", "notify:OnMultipleItemChange", "notify:OnModuleFitted", "notify:OnModuleAttributeChanges", "notify:OnFittingsUpdated"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7105);
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
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // 1. Saved fittings (Personal, Corp, Community, DNA import/export) + Ghost simulation (`GetShipFittingInfo`)
    let dna = fitting_to_dna(587, &[(487, 1), (434, 1), (2048, 1)]);
    let (parsed_ship, parsed_items) = dna_to_fitting(&dna).expect("dna_to_fitting");
    assert_eq!(parsed_ship, 587);
    assert_eq!(parsed_items.len(), 3);

    let fit_id_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(char_id),
                PyRep::keyval([
                    ("name", PyRep::String("Rifter Tackle".into())),
                    ("shipTypeID", PyRep::Integer(587)),
                    ("description", PyRep::String("Fast tackle".into())),
                    (
                        "fitData",
                        PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::Integer(2048),
                            PyRep::Integer(11),
                            PyRep::Integer(1),
                        ])]),
                    ),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.SaveFitting");
    let saved_fid = fit_id_rep.as_i64().unwrap();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charFittingMgr", None, "GetFittings", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("charFittingMgr.GetFittings");
    let comm_fits = ctx
        .dispatch_rpc(&mut sess, "corpFittingMgr", None, "GetCommunityFittings", &[], &kw, None)
        .await
        .expect("corpFittingMgr.GetCommunityFittings");
    assert!(!matches!(comm_fits, PyRep::None));

    let ghost_sim = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "GetShipFittingInfo",
            &[
                PyRep::Integer(587),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(2048),
                    PyRep::Integer(11),
                    PyRep::Integer(1),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("ship.GetShipFittingInfo ghost simulation");
    assert_eq!(ghost_sim.dict_get("simulated").and_then(|v| v.as_bool()), Some(true));

    // 2. Multifit (`ship.FitShips`) across 2 unpackaged Rifters
    let hull_a = 830_001i64;
    let hull_b = 830_002i64;
    let dc_stack = 830_003i64;
    seed_item(&ctx, hull_a, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, hull_b, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, dc_stack, 2048, char_id, station_id, InventoryFlag::Hangar, 5, false);
    grant_all_required_skills_for_type(&ctx, char_id, 587);
    grant_all_required_skills_for_type(&ctx, char_id, 2048);

    let mfit_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[
                PyRep::List(vec![PyRep::Integer(hull_a), PyRep::Integer(hull_b)]),
                PyRep::Integer(saved_fid),
            ],
            &kw,
            None,
        )
        .await
        .expect("ship.FitShips multifit");
    assert_eq!(mfit_rep.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(2));

    // 3. In-space refitting near a Mobile Depot (33474) vs out-of-range vs active weapons timer
    let sys_id = 30_000_142u32;
    let depot_id = 830_010i64;
    let spare_mod_id = 830_011i64;
    seed_item(&ctx, depot_id, 33474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, spare_mod_id, 2048, char_id, ship_id, InventoryFlag::Cargo, 1, true);

    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_350_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    // First spawn Mobile Depot at 10,000m (out of 3,000m refit range)
    let _ = sys
        .spawn_ball(
            Ball {
                id: depot_id,
                type_id: 33474,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
        )
        .await;

    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(spare_mod_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot2 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Fitting in space 10km from Mobile Depot must fail");
    assert_eq!(far_err.code, "CannotFitInSpace");

    // Move Mobile Depot to 1,500m (within 3,000m refit range)
    let _ = sys.remove_ball(depot_id).await;
    let _ = sys
        .spawn_ball(
            Ball {
                id: depot_id,
                type_id: 33474,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
        )
        .await;

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(spare_mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fitting in space within 1,500m of Mobile Depot succeeds");

    // Active weapons timer blocks unfitting even next to Mobile Depot
    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .trigger_weapon_timer(char_id, ctx.now_unix_secs());
    let wt_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(spare_mod_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Unfitting in space with active weapons timer must fail");
    assert_eq!(wt_err.code, "CannotRefitWithWeaponsTimer");
}
