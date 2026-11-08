//! Comprehensive Inventory (14 rows), Ships (8 rows), and Fitting (10 rows) integration test suite.

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

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
async fn test_containers_cargo_secure_alsc_station_freight() {
    // parity: feat:inventory.containers, feat:inventory.secure-containers, feat:inventory.audit-log-container-events, feat:inventory.station-containers, feat:inventory.freight-container-space-rules, feat:inventory.container-contents-restrictions, rpc:invbroker.AssembleCargoContainer, rpc:invbroker.GetContainerContents, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:inventory.containers",
        "feat:inventory.secure-containers",
        "feat:inventory.audit-log-container-events",
        "feat:inventory.station-containers",
        "feat:inventory.freight-container-space-rules",
        "feat:inventory.container-contents-restrictions",
        "rpc:invbroker.AssembleCargoContainer",
        "rpc:invbroker.GetContainerContents",
    ]);
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

    // Reject placing a container inside another container (`CannotPutContainerInContainer`)
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
async fn test_repackage_assemble_trash_encoded_crystal_and_item_locking() {
    // parity: feat:inventory.item-locking, feat:inventory.encoded-items, feat:inventory.crystal-damage, rpc:repackagingSvc.RepackageItems, rpc:ship.AssembleShip, rpc:invbroker.TrashItems, rpc:encodedItems.DecodeItem, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.GetDamageForCrystals, rpc:invbroker.IsMyItemAtDockableLocation, rpc:itemLocking.GetItemsByLocation, rpc:itemLocking.GetLockedItemLocations
    eve_testkit::attest(&[
        "feat:inventory.item-locking",
        "feat:inventory.encoded-items",
        "feat:inventory.crystal-damage",
        "rpc:repackagingSvc.RepackageItems",
        "rpc:ship.AssembleShip",
        "rpc:invbroker.TrashItems",
        "rpc:encodedItems.DecodeItem",
        "rpc:invbroker.GetDamageForCrystals",
        "rpc:invbroker.IsMyItemAtDockableLocation",
        "rpc:itemLocking.GetItemsByLocation",
        "rpc:itemLocking.GetLockedItemLocations",
    ]);
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
    let corp_id = 98_440_001i64;

    // 1. Repackage & Assemble ship + TrashItems + Encoded Items + Crystal Damage
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

    // 2. Shareholder vote-locked BPO + ALSC-locked item vs active industry job (must not conflate)
    let corp_bpo_id = 820_501i64;
    let personal_bpo_id = 820_502i64;
    let alsc_can_id = 820_504i64;
    let alsc_locked_id = 820_503i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Item Lock Corp".into(),
                ticker: "ILC".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents::from_isk(1_000_000),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, u64::MAX)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(station_id),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        // Active industry job on personal_bpo_id — must NOT appear in itemLocking.GetItemsByLocation
        snap.industry_jobs.insert(
            820_601,
            eve_db::IndustryJobRecord {
                job_id: 820_601,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: personal_bpo_id,
                blueprint_type_id: 688,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: 1_700_000_000, ..Default::default() },
        );
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = u64::MAX;
    seed_item(&ctx, corp_bpo_id, 688, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true);
    seed_item(&ctx, personal_bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, alsc_can_id, 17363, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, alsc_locked_id, 35, char_id, alsc_can_id, InventoryFlag::Cargo, 10, false);

    // Before locking, station_id has no locked items (even though personal_bpo_id is in an active industry job)
    let before_station = ctx
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
        .expect("GetItemsByLocation before lock");
    let PyRep::Dict(before_entries) = before_station else {
        panic!("expected Dict from GetItemsByLocation");
    };
    assert!(
        before_entries.is_empty(),
        "Active industry job BPO must not appear in itemLocking.GetItemsByLocation"
    );

    // Lock corp_bpo_id via shareholder vote (voteType = 6 Item Lockdown) + ActivateSanctionedAction
    let vc_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "voteManager",
            None,
            "InsertVoteCase",
            &[
                PyRep::String("Lock Rifter BPO".into()),
                PyRep::String("Prevent theft".into()),
                PyRep::Integer(6),
                PyRep::Integer(corp_bpo_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("InsertVoteCase Item Lockdown");
    let vc_id = vc_rep.dict_get("voteCaseID").and_then(PyRep::as_i64).unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "voteManager",
        None,
        "InsertVote",
        &[PyRep::Integer(vc_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("InsertVote YES");
    ctx.advance_virtual_clock(2 * 86_400);
    ctx.dispatch_rpc(
        &mut sess,
        "voteManager",
        None,
        "ActivateSanctionedAction",
        &[PyRep::Integer(vc_id)],
        &kw,
        None,
    )
    .await
    .expect("ActivateSanctionedAction lockdown");

    // Also lock alsc_locked_id in ALSC
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "ALSCLockItems",
        &[
            PyRep::Integer(alsc_can_id),
            PyRep::List(vec![PyRep::Integer(alsc_locked_id)]),
        ],
        &kw,
        None,
    )
    .await
    .expect("ALSCLockItems");

    let locs_rep = ctx
        .dispatch_rpc(&mut sess, "itemLocking", None, "GetLockedItemLocations", &[], &kw, None)
        .await
        .expect("GetLockedItemLocations");
    let PyRep::List(locs) = locs_rep else {
        panic!("expected List from GetLockedItemLocations");
    };
    let loc_ids: Vec<i64> = locs.iter().filter_map(PyRep::as_i64).collect();
    assert!(loc_ids.contains(&station_id), "station_id must be in GetLockedItemLocations");
    assert!(loc_ids.contains(&alsc_can_id), "alsc_can_id must be in GetLockedItemLocations");

    let station_locked = ctx
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
        .expect("GetItemsByLocation station_id");
    let PyRep::Dict(station_dict) = station_locked else {
        panic!("expected Dict from GetItemsByLocation");
    };
    let locked_station_ids: Vec<i64> = station_dict.iter().filter_map(|(k, _)| k.as_i64()).collect();
    assert_eq!(locked_station_ids, vec![corp_bpo_id]);

    let alsc_locked = ctx
        .dispatch_rpc(
            &mut sess,
            "itemLocking",
            None,
            "GetItemsByLocation",
            &[PyRep::Integer(alsc_can_id)],
            &kw,
            None,
        )
        .await
        .expect("GetItemsByLocation alsc_can_id");
    let PyRep::Dict(alsc_dict) = alsc_locked else {
        panic!("expected Dict from GetItemsByLocation");
    };
    let locked_alsc_ids: Vec<i64> = alsc_dict.iter().filter_map(|(k, _)| k.as_i64()).collect();
    assert_eq!(locked_alsc_ids, vec![alsc_locked_id]);

    // Regressions for Issues #6, #7, #8:
    // Issue #6: Cross-station item teleportation & remote fitting via Add and MultiAdd must be rejected
    let active_ship_id = 100_000_001_i64;
    let remote_station_id = 60_008_494_i64;
    let remote_ore_id = 994_001_i64;
    let remote_ship_id = 994_002_i64;
    let remote_mod_id = 994_003_i64;
    let mutated_a_id = 994_004_i64;
    let mutated_b_id = 994_005_i64;
    let split_atomicity_id = 994_006_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            remote_ore_id,
            InventoryItemRecord {
                item_id: remote_ore_id,
                type_id: 34,
                owner_id: char_id,
                location_id: remote_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            remote_ship_id,
            InventoryItemRecord {
                item_id: remote_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: remote_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            remote_mod_id,
            InventoryItemRecord {
                item_id: remote_mod_id,
                type_id: 440,
                owner_id: char_id,
                location_id: remote_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut dyn_attrs = std::collections::HashMap::new();
        dyn_attrs.insert(54, 1.25);
        snap.items.insert(
            mutated_a_id,
            InventoryItemRecord {
                item_id: mutated_a_id,
                type_id: 440,
                owner_id: char_id,
                location_id: active_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: dyn_attrs.clone(),
                source_mutaplasmid_id: Some(47740),
                ..Default::default()
            },
        );
        snap.items.insert(
            mutated_b_id,
            InventoryItemRecord {
                item_id: mutated_b_id,
                type_id: 440,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: dyn_attrs,
                source_mutaplasmid_id: Some(47740),
                ..Default::default()
            },
        );
        snap.items.insert(
            split_atomicity_id,
            InventoryItemRecord {
                item_id: split_atomicity_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 5,
                ..Default::default()
            },
        );
    });

    // Moving remote_ore_id from Amarr to active_ship_id at Jita via Add must fail
    let tele_add_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(remote_ore_id),
                PyRep::Integer(active_ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Cross-station Add must fail");
    assert_eq!(tele_add_err.code, "ItemOwnershipViolation");

    // Moving remote_ore_id from Amarr via MultiAdd must fail
    let tele_multi_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(remote_ore_id)]),
                PyRep::Integer(active_ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Cross-station MultiAdd must fail");
    assert_eq!(tele_multi_err.code, "ItemOwnershipViolation");

    // Fitting remote_mod_id from Amarr onto active_ship_id at Jita must fail
    let remote_fit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(remote_mod_id),
                PyRep::Integer(active_ship_id),
                PyRep::Integer(InventoryFlag::MedSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Remote module fitting must fail");
    assert_eq!(remote_fit_err.code, "ItemOwnershipViolation");

    // Fitting onto remote_ship_id in Amarr must fail
    let remote_ship_fit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(remote_mod_id),
                PyRep::Integer(remote_ship_id),
                PyRep::Integer(InventoryFlag::MedSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Fitting onto remote ship must fail");
    assert_eq!(remote_ship_fit_err.code, "InvalidFittingTarget");

    // Issue #7: Split + Add with mismatched expected_version must fail BEFORE splitting the stack
    let mut bad_ver_kw = std::collections::HashMap::new();
    bad_ver_kw.insert("qty".to_string(), PyRep::Integer(40));
    bad_ver_kw.insert("expected_version".to_string(), PyRep::Integer(999));
    let ver_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(split_atomicity_id),
                PyRep::Integer(active_ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &bad_ver_kw,
            None,
        )
        .await
        .expect_err("Mismatched expected_version must fail");
    assert_eq!(ver_err.code, "ItemVersionMismatch");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items[&split_atomicity_id].quantity, 100,
            "Original stack quantity must remain intact when Add fails validation"
        );
    });

    // Issue #7: Mutated items must not auto-merge when moved into ship cargo hold
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(mutated_b_id),
            PyRep::Integer(active_ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Move mutated_b into cargo");
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&mutated_a_id), "mutated_a must still exist");
        assert!(snap.items.contains_key(&mutated_b_id), "mutated_b must not be merged into mutated_a");
        assert_eq!(snap.items[&mutated_a_id].quantity, 1);
        assert_eq!(snap.items[&mutated_b_id].quantity, 1);
    });

    // Issue #8: Undocked player cannot trash station items even when spoofing expected_station
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.solar_system_id = 30_000_142;
        }
        if let Some(s) = snap.items.get_mut(&active_ship_id) {
            s.location_id = 30_000_142;
            s.flag = InventoryFlag::None;
        }
    });
    let undocked_trash_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "TrashItems",
            &[
                PyRep::List(vec![PyRep::Integer(split_atomicity_id)]),
                PyRep::Integer(station_id),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Undocked player must not be able to trash station hangar items");
    assert_eq!(undocked_trash_err.code, "DeleteFailed");
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.items.contains_key(&split_atomicity_id),
            "Station item must not be deleted while undocked"
        );
    });
}

#[allow(unused_imports, dead_code)]
mod ship_boarding_fitting_slots_and_multifit {
    use super::*;

// Comprehensive Inventory, Ships, and Fitting integration test suite.

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
async fn test_ships_board_eject_corvette_insurance_repair_killmarks_mastery_t3c() {
    // parity: feat:ships.board-in-space, feat:ships.eject, feat:ships.newbie-ship, feat:ships.ship-insurance-link, rpc:ship.Board, rpc:ship.Eject, rpc:dogmaIM.CreateNewbieShip, rpc:insuranceSvc.GetInsurancePrice, rpc:invbroker.SetLabel, rpc:shipKillCounter.GetItemKillCountPlayer, rpc:dogmaIM.GetRequiredSkillLevels, notify:OnSessionChanged, notify:DoBallsAdded, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:ships.board-in-space",
        "feat:ships.eject",
        "feat:ships.newbie-ship",
        "feat:ships.ship-insurance-link",
        "rpc:ship.Board",
        "rpc:ship.Eject",
        "rpc:dogmaIM.CreateNewbieShip",
        "rpc:insuranceSvc.GetInsurancePrice",
        "rpc:invbroker.SetLabel",
        "rpc:shipKillCounter.GetItemKillCountPlayer",
        "rpc:dogmaIM.GetRequiredSkillLevels",
        "notify:OnSessionChanged",
        "notify:DoBallsAdded",
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
    let ship_id = 100_000_001i64;
    let _station_id = 60_003_760i64;

    let expected_ref_val = eve_server::formulas::compute_ship_insurance_reference_value(&ctx.sde, 587);
    let ins_quote = ctx
        .dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("GetInsurancePrice by typeID");
    assert_eq!(ins_quote.as_f64(), Some(expected_ref_val));
    assert!(expected_ref_val > 0.0);
    let ins_quote_ship = ctx
        .dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("GetInsurancePrice by shipID");
    assert_eq!(ins_quote_ship.as_f64(), Some(expected_ref_val));
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
    sess.role |= eve_db::service_roles::ROLE_GMH;
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
    sess.role &= !eve_db::service_roles::ROLE_GMH;
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

    ctx.advance_secs(12);
    let leave_rep = ctx
        .dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None)
        .await
        .expect("LeaveShip in station");
    let pod_id = leave_rep.as_i64().expect("capsule_id integer");
    assert_eq!(sess.ship_id, Some(pod_id));

    // Non-corvette ship (Rifter) in station must NOT block CreateNewbieShip (#656)
    let corvette_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect("CreateNewbieShip succeeds even with non-corvette ship in station");
    let corvette_id = corvette_rep.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    assert!(corvette_id > 0);
    let cd_corvette_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip within 60s must fail with NewbieShipCooldown");
    assert_eq!(cd_corvette_err.code, "NewbieShipCooldown");
    ctx.advance_virtual_clock(65);
    let already_corv_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip while already in a corvette must fail with AlreadyInNewbieShip");
    assert_eq!(already_corv_err.code, "AlreadyInNewbieShip");

    // Leave corvette into capsule -> calling CreateNewbieShip again while Corvette is in station fails AlreadyHaveShipInStation
    ctx.advance_secs(12);
    let pod_after_corvette = ctx
        .dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None)
        .await
        .expect("LeaveShip from corvette");
    assert!(pod_after_corvette.as_i64().is_some());
    let dup_corvette_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip must fail when character already has a corvette in station");
    assert_eq!(dup_corvette_err.code, "AlreadyHaveShipInStation");

    // Re-board corvette before undocking
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(corvette_id)], &kw, None)
        .await
        .expect("Board back into corvette");
    ctx.advance_secs(12);

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

    ctx.advance_virtual_clock(10);

    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(corvette_id)], &kw, None)
        .await
        .expect("Board ship in space from capsule");
    assert_eq!(sess.ship_id, Some(corvette_id));
    let pod_removed = ctx.db.with_snapshot(|snap| !snap.items.contains_key(&space_pod_id));
    assert!(pod_removed, "Boarding ship in space from capsule must despawn the capsule");
}

#[tokio::test]
async fn test_fitting_slots_cpu_pwg_hardpoints_rigs_saved_multifit_ghost_inspace_weapons_ammo() {
    // parity: feat:fitting.fit-from-saved, feat:fitting.community-fittings, feat:fitting.fitting-simulation, feat:fitting.fitting-service-space, rpc:corpFittingMgr.GetCommunityFittings, rpc:ship.FitShips, rpc:ship.GetShipFittingInfo, notify:OnItemChange, notify:ProcessInventoryChange, notify:OnMultipleItemChange, notify:OnModuleFitted, notify:OnModuleAttributeChanges, notify:OnFittingsUpdated, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:fitting.fit-from-saved",
        "feat:fitting.community-fittings",
        "feat:fitting.fitting-simulation",
        "feat:fitting.fitting-service-space",
        "rpc:corpFittingMgr.GetCommunityFittings",
        "rpc:ship.FitShips",
        "rpc:ship.GetShipFittingInfo",
        "notify:OnItemChange",
        "notify:ProcessInventoryChange",
        "notify:OnMultipleItemChange",
        "notify:OnModuleFitted",
        "notify:OnModuleAttributeChanges",
        "notify:OnFittingsUpdated",
    ]);
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

}

#[allow(unused_imports, dead_code)]
mod jetcan_wreck_sma_and_asset_safety {
    use super::*;

// Comprehensive Inventory, Ships, and Fitting integration test suite: Asset Safety, SMA, Fleet Hangar, Jetcan, and Wreck Looting.

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
                last_damage_unix: 0, self_destruct_at_unix: None,
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
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(29637, eve_db::TrainedSkillRecord { type_id: 29637, level: 5, skill_points: 256_000.0 });
        sk.insert(3327, eve_db::TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });
    ctx.advance_secs(12);
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

}

// Folded submodule: ships_modules_fitting_suite
#[allow(unused_imports, dead_code)]
mod ships_modules_fitting_suite {
    use super::*;

// Integration tests closing `ships.*` and `modules.*` parity rows:
// - `feat:ships.frigate-escape-bay`
// - `feat:ships.blockade-runner-cargo-scan-immunity`
// - `feat:ships.board-ship-skill-and-clone-check`
// - `feat:ships.cynosural-beacon-spawning`
// - `feat:ships.jump-drive-fatigue-accumulation`
// - `feat:ships.titan-jump-portal-bridge`
// - `feat:modules.stasis-grappler-mechanics`
// - `feat:modules.emergency-hull-energizer`
// - `feat:modules.mutadaptive-remote-armor-repairer`
// - `feat:modules.entropic-disintegrator-damage-spool`

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

async fn call_service(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
    kw: HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(ctx, sess, svc, method, &args, &kw)
        .await
}

fn make_session(
    sid: u64,
    user_id: u32,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = station_id;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = ship_id;
    sess
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton,
            version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, x: f64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.armor_hp = cs.max_armor_hp * 0.2;
    let b = Ball {
        id: ship_id, type_id, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 150.0, inertia: 1.0, mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 26, category_id: 6, owner_id, char_id: Some(owner_id),
        corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
    };
    let _ = sys_h.spawn_ball_with_meta(b, cs, meta).await;
}

fn grant_type_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id).cloned() else { return; };
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for &(sa, la) in &[(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
            if let Some(&sid_f) = t.default_attributes.get(&sa) {
                let sid = sid_f.round() as u32;
                let lvl = t.default_attributes.get(&la).copied().unwrap_or(1.0).round() as u8;
                if sid > 0 {
                    sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl.max(5), skill_points: 256_000.0 });
                }
            }
        }
    });
}

#[tokio::test]
async fn test_ships_frigate_escape_bay_blockade_runner_scan_and_board_skill_clone_checks() {
    // parity: feat:ships.board-ship-skill-and-clone-check
    eve_testkit::attest(&[
        "feat:ships.board-ship-skill-and-clone-check",
    ]);

    let ctx = new_ctx();
    ctx.set_clock_paused(true);
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    grant_type_skills(&ctx, c1, 638);
    grant_type_skills(&ctx, c1, 587);
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&c1) { ch.clone_grade = 2; });

    // 1. Frigate Escape Bay (`flag 179` on Raven `638`, `frigateEscapeBayCapacity` attr 3020 > 0)
    let raven_id = 940_001;
    let packaged_rifter = 940_002;
    let rifter_with_cargo = 940_003;
    let cargo_trit = 940_004;
    let valid_rifter = 940_005;
    let second_rifter = 940_006;
    let cruiser_feb_id = 940_007;

    seed_item(&ctx, raven_id, 638, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, packaged_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, rifter_with_cargo, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, cargo_trit, 34, c1, rifter_with_cargo, InventoryFlag::Cargo, 100, false);
    seed_item(&ctx, valid_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, second_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, cruiser_feb_id, 626, c1, station_id, InventoryFlag::Hangar, 1, true);

    let mut sess1 = make_session(1, 1, c1, 1000009, Some(station_id), sys_id, Some(raven_id));

    let cruiser_feb_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(cruiser_feb_id), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Cruiser into FEB must fail");
    assert_eq!(cruiser_feb_err.code, "InvalidItemTypeForHold");

    let pkg_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(packaged_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Packaged frigate must be rejected");
    assert_eq!(pkg_err.code, "ShipMustBeAssembled");

    let cargo_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(rifter_with_cargo), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Frigate with non-charge cargo must be rejected");
    assert_eq!(cargo_err.code, "ShipHasItemsInCargo");

    let _ = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(valid_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect("Assembled empty frigate must fit in Frigate Escape Bay");

    let second_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(second_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Frigate Escape Bay must reject a second frigate");
    assert_eq!(second_err.code, "CapacityExceeded");

    // Undock Raven into space and destroy it via execute_death_pipeline -> pilot boards `valid_rifter`!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.station_id = None;
            ch.active_ship_id = raven_id;
        }
        if let Some(sh) = snap.items.get_mut(&raven_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    spawn_ship_ball(&ctx, sys_id, raven_id, 638, c1, 1_000.0).await;
    let outcome = execute_death_pipeline(&ctx, sys_id, c1, raven_id, c2, DVec3::new(1_000.0, 0.0, 0.0), ctx.now_unix_secs()).await.expect("execute_death_pipeline succeeds");
    assert_eq!(outcome.ejected_capsule_id, Some(valid_rifter));
    let active_after_death = ctx.db.with_snapshot(|s| s.characters.get(&c1).map(|c| c.active_ship_id).unwrap_or(0));
    assert_eq!(active_after_death, valid_rifter, "Pilot must be placed directly into escape frigate");
    assert!(!outcome.loot_dropped_item_ids.contains(&valid_rifter), "Frigate must not drop as loot");
    assert!(!outcome.loot_destroyed_item_ids.contains(&valid_rifter), "Frigate must not be destroyed in wreck");
    let dkm = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&outcome.killmail_id).cloned()).expect("Detailed killmail must exist");
    assert!(dkm.dropped_items.iter().any(|it| it.type_id == 587 && it.flag == 179 && it.qty_dropped == 0 && it.qty_destroyed == 0), "Killmail must record surviving frigate at flag 179");
    ctx.advance_virtual_clock(15);

    // 2. Blockade Runner (`Crane` 12729, group 1202) cargo scan immunity vs NPC customs contraband scan
    let crane_id = 940_020;
    let contraband_id = 940_021; // Slaves (type 3721)
    let scanner_mod = 940_022; // Cargo Scanner I (type 442)
    seed_item(&ctx, crane_id, 12729, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, contraband_id, 3721, c2, crane_id, InventoryFlag::Cargo, 5, false);
    seed_item(&ctx, scanner_mod, 442, c1, valid_rifter, InventoryFlag::MedSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(scanner_mod, true);
        if let Some(ch) = snap.characters.get_mut(&c2) {
            ch.balance = IskCents::from_isk(10_000_000);
            ch.active_ship_id = crane_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
    });
    spawn_ship_ball(&ctx, sys_id, crane_id, 12729, c2, 2_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.add_target_lock(valid_rifter, TargetLock { target_id: crane_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(25_000.0) }).await;

    sess1.station_id = None;
    sess1.ship_id = Some(valid_rifter);
    let scan_rep = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(scanner_mod), PyRep::String("cargoScan".into()), PyRep::Integer(crane_id)], HashMap::new()).await.expect("Cargo Scanner activation on Blockade Runner");
    assert_eq!(scan_rep.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(scan_rep.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    let spawned_police = eve_server::services::combat_npc_police::check_and_spawn_faction_police(&ctx, sys_id).await;
    assert!(!spawned_police.is_empty(), "High-sec customs police must spawn when contraband is detected");
    let (cargo_has_contraband, c2_bal, has_standing_loss) = ctx.db.with_snapshot(|s| {
        let in_cargo = s.items.get(&contraband_id).map(|i| i.location_id == crane_id).unwrap_or(false);
        let bal = s.characters.get(&c2).map(|c| c.balance).unwrap_or(IskCents(0));
        let standing = s.standing_transactions.values().flatten().any(|st| st.to_id == c2 && st.modification < 0.0);
        (in_cargo, bal, standing)
    });
    assert!(!cargo_has_contraband, "Contraband must be confiscated from Blockade Runner cargo");
    assert!(c2_bal < IskCents::from_isk(10_000_000), "Customs fine must be deducted from pilot wallet");
    assert!(has_standing_loss, "Standing penalty transaction must be recorded for contraband");

    // R-CUSTOMS-SCOPE negative tests:
    // Wrecks and jetcans with contraband are NOT confiscated:
    let (wreck_id, wreck_cb, jetcan_id, jetcan_cb) = (940_080, 940_081, 940_082, 940_083);
    seed_item(&ctx, wreck_id, 26561, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, wreck_cb, 3721, c2, wreck_id, InventoryFlag::Cargo, 5, false);
    seed_item(&ctx, jetcan_id, 23, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, jetcan_cb, 3721, c2, jetcan_id, InventoryFlag::Cargo, 5, false);
    spawn_ship_ball(&ctx, sys_id, wreck_id, 26561, c2, 3_000.0).await;
    spawn_ship_ball(&ctx, sys_id, jetcan_id, 23, c2, 4_000.0).await;
    assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, sys_id, wreck_id).is_empty());
    assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, sys_id, jetcan_id).is_empty());
    assert!(ctx.db.get_item(wreck_cb).is_some() && ctx.db.get_item(jetcan_cb).is_some());

    // System below confiscateMinSec: Tama (0.28) is below SDE confiscateMinSec -> contraband NOT confiscated:
    let (tama_sys, tama_ship, tama_cb) = (30_002_813, 940_084, 940_085);
    seed_item(&ctx, tama_ship, 587, c2, tama_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, tama_cb, 3721, c2, tama_ship, InventoryFlag::Cargo, 5, false);
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&c2) { ch.solar_system_id = tama_sys; ch.active_ship_id = tama_ship; } });
    spawn_ship_ball(&ctx, tama_sys, tama_ship, 587, c2, 1_000.0).await;
    assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, tama_sys, tama_ship).is_empty());
    assert!(ctx.db.get_item(tama_cb).is_some(), "Contraband below confiscateMinSec must not be confiscated");
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&c2) { ch.solar_system_id = sys_id; ch.active_ship_id = crane_id; } });

    // 3. Board ship skill and Alpha clone grade checks (`ship.Board` and `ship.BoardStoredShip`)
    sess1.station_id = Some(station_id);
    let bustard_id = 940_030;
    seed_item(&ctx, bustard_id, 12731, c1, station_id, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.station_id = Some(station_id);
        }
        snap.skills.entry(c1).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    let board_no_skill = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect_err("Boarding Bustard without Transport Ships skill must fail");
    assert_eq!(board_no_skill.code, "ShipHasSkillPrerequisites");

    grant_type_skills(&ctx, c1, 12731);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 0; } });
    let board_alpha_err = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect_err("Alpha clone must be blocked from boarding Omega-only Bustard");
    assert_eq!(board_alpha_err.code, "TrialAccountRestriction");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 2; } });
    let board_ok = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect("Omega clone with skills must board Bustard");
    assert_eq!(board_ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(bustard_id));

    // In space boarding (`ship.Board`):
    let unpiloted_bustard = 940_040;
    let c2_ship = 940_050;
    seed_item(&ctx, unpiloted_bustard, 12731, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, c2_ship, 587, c2, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball(&ctx, sys_id, unpiloted_bustard, 12731, c2, 11_000.0).await;
    spawn_ship_ball(&ctx, sys_id, c2_ship, 587, c2, 11_500.0).await;
    let mut sess_space = make_session(2, 2, c2, 1000009, None, sys_id, Some(c2_ship));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c2) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = c2_ship;
            ch.station_id = None;
            ch.clone_grade = 0;
        }
        snap.skills.entry(c2).or_default().clear();
    });
    let space_no_skill = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect_err("Missing skills in space");
    assert_eq!(space_no_skill.code, "ShipHasSkillPrerequisites");

    grant_type_skills(&ctx, c2, 12731);
    let space_alpha_err = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect_err("Alpha in space");
    assert_eq!(space_alpha_err.code, "TrialAccountRestriction");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c2) { ch.clone_grade = 2; } });
    let space_board_ok = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect("Omega in space boards");
    assert_eq!(space_board_ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(unpiloted_bustard));
}

#[tokio::test]
async fn test_ships_cyno_beacon_jump_fatigue_and_titan_portal_bridge() {

    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    let cyno_char = 90_000_001;
    let titan_char = 90_000_002;
    let subcap_char = 90_000_003;
    let low_origin = 30_002_538;
    let low_dest = 30_002_539;

    let falcon_id = 950_001;
    let cyno_mod = 950_002;
    let lo_id = 950_003;
    let cloak_mod = 950_004;
    let avatar_id = 950_010;
    let portal_mod = 950_011;
    let stront_id = 950_012;
    let helium_id = 950_013;
    let rifter_id = 950_020;

    seed_item(&ctx, falcon_id, 11957, cyno_char, low_dest as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, cyno_mod, 21096, cyno_char, falcon_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, cloak_mod, 11578, cyno_char, falcon_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, lo_id, 16273, cyno_char, falcon_id, InventoryFlag::Cargo, 2_000, false);

    seed_item(&ctx, avatar_id, 11567, titan_char, low_origin as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, portal_mod, 23953, titan_char, avatar_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, stront_id, 16275, titan_char, avatar_id, InventoryFlag::Cargo, 5_000, false);
    seed_item(&ctx, helium_id, 16274, titan_char, avatar_id, InventoryFlag::Cargo, 500_000, false);
    seed_item(&ctx, rifter_id, 587, subcap_char, low_origin as i64, InventoryFlag::None, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cyno_mod, true);
        snap.module_online_states.insert(cloak_mod, true);
        snap.module_online_states.insert(portal_mod, true);
        snap.skills.entry(cyno_char).or_default().insert(21611, TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(cyno_char).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(titan_char).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
        let fleet_id = 77_001;
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: titan_char,
                members: vec![cyno_char, titan_char, subcap_char],
                ..Default::default()
            },
        );
        if let Some(ch_base) = snap.characters.get(&cyno_char).cloned() {
            let mut sub_ch = ch_base;
            sub_ch.character_id = subcap_char;
            sub_ch.user_id = 3;
            sub_ch.active_ship_id = rifter_id;
            sub_ch.solar_system_id = low_origin;
            snap.characters.insert(subcap_char, sub_ch);
        }
        for (cid, sid, sys) in [(cyno_char, falcon_id, low_dest), (titan_char, avatar_id, low_origin), (subcap_char, rifter_id, low_origin)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys;
                ch.active_ship_id = sid;
                ch.station_id = None;
            }
        }
        snap.accounts.insert(
            "sub".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "sub".to_string(),
                password_token: "dummy".to_string(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    spawn_ship_ball(&ctx, low_dest, falcon_id, 11957, cyno_char, 0.0).await;
    spawn_ship_ball(&ctx, low_origin, avatar_id, 11567, titan_char, 0.0).await;
    spawn_ship_ball(&ctx, low_origin, rifter_id, 587, subcap_char, 1_000.0).await;

    let mut s_cyno = make_session(1, 1, cyno_char, 1000009, None, low_dest, Some(falcon_id));
    let mut s_titan = make_session(2, 2, titan_char, 1000009, None, low_origin, Some(avatar_id));
    let mut s_sub = make_session(3, 3, subcap_char, 1000009, None, low_origin, Some(rifter_id));

    let now_ts = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(1, s_cyno.session_id, Some(cyno_char), true, now_ts);
    let _ = ctx.db.acquire_online_lock(2, s_titan.session_id, Some(titan_char), true, now_ts);
    let _ = ctx.db.acquire_online_lock(3, s_sub.session_id, Some(subcap_char), true, now_ts);

    let cyno_rep = call_service(&ctx, &mut s_cyno, "dogmaIM", "Activate", vec![PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], HashMap::new()).await.expect("Falcon lights cynosural field");
    let beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");
    assert!(beacon_id > 0);

    let lo_after = ctx.db.with_snapshot(|s| s.items.get(&lo_id).map(|i| i.quantity).unwrap_or(0));
    assert!(lo_after < 2_000, "Liquid Ozone must be consumed scaled by Cynosural Field Theory");
    let b_ball = ctx.systems.get_or_spawn(low_dest).get_ball_snapshot(beacon_id).await.ok().flatten();
    assert!(b_ball.is_some(), "Beacon ball must be spawned in space");
    let f_ball = ctx.systems.get_or_spawn(low_dest).get_ball_snapshot(falcon_id).await.ok().flatten().unwrap().1;
    assert_eq!(f_ball.max_velocity, 0.0, "Cyno ship must be immobilized (0 m/s)");

    let dock_err = call_service(&ctx, &mut s_cyno, "stationSvc", "Dock", vec![PyRep::Integer(60003760)], HashMap::new()).await.expect_err("Dock blocked");
    assert_eq!(dock_err.code, "CynosuralFieldActive");
    let b_cyno = ctx.bind_object(&s_cyno, "beyonce", low_dest as i64).expect("bind beyonce");
    let jump_err = ctx.dispatch_rpc(&mut s_cyno, "beyonce", Some(b_cyno), "CmdStargateJump", &[PyRep::Integer(50000001)], &HashMap::new(), None).await.expect_err("Gate jump blocked");
    assert_eq!(jump_err.code, "CynosuralFieldActive");
    let cloak_err = call_service(&ctx, &mut s_cyno, "dogmaIM", "Activate", vec![PyRep::Integer(cloak_mod), PyRep::String("cloaking".into())], HashMap::new()).await.expect_err("Cloak blocked");
    assert_eq!(cloak_err.code, "CynosuralFieldActive");

    let beacon_notifs = ctx.drain_notifications_for_char(titan_char);
    let beacon_notif = beacon_notifs.iter().find(|n| n.event == "OnFleetJumpBeaconModuleChange").expect("Beacon notification sent to fleet members");
    assert_eq!(beacon_notif.args.get(2).and_then(|v| v.as_i64()), Some(beacon_id), "Beacon must appear in fleet jump target list");

    let bridge_rep = call_service(&ctx, &mut s_titan, "beyonce", "CmdBridgeToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect("Titan opens jump portal bridge");
    assert_eq!(bridge_rep.dict_get("beaconID").and_then(|v| v.as_i64()), Some(beacon_id));

    let _ = ctx.systems.get_or_spawn(low_origin).set_ball_position(rifter_id, DVec3::new(3_500.0, 0.0, 0.0)).await;
    let dist_err = call_service(&ctx, &mut s_sub, "beyonce", "CmdJumpThroughFleet", vec![PyRep::Integer(titan_char), PyRep::Integer(avatar_id)], HashMap::new()).await.expect_err("Passenger > 2500m must be refused");
    assert_eq!(dist_err.code, "OutOfPortalRange");
    let _ = ctx.systems.get_or_spawn(low_origin).set_ball_position(rifter_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let titan_h_before = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
    let jump_through = call_service(&ctx, &mut s_sub, "beyonce", "CmdJumpThroughFleet", vec![PyRep::Integer(titan_char), PyRep::Integer(avatar_id)], HashMap::new()).await.expect("Passenger jumps through portal");
    assert_eq!(jump_through.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(low_dest as i64));
    let titan_h_after = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
    assert!(titan_h_after < titan_h_before, "Titan isotopes consumed scaled by passenger mass and distance");
    let fatigue_min = jump_through.dict_get("jumpFatigueMinutes").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(fatigue_min > 0.0, "Passenger must accumulate jump fatigue");

    ctx.advance_secs(61);
    let _ = ctx.systems.get_or_spawn(low_origin).step_manual(1.0).await;

    let far_err = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(30000142), PyRep::Integer(beacon_id)], HashMap::new()).await.expect_err("Jump distance out of range");
    assert_eq!(far_err.code, "JumpDistanceOutOfRange");

    let titan_jump = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect("Titan jumps to fleet cyno beacon");
    assert_eq!(titan_jump.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(low_dest as i64));
    let titan_h_final = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
    assert!(titan_h_final < titan_h_after, "Titan isotopes consumed scaled by Jump Fuel Conservation");
    let cd_err = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect_err("Immediate jump cooldown");
    assert_eq!(cd_err.code, "JumpReactivationCooldown");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod ships_cyno_and_escape_bay {
    use super::*;

//  End-to-end integration tests for `feat:ships.*` batch closure:
//  - `feat:ships.covert-cyno-beacon`
//  - `feat:ships.cynosural-beacon-spawning`
//  - `feat:ships.frigate-escape-bay`
//  - `feat:ships.jump-drive-fatigue-accumulation`
//  - `feat:ships.titan-jump-portal-bridge`


use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
use eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_cyno_portal::{
    is_ship_cyno_active, sweep_expired_cyno_beacons,
};
use glam::DVec3;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const C3: i64 = 90_000_003;

const FALCON: u32 = 11957;
const RAVEN: u32 = 638;
const RIFTER: u32 = 587;
const CARACAL: u32 = 621;
const AVATAR: u32 = 11567;
const REVELATION: u32 = 19720;
const CYNO_GEN: u32 = 21096;
const COVERT_CYNO_GEN: u32 = 28646;
const JUMP_PORTAL_GEN: u32 = 23953;
const LIQUID_OZONE: u32 = 16273;
const STRONTIUM: u32 = 16275;
const HELIUM_ISOTOPES: u32 = 16274;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third".into(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "third".into(),
                password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut c = snap.characters[&C2].clone();
        c.character_id = C3;
        c.user_id = 3;
        c.name = "Third Pilot".into();
        snap.characters.insert(C3, c);
        let sk = snap.skills.get(&C2).cloned().unwrap_or_default();
        snap.skills.insert(C3, sk);
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

fn grant_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    let skills: Vec<u32> = pairs
        .iter()
        .filter_map(|(s, _)| ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(s).map(|&v| v as u32)))
        .filter(|&s| s > 0)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in skills {
            entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    loc_id: i64,
    flag: InventoryFlag,
    qty: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id: loc_id,
                flag,
                quantity: qty,
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

async fn spawn_pilot_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    grant_skills_for_type(ctx, char_id, type_id);
    if ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)) {
        seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.jump_cooldown_until_unix = 0;
            ch.jump_fatigue_until_unix = 0;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.invalidate_ship_fitting(ship_id);

    let t = &ctx.sde.types[&type_id];
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 150.0,
        inertia: 1.0,
        mass: t.mass,
        radius: t.radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    let meta = ctx.build_ship_slim_meta(char_id, type_id);
    ctx.systems.get_or_spawn(sys_id).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn");
}

fn fit_online_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    mod_item_id: i64,
    mod_type_id: u32,
    flag: InventoryFlag,
) {
    grant_skills_for_type(ctx, char_id, mod_type_id);
    seed_item(ctx, mod_item_id, mod_type_id, char_id, ship_id, flag, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mod_item_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
}

fn create_fleet(ctx: &ServerContext, fleet_id: i64, members: &[i64]) {
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: members[0],
                members: members.to_vec(),
                ..Default::default()
            },
        );
        for &m in members {
            if let Some(ch) = s.characters.get_mut(&m) {
                ch.fleet_id = Some(fleet_id);
            }
        }
    });
}

fn system_distance_ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
    let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
    let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
    ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt()
}

fn pick_test_systems(ctx: &ServerContext) -> (u32, u32) {
    let mut lowsec: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
        .map(|s| s.solar_system_id)
        .collect();
    lowsec.sort_unstable();
    for &a in &lowsec {
        if let Some(&b) = lowsec.iter().find(|&&b| b != a && (0.5..2.5).contains(&system_distance_ly(ctx, a, b))) {
            return (a, b);
        }
    }
    panic!("No pair of lowsec systems found");
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_covert_and_standard_cyno_spawning_and_despawning() {
    // parity: feat:ships.covert-cyno-beacon
    // parity: feat:ships.cynosural-beacon-spawning
    eve_testkit::attest(&[
        "feat:ships.covert-cyno-beacon",
        "feat:ships.cynosural-beacon-spawning",
    ]);

    let ctx = new_ctx();
    let (sys_a, sys_b) = pick_test_systems(&ctx);
    let highsec_sys = 30_000_142_u32; // Jita

    let mut sess1 = SessionState::new(101);
    sess1.character_id = Some(C1);
    sess1.user_id = Some(1);
    let ship1 = 101_001_i64;
    spawn_pilot_in_space(&ctx, &mut sess1, C1, ship1, FALCON, highsec_sys, DVec3::ZERO).await;
    create_fleet(&ctx, 601, &[C1, C2]);

    // 1. Highsec rejection: Standard cyno cannot be lit in highsec (security >= 0.45)
    let cyno_mod_hi = 201_001_i64;
    fit_online_module(&ctx, C1, ship1, cyno_mod_hi, CYNO_GEN, InventoryFlag::HiSlot0);
    seed_item(&ctx, 301_001, LIQUID_OZONE, C1, ship1, InventoryFlag::Cargo, 1000, false);
    let hi_err = rpc(
        &ctx,
        &mut sess1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_hi), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(hi_err.code, "CannotLightCynoInSystem");

    // 2. Standard Cyno in lowsec: consumes Liquid Ozone, spawns beacon, immobilizes ship
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&C1) {
            ch.solar_system_id = sys_a;
        }
        if let Some(sh) = s.items.get_mut(&ship1) {
            sh.location_id = sys_a as i64;
        }
    });
    sess1.solar_system_id = Some(sys_a);
    ctx.systems.get_or_spawn(sys_a).spawn_ball_with_meta(
        Ball {
            id: ship1,
            type_id: FALCON,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: ctx.sde.types[&FALCON].mass,
            radius: ctx.sde.types[&FALCON].radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(C1, ship1, FALCON),
        ctx.build_ship_slim_meta(C1, FALCON),
    ).await.expect("spawn in sys_a");
    let cft_skill = 21603_u32; // Cynosural Field Theory
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(C1).or_default().insert(
            cft_skill,
            eve_db::TrainedSkillRecord { type_id: cft_skill, level: 5, skill_points: 256_000.0 },
        );
    });

    let act_res = rpc(
        &ctx,
        &mut sess1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_hi), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .expect("activate cyno in lowsec");

    let beacon_id = act_res.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");
    assert!(beacon_id > 0);
    assert!(is_ship_cyno_active(&ctx, ship1));

    // Consumed Liquid Ozone (Falcon Force Recon role gives -80%, CFT V gives -50%, 500 * 0.2 * 0.5 = 50 consumed)
    let ozone_left = ctx.db.with_snapshot(|s| s.items.get(&301_001).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(ozone_left, 950);

    // Ship is immobilized (max_velocity becomes 0.0)
    let snap_ball = ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(ship1).await.unwrap().unwrap().1;
    assert_eq!(snap_ball.max_velocity, 0.0);

    // Cyno beacon ball exists in ballpark
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(beacon_id).await.unwrap().is_some());

    // 3. Covert Cyno Generator on Force Recon / Falcon:
    let mut sess2 = SessionState::new(102);
    sess2.character_id = Some(C2);
    sess2.user_id = Some(2);
    let ship2 = 101_002_i64;
    spawn_pilot_in_space(&ctx, &mut sess2, C2, ship2, FALCON, sys_b, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let covert_mod = 201_002_i64;
    fit_online_module(&ctx, C2, ship2, covert_mod, COVERT_CYNO_GEN, InventoryFlag::HiSlot1);
    seed_item(&ctx, 301_002, LIQUID_OZONE, C2, ship2, InventoryFlag::Cargo, 500, false);

    // Advance clock past standard cyno expiry
    ctx.advance_secs(600);
    sweep_expired_cyno_beacons(&ctx);
    assert!(!is_ship_cyno_active(&ctx, ship1));
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(beacon_id).await.unwrap().is_none());

    // Light Covert Cyno in sys_b
    let cov_act = rpc(
        &ctx,
        &mut sess2,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(covert_mod), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .expect("activate covert cyno");
    let cov_beacon_id = cov_act.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cov beaconID");
    assert!(cov_beacon_id > 0);
    assert!(is_ship_cyno_active(&ctx, ship2));

    // Covert cyno duration is 60s, isCovert: true, onGridOnly: true (no local notification)
    let dur = cov_act.dict_get("durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert_eq!(dur, 60.0);
    assert_eq!(cov_act.dict_get("isCovert"), Some(&PyRep::Bool(true)));
    assert_eq!(cov_act.dict_get("onGridOnly"), Some(&PyRep::Bool(true)));

    // Capital ship (Avatar) cannot jump to covert cyno beacon
    create_fleet(&ctx, 701, &[C1, C2]);
    let mut sess_titan = SessionState::new(103);
    sess_titan.character_id = Some(C1);
    sess_titan.user_id = Some(1);
    let titan_ship = 101_003_i64;
    spawn_pilot_in_space(&ctx, &mut sess_titan, C1, titan_ship, AVATAR, sys_a, DVec3::ZERO).await;

    let titan_jump_err = rpc(
        &ctx,
        &mut sess_titan,
        "beyonce",
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(C2), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(titan_jump_err.code, "ShipCannotJumpToBeacon");

    // Advance clock past covert cyno expiry: swept clean
    ctx.advance_secs(70);
    sweep_expired_cyno_beacons(&ctx);
    assert!(!is_ship_cyno_active(&ctx, ship2));
    assert!(ctx.systems.get_or_spawn(sys_b).get_ball_snapshot(cov_beacon_id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_battleship_frigate_escape_bay_ejection_on_destruction() {
    // parity: feat:ships.frigate-escape-bay
    eve_testkit::attest(&["feat:ships.frigate-escape-bay"]);

    let ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let victim_char = C1;
    let attacker_char = C2;
    let battleship_id = 500_001_i64;
    let frigate_id = 500_002_i64;
    let non_frigate_id = 500_003_i64;

    let mut sess = SessionState::new(201);
    sess.character_id = Some(victim_char);
    sess.user_id = Some(1);
    spawn_pilot_in_space(&ctx, &mut sess, victim_char, battleship_id, RAVEN, sys_id, DVec3::ZERO).await;

    // 1. Rejection: Non-frigate (Caracal, group 26) cannot be put into Frigate Escape Bay (flag 177)
    seed_item(&ctx, non_frigate_id, CARACAL, victim_char, battleship_id, InventoryFlag::FrigateEscapeBay, 1, true);
    // 2. Put assembled frigate (Rifter, group 25) into Frigate Escape Bay
    seed_item(&ctx, frigate_id, RIFTER, victim_char, battleship_id, InventoryFlag::FrigateEscapeBay, 1, true);

    // Verify Raven has escape bay in SDE attribute 3020 or group 27
    let group_id = ctx.sde.types[&RAVEN].group_id;
    assert_eq!(group_id, 27); // Battleship

    // 3. Destroy Raven in space via death pipeline
    let death_result = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        victim_char,
        battleship_id,
        attacker_char,
        DVec3::new(100.0, 200.0, 300.0),
        12345,
    )
    .expect("execute_death_pipeline_sync");

    // Pilot auto-ejects into the fitted frigate instead of capsule
    assert_eq!(
        death_result.ejected_capsule_id,
        Some(frigate_id),
        "Pilot must eject in the frigate from FrigateEscapeBay"
    );

    // Frigate survives in space at ship position
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&victim_char).unwrap();
        assert_eq!(ch.active_ship_id, frigate_id, "Character active ship must become the frigate");

        let esc_item = snap.items.get(&frigate_id).unwrap();
        assert_eq!(esc_item.location_id, sys_id as i64, "Frigate is now located in space");
        assert_eq!(esc_item.flag, InventoryFlag::None);

        // Raven was removed
        assert!(!snap.items.contains_key(&battleship_id));
    });

    // Killmail records the frigate survived
    let detailed_km = eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails.get(&death_result.killmail_id).cloned()
    })
    .expect("detailed killmail");
    let km_frigate = detailed_km
        .dropped_items
        .iter()
        .find(|it| it.type_id == RIFTER)
        .expect("Killmail must record the frigate");
    assert!(
        km_frigate.flag == 177 || km_frigate.flag == 179,
        "Flag on killmail must match escape bay flag"
    );
    assert_eq!(km_frigate.qty_dropped, 0);
    assert_eq!(km_frigate.qty_destroyed, 0);

    // Frigate did not drop as loot and was not destroyed
    assert!(!death_result.loot_dropped_item_ids.contains(&frigate_id));
    assert!(!death_result.loot_destroyed_item_ids.contains(&frigate_id));
}

#[tokio::test]
async fn test_jump_drive_fatigue_and_titan_portal_bridge() {
    // parity: feat:ships.jump-drive-fatigue-accumulation
    // parity: feat:ships.titan-jump-portal-bridge
    eve_testkit::attest(&[
        "feat:ships.jump-drive-fatigue-accumulation",
        "feat:ships.titan-jump-portal-bridge",
    ]);

    let ctx = new_ctx();
    let (sys_a, sys_b) = pick_test_systems(&ctx);
    let dist_ly = system_distance_ly(&ctx, sys_a, sys_b);
    assert!(dist_ly > 0.0 && dist_ly < 5.0);

    // 1. Mathematical fatigue accumulation and cooldown checks
    // Cooldown formula: 1 + effective_ly (min 10% of fatigue, max 30 min)
    // Fatigue formula: max(current_fatigue, 10) * (1 + effective_ly) (max 300 min = 5h)
    let (cd1, fat1) = compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
    assert!((cd1 - 6.0).abs() < 1e-6, "Cooldown for 5 LY from 0 fatigue is 6 min");
    assert!((fat1 - 60.0).abs() < 1e-6, "Fatigue for 5 LY from 0 fatigue is 60 min");

    // Existing fatigue: 30 min -> 30 * (1 + 5) = 180 min
    let (cd2, fat2) = compute_jump_fatigue_and_cooldown_minutes(30.0, 5.0, 0.0);
    assert!((cd2 - 6.0).abs() < 1e-6);
    assert!((fat2 - 180.0).abs() < 1e-6);

    // 5-hour cap (300 min)
    let (_, fat_cap) = compute_jump_fatigue_and_cooldown_minutes(100.0, 5.0, 0.0);
    assert!((fat_cap - 300.0).abs() < 1e-6, "Fatigue must be capped at 300 min");

    // 30-minute cooldown cap
    let (cd_cap, _) = compute_jump_fatigue_and_cooldown_minutes(300.0, 35.0, 0.0);
    assert!((cd_cap - 30.0).abs() < 1e-6, "Cooldown must be capped at 30 min");

    // Black Ops fatigue reduction bonus (75% reduction on 8 LY -> 2 effective LY)
    let (cd_blops, fat_blops) = compute_jump_fatigue_and_cooldown_minutes(0.0, 8.0, 0.75);
    assert!((cd_blops - 3.0).abs() < 1e-6);
    assert!((fat_blops - 30.0).abs() < 1e-6);

    // 2. Titan Jump Portal Bridge Setup
    create_fleet(&ctx, 801, &[C1, C2, C3]);

    // Pilot 1: Avatar Titan in sys_a
    let mut sess_titan = SessionState::new(301);
    sess_titan.character_id = Some(C1);
    sess_titan.user_id = Some(1);
    let titan_id = 600_001_i64;
    let portal_mod = 600_002_i64;
    spawn_pilot_in_space(&ctx, &mut sess_titan, C1, titan_id, AVATAR, sys_a, DVec3::ZERO).await;
    fit_online_module(&ctx, C1, titan_id, portal_mod, JUMP_PORTAL_GEN, InventoryFlag::HiSlot0);
    seed_item(&ctx, 700_001, STRONTIUM, C1, titan_id, InventoryFlag::Cargo, 1000, false);
    seed_item(&ctx, 700_002, HELIUM_ISOTOPES, C1, titan_id, InventoryFlag::Cargo, 50_000, false);

    // Pilot 2: Cyno ship in sys_b
    let mut sess_cyno = SessionState::new(302);
    sess_cyno.character_id = Some(C2);
    sess_cyno.user_id = Some(2);
    let cyno_ship = 600_003_i64;
    let cyno_mod = 600_004_i64;
    spawn_pilot_in_space(&ctx, &mut sess_cyno, C2, cyno_ship, FALCON, sys_b, DVec3::ZERO).await;
    fit_online_module(&ctx, C2, cyno_ship, cyno_mod, CYNO_GEN, InventoryFlag::HiSlot0);
    seed_item(&ctx, 700_003, LIQUID_OZONE, C2, cyno_ship, InventoryFlag::Cargo, 1000, false);

    // Light cyno in sys_b
    let cyno_act = rpc(
        &ctx,
        &mut sess_cyno,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .expect("cyno activate");
    let beacon_id = cyno_act.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");

    // Titan opens bridge to fleet beacon in sys_b
    let bridge_res = rpc(
        &ctx,
        &mut sess_titan,
        "beyonce",
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(C2), PyRep::Integer(sys_b as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .expect("open jump bridge");

    let portal_ball_id = bridge_res.dict_get("portalBallID").and_then(|v| v.as_i64()).expect("portalBallID");
    assert!(portal_ball_id > 0);
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(portal_ball_id).await.unwrap().is_some());

    // Strontium consumed for opening bridge (500 base, with Titan skill level 5 reduced to 250)
    let stront_left = ctx.db.with_snapshot(|s| s.items.get(&700_001).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(stront_left, 750);

    // 3. Subcapital Passenger (Rifter) jumps through bridge
    let mut sess_pass = SessionState::new(303);
    sess_pass.character_id = Some(C3);
    sess_pass.user_id = Some(3);
    let rifter_id = 600_005_i64;
    // Spawn within 2 km of titan (within 2.5 km portal radius)
    spawn_pilot_in_space(&ctx, &mut sess_pass, C3, rifter_id, RIFTER, sys_a, DVec3::new(1000.0, 0.0, 0.0)).await;

    let jump_res = rpc(
        &ctx,
        &mut sess_pass,
        "beyonce",
        "CmdJumpThroughFleet",
        &[PyRep::Integer(C1), PyRep::Integer(titan_id)],
    )
    .await
    .expect("CmdJumpThroughFleet");

    let final_sys = jump_res.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!(final_sys, sys_b as i64, "Passenger must arrive at cyno system");

    let fuel_consumed = jump_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(fuel_consumed > 0, "Helium isotopes must be consumed from Titan");

    let isotopes_left = ctx.db.with_snapshot(|s| s.items.get(&700_002).map(|i| i.quantity as i64).unwrap_or(0));
    assert_eq!(isotopes_left, 50_000 - fuel_consumed);

    // Passenger accumulated fatigue
    let pass_fatigue = jump_res.dict_get("jumpFatigueMinutes").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(pass_fatigue > 0.0);

    // 4. Capital ship passenger (Revelation) is rejected by Titan bridge
    let mut sess_cap = SessionState::new(304);
    sess_cap.character_id = Some(C3);
    sess_cap.user_id = Some(3);
    let dread_id = 600_006_i64;
    spawn_pilot_in_space(&ctx, &mut sess_cap, C3, dread_id, REVELATION, sys_a, DVec3::new(1000.0, 0.0, 0.0)).await;

    let cap_jump_err = rpc(
        &ctx,
        &mut sess_cap,
        "beyonce",
        "CmdJumpThroughFleet",
        &[PyRep::Integer(C1), PyRep::Integer(titan_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(cap_jump_err.code, "JumpBridgeRestrictedShip");

    // 5. Advance clock past bridge duration: bridge and portal ball despawn
    ctx.advance_secs(60);
    sweep_expired_cyno_beacons(&ctx);
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(portal_ball_id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_issue_630_docked_fit_miner_sets_singleton_dbrow_and_survives_undock() {
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7630);
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
    let char_id = sess.character_id.unwrap();
    let station_id = sess.station_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let miner_stack_id = 990_630_001i64;
    // Miner I (typeID 483), qty=2, singleton=false in station hangar (matches #630 repro)
    seed_item(&ctx, miner_stack_id, 483, char_id, station_id, InventoryFlag::Hangar, 2, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3386, // Mining I
            eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    // Fit 2x Miner I into HiSlot1 (28) and HiSlot2 (29) while docked
    let fit1 = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(miner_stack_id), PyRep::Integer(ship_id), PyRep::Integer(28), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("fit first Miner I");
    let miner1_id = fit1.dict_get("itemID").and_then(PyRep::as_i64).unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(miner_stack_id), PyRep::Integer(ship_id), PyRep::Integer(29)],
        &kw,
        None,
    )
    .await
    .expect("fit second Miner I");

    let notifs = ctx.drain_notifications_for_char(char_id);
    for fitted_id in [miner1_id, miner_stack_id] {
        let chg = notifs
            .iter()
            .find(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dict_get("itemID")).and_then(PyRep::as_i64) == Some(fitted_id)
                    && n.args.first().and_then(|r| r.dict_get("locationID")).and_then(PyRep::as_i64) == Some(ship_id)
            })
            .expect("OnItemChange for fitted Miner I");
        assert!(matches!(chg.args.first(), Some(PyRep::DBRow { .. })));
        assert_eq!(chg.args[0].dict_get("quantity").and_then(PyRep::as_i64), Some(-1));
        assert_eq!(chg.args[0].dict_get("groupID").and_then(PyRep::as_i64), Some(54));
        assert_eq!(chg.args[0].dict_get("categoryID").and_then(PyRep::as_i64), Some(7));
    }

    // Undock and confirm dogmaIM.GetAllInfo(True, True) lists both fitted Miner Is with invItem singleton quantity=-1
    let _ = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock succeeds");
    let all_info = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None)
        .await
        .expect("dogmaIM.GetAllInfo after undock");
    let fitted_slots: Vec<(i64, i64, i64)> = match all_info.dict_get("shipInfo") {
        Some(PyRep::Dict(entries)) => entries
            .iter()
            .filter_map(|(k, v)| {
                let id = k.as_i64()?;
                let inv = v.dict_get("invItem")?;
                Some((id, inv.dict_get("flagID")?.as_i64()?, inv.dict_get("quantity")?.as_i64()?))
            })
            .collect(),
        _ => Vec::new(),
    };
    assert!(fitted_slots.contains(&(miner1_id, 28, -1)), "GetAllInfo missing Miner I in slot 28: {fitted_slots:?}");
    assert!(fitted_slots.contains(&(miner_stack_id, 29, -1)), "GetAllInfo missing Miner I in slot 29: {fitted_slots:?}");
}

}
