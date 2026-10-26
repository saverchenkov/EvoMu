//! Comprehensive Inventory (14 rows), Ships (8 rows), and Fitting (10 rows) integration test suite (Part 1).

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
                start_unix: 1_700_000_000,
            },
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
