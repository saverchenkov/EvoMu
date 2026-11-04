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

pub(crate) mod ships_board_eject_corvette_insurance {
    // Comprehensive Inventory, Ships, and Fitting integration test suite (Part 2).

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
        let station_id = 60_003_760i64;

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

        let dup_corvette_err = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
            .await
            .expect_err("CreateNewbieShip must fail when character already has a non-capsule ship in station");
        assert_eq!(dup_corvette_err.code, "AlreadyHaveShipInStation");

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

pub(crate) mod jetcan_wreck_sma_fleet_hangar {
    // Comprehensive Inventory, Ships, and Fitting integration test suite (Part 3: Asset Safety, SMA, Fleet Hangar, Jetcan, and Wreck Looting).

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

pub(crate) mod make_session_ship_bay_passwords {
    // Round-4 Track 3 Regression Tests (`#176`–`#187`):
    // Skills, Fitting, Inventory, Stacking, Corporation Governance & Agents.


    use eve_db::{
        CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
        SkillQueueEntry, TrainedSkillRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::{BoundObjectEntry, ServerContext};
    use std::collections::{HashMap, HashSet};

    fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) -> SessionState {
        let mut s = SessionState::default();
        s.user_id = Some(1);
        s.character_id = Some(char_id);
        s.corporation_id = Some(corp_id);
        s.station_id = station_id;
        s.solar_system_id = Some(sys_id);
        s.ship_id = Some(ship_id);
        s
    }

    #[tokio::test]
    async fn r4_176_180_181_183_ship_bay_passwords_alsc_locks_and_container_roles() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let station_id = 60_003_760_i64;
        let sys_id = 30_000_142_u32;
        let corp_id = 2_000_501_i64;
        let owner_id = 95_001_001_i64;
        let fleetmate_id = 95_001_002_i64;
        let host_ship_id = 880_001_001_i64;
        let target_ship_id = 880_001_002_i64;
        let alsc_id = 880_001_010_i64;
        let trit_a = 880_001_011_i64;
        let trit_b = 880_001_012_i64;
        let incompatible_item = 880_001_013_i64;
        let corp_alsc_id = 880_001_020_i64;
        let corp_item_id = 880_001_021_i64;

        let carrier_tid = ctx
            .sde
            .types
            .values()
            .find(|t| {
                t.category_id == 6
                    && t.default_attributes.get(&908).copied().unwrap_or(0.0) >= 100_000.0
                    && t.default_attributes.get(&912).copied().unwrap_or(0.0) >= 1_000.0
            })
            .map(|t| t.type_id)
            .unwrap_or(28606); // Orca

        let alsc_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 448 && t.capacity >= 1_000.0)
            .map(|t| t.type_id)
            .unwrap_or(17366);

        ctx.db.with_mut_snapshot(|snap| {
            let base_ch = snap.characters.get(&90_000_001).cloned().expect("seeded char");
            for &cid in &[owner_id, fleetmate_id] {
                let mut ch = base_ch.clone();
                ch.character_id = cid;
                ch.name = format!("Pilot {cid}");
                ch.corporation_id = corp_id;
                ch.solar_system_id = sys_id;
                ch.station_id = Some(station_id);
                ch.active_ship_id = if cid == owner_id { host_ship_id } else { 0 };
                ch.balance = IskCents::from_isk(10_000_000);
                ch.clone_grade = 2;
                snap.characters.insert(cid, ch);
            }
            let mut members = HashSet::new();
            members.insert(owner_id);
            members.insert(fleetmate_id);
            let mut member_roles = HashMap::new();
            member_roles.insert(owner_id, u64::MAX);
            // Give fleetmate HangarCanTake1 (1 << 13) and HangarCanQuery1 (1 << 20), but NOT ContainerCanTake1 (1 << 43)
            member_roles.insert(fleetmate_id, (1u64 << 13) | (1u64 << 20));
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "R4 Test Corp".into(),
                    ticker: "R4TC".into(),
                    ceo_id: owner_id,
                    members,
                    member_roles,
                    ..Default::default()
                },
            );
            snap.items.insert(
                host_ship_id,
                InventoryItemRecord {
                    item_id: host_ship_id,
                    type_id: carrier_tid,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                target_ship_id,
                InventoryItemRecord {
                    item_id: target_ship_id,
                    type_id: 587, // Rifter
                    owner_id: fleetmate_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                alsc_id,
                InventoryItemRecord {
                    item_id: alsc_id,
                    type_id: alsc_tid,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                trit_a,
                InventoryItemRecord {
                    item_id: trit_a,
                    type_id: 34,
                    owner_id,
                    location_id: alsc_id,
                    flag: InventoryFlag::Unlocked,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                trit_b,
                InventoryItemRecord {
                    item_id: trit_b,
                    type_id: 34,
                    owner_id,
                    location_id: alsc_id,
                    flag: InventoryFlag::Unlocked,
                    quantity: 50,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                incompatible_item,
                InventoryItemRecord {
                    item_id: incompatible_item,
                    type_id: 35, // Pyerite (different type)
                    owner_id,
                    location_id: alsc_id,
                    flag: InventoryFlag::Unlocked,
                    quantity: 25,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                corp_alsc_id,
                InventoryItemRecord {
                    item_id: corp_alsc_id,
                    type_id: alsc_tid,
                    owner_id: corp_id,
                    location_id: station_id,
                    flag: InventoryFlag::CorpSAG1,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                corp_item_id,
                InventoryItemRecord {
                    item_id: corp_item_id,
                    type_id: 34,
                    owner_id: corp_id,
                    location_id: corp_alsc_id,
                    flag: InventoryFlag::Unlocked,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let mut owner_sess = make_session(owner_id, corp_id, Some(station_id), sys_id, host_ship_id);
        let mut mate_sess = make_session(fleetmate_id, corp_id, Some(station_id), sys_id, 0);
        let kw = HashMap::new();

        ctx.dispatch_rpc(
            &mut owner_sess,
            "ship",
            None,
            "ConfigureShip",
            &[
                PyRep::Integer(host_ship_id),
                PyRep::Dict(vec![
                    (PyRep::String("FleetHangar_AllowFleetAccess".into()), PyRep::Bool(true)),
                    (PyRep::String("FleetHangar_AllowCorpAccess".into()), PyRep::Bool(true)),
                    (PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(true)),
                    (PyRep::String("SMB_AllowCorpAccess".into()), PyRep::Bool(true)),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("ConfigureShip");
        ctx.dispatch_rpc(
            &mut owner_sess,
            "ship",
            None,
            "SetShipPassword",
            &[
                PyRep::Integer(host_ship_id),
                PyRep::String("secret-bay".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SetShipPassword");

        // #176: StoreVessel without password must be rejected with SMAAccessDenied
        let no_pw_res = ctx
            .dispatch_rpc(
                &mut mate_sess,
                "ship",
                None,
                "StoreVessel",
                &[PyRep::Integer(target_ship_id), PyRep::Integer(host_ship_id)],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&no_pw_res, Err(e) if e.code == "SMAAccessDenied"),
            "expected SMAAccessDenied when password omitted, got {no_pw_res:?}"
        );

        // #176: StoreVessel with correct password succeeds
        let mut pw_kw = HashMap::new();
        pw_kw.insert("password".into(), PyRep::String("secret-bay".into()));
        ctx.dispatch_rpc(
            &mut mate_sess,
            "ship",
            None,
            "StoreVessel",
            &[PyRep::Integer(target_ship_id), PyRep::Integer(host_ship_id)],
            &pw_kw,
            None,
        )
        .await
        .expect("StoreVessel with valid password should succeed");

        // #183: Lock trit_a inside ALSC and verify MultiMerge rejects it,
        // and partial MultiMerge onto incompatible_item does not leave split stacks behind.
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "ALSCLockItems",
            &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(trit_a)])],
            &kw,
            None,
        )
        .await
        .expect("ALSCLockItems");

        let merge_locked = ctx
            .dispatch_rpc(
                &mut owner_sess,
                "invbroker",
                None,
                "MultiMerge",
                &[PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(trit_b),
                    PyRep::Integer(trit_a),
                    PyRep::Integer(10),
                ])])],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&merge_locked, Err(e) if e.code == "ALSCItemLocked"),
            "expected ALSCItemLocked on MultiMerge with locked item, got {merge_locked:?}"
        );

        let item_count_before = ctx.db.with_snapshot(|s| s.items.len());
        let merge_incompat = ctx
            .dispatch_rpc(
                &mut owner_sess,
                "invbroker",
                None,
                "MultiMerge",
                &[PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(trit_b),
                    PyRep::Integer(incompatible_item),
                    PyRep::Integer(10),
                ])])],
                &kw,
                None,
            )
            .await;
        assert!(merge_incompat.is_err(), "merging different types must fail");
        let (item_count_after, trit_b_qty) = ctx.db.with_snapshot(|s| {
            (
                s.items.len(),
                s.items.get(&trit_b).map(|i| i.quantity).unwrap_or(0),
            )
        });
        assert_eq!(item_count_after, item_count_before, "failed partial MultiMerge must not leave split stacks behind");
        assert_eq!(trit_b_qty, 50, "source stack quantity must remain intact when merge fails");

        // #180 & #181: Set password on ALSC container; verify bound List requires password
        // and corp container take requires ROLE_CONTAINER_CAN_TAKE (1 << 43).
        ctx.dispatch_rpc(
            &mut owner_sess,
            "invbroker",
            None,
            "SetPassword",
            &[PyRep::Integer(alsc_id), PyRep::String("alsc-pass".into())],
            &kw,
            None,
        )
        .await
        .expect("SetPassword on ALSC");

        let bound_alsc = BoundObjectEntry {
            bound_id: 99991,
            session_id: mate_sess.session_id,
            user_id: 1,
            character_id: fleetmate_id,
            service: "invbroker".into(),
            object_id: alsc_id, oid_str: "N=888444:99991".into(), ref_id: 133_000_000_000_000_000,
        };
        ctx.bound_objects.write().unwrap().insert(99991, bound_alsc);
        let list_no_pw = ctx
            .dispatch_rpc(&mut mate_sess, "invbroker", Some(99991), "List", &[], &kw, None)
            .await;
        assert!(
            matches!(&list_no_pw, Err(e) if e.code == "ALSCPasswordRequired"),
            "expected ALSCPasswordRequired on bound List without password, got {list_no_pw:?}"
        );

        let take_no_cont_role = ctx
            .dispatch_rpc(
                &mut mate_sess,
                "invbroker",
                None,
                "Add",
                &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(4)],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&take_no_cont_role, Err(e) if e.code == "CorpRoleDenied"),
            "expected CorpRoleDenied when lacking ContainerCanTake1, got {take_no_cont_role:?}"
        );

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.corporations.get_mut(&corp_id) {
                *c.member_roles.entry(fleetmate_id).or_default() |= 1u64 << 43;
            }
        });
        ctx.dispatch_rpc(
            &mut mate_sess,
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("Add from corp container with ContainerCanTake1 role");
    }

    #[tokio::test]
    async fn r4_177_178_179_182_184_185_186_187_skills_corp_ammo_agents_and_killcounter() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let station_id = 60_003_760_i64;
        let sys_id = 30_000_142_u32;
        let corp_id = 2_000_502_i64;
        let ceo_id = 95_002_001_i64;
        let member_id = 95_002_002_i64;
        let ship_id = 880_002_001_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let base_ch = snap.characters.get(&90_000_001).cloned().expect("seeded char");
            for &cid in &[ceo_id, member_id] {
                let mut ch = base_ch.clone();
                ch.character_id = cid;
                ch.name = format!("Pilot {cid}");
                ch.corporation_id = corp_id;
                ch.solar_system_id = sys_id;
                ch.station_id = Some(station_id);
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(50_000_000);
                ch.clone_grade = 2;
                ch.neural = NeuralAttributes {
                    charisma: 20,
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    bonus_remaps: 1,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                };
                snap.characters.insert(cid, ch);
            }
            let mut members = HashSet::new();
            members.insert(ceo_id);
            members.insert(member_id);
            let mut member_roles = HashMap::new();
            member_roles.insert(ceo_id, u64::MAX);
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "R4 Gov Corp".into(),
                    ticker: "R4GC".into(),
                    ceo_id,
                    members,
                    member_roles,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 24698, // Drake (Battlecruiser, supports Command Bursts)
                    owner_id: ceo_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        let mut ceo_sess = make_session(ceo_id, corp_id, Some(station_id), sys_id, ship_id);
        ceo_sess.corp_role = u64::MAX;
        let kw = HashMap::new();

        // #187: SetShipKillCounter requires GM role
        let kill_non_gm = ctx
            .dispatch_rpc(
                &mut ceo_sess,
                "shipKillCounter",
                None,
                "SetShipKillCounter",
                &[PyRep::Integer(ship_id), PyRep::Integer(42)],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&kill_non_gm, Err(e) if e.code == "GMRoleRequired"),
            "expected GMRoleRequired on SetShipKillCounter without GM role, got {kill_non_gm:?}"
        );

        // #182: ResignFromCEO rejects new_ceo = 0 when corp has > 1 member, and clears former CEO's roles on valid succession
        let resign_zero = ctx
            .dispatch_rpc(
                &mut ceo_sess,
                "corpRegistry",
                None,
                "ResignFromCEO",
                &[PyRep::Integer(0)],
                &kw,
                None,
            )
            .await;
        assert!(
            matches!(&resign_zero, Err(e) if e.code == "InvalidNewCEO"),
            "expected InvalidNewCEO when resigning with 0 while other members exist, got {resign_zero:?}"
        );

        ctx.dispatch_rpc(
            &mut ceo_sess,
            "corpRegistry",
            None,
            "ResignFromCEO",
            &[PyRep::Integer(member_id)],
            &kw,
            None,
        )
        .await
        .expect("ResignFromCEO to valid member");
        assert_eq!(ceo_sess.corp_role, 0, "resigning CEO session.corp_role must be cleared");
        let former_ceo_roles = ctx.db.with_snapshot(|s| {
            s.corporations
                .get(&corp_id)
                .and_then(|c| c.member_roles.get(&ceo_id).copied())
                .unwrap_or(0)
        });
        assert_eq!(former_ceo_roles, 0, "resigning CEO member_roles must be cleared to 0");

        // #178: AbortTraining catches up accrued skill training before pausing
        let now_unix = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(ceo_id).or_default().insert(
                3300,
                TrainedSkillRecord {
                    type_id: 3300,
                    level: 0,
                    skill_points: 0.0,
                },
            );
            snap.skill_queues.insert(
                ceo_id,
                vec![SkillQueueEntry {
                    type_id: 3300,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    started_at_unix: Some(now_unix.saturating_sub(180)),
                    duration_seconds: 600.0,
                }],
            );
        });
        ctx.dispatch_rpc(&mut ceo_sess, "skillMgr2", None, "AbortTraining", &[], &kw, None)
            .await
            .expect("AbortTraining");
        let accrued_sp = ctx.db.with_snapshot(|s| {
            s.skills
                .get(&ceo_id)
                .and_then(|m| m.get(&3300))
                .map(|sk| sk.skill_points)
                .unwrap_or(0.0)
        });
        assert!(accrued_sp > 0.0, "AbortTraining must catch up and preserve accrued SP (got {accrued_sp})");

        // #179: Command Burst charge with quantity == 1 is consumed and removed on cycle
        let burst_mod_id = 880_002_050_i64;
        let burst_charge_id = 880_002_051_i64;
        let burst_mod_tid = 42526_u32; // Shield Command Burst I
        let burst_charge_tid = 42832_u32; // Active Shielding Charge
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                burst_mod_id,
                InventoryItemRecord {
                    item_id: burst_mod_id,
                    type_id: burst_mod_tid,
                    owner_id: ceo_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                burst_charge_id,
                InventoryItemRecord {
                    item_id: burst_charge_id,
                    type_id: burst_charge_tid,
                    owner_id: ceo_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(burst_mod_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let fit = ctx.get_or_rebuild_ship_fitting(ceo_id, ship_id).expect("fit");
        let mod_type = ctx.sde.types.get(&burst_mod_tid).expect("burst mod type");
        let mod_state = eve_dogma::DogmaItemState::from_sde(burst_mod_id, burst_mod_tid, &ctx.sde);
        eve_server::services::tactical_modules::activate_command_burst(
            &ctx,
            &sys_handle,
            ceo_id,
            ship_id,
            sys_id,
            burst_mod_id,
            mod_type,
            &mod_state,
            Some(burst_charge_tid),
            &fit,
        )
        .await
        .expect("activate_command_burst with 1 charge");
        let charge_still_exists = ctx.db.with_snapshot(|s| s.items.contains_key(&burst_charge_id));
        assert!(!charge_still_exists, "1-unit Command Burst charge must be consumed and removed");

        // #177: R&D StartResearch starts at 0.0 RP (no instant free RP on StartResearch)
        if let Some((&rnd_agent_id, rnd_agent)) = ctx.sde.agents.iter().find(|(_, a)| a.agent_type_id == 4) {
            let rnd_sys = rnd_agent.solar_system_id.unwrap_or(sys_id);
            let rnd_sta = rnd_agent.station_id.or(Some(station_id));
            let skill_tid = rnd_agent.research_skills.first().copied().unwrap_or(11442);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                    ch.solar_system_id = rnd_sys;
                    ch.station_id = rnd_sta;
                    ch.standings.insert(rnd_agent_id, 8.0);
                    ch.standings.insert(rnd_agent.corporation_id as i64, 8.0);
                }
                for sid in [3402, 3392, skill_tid] {
                    snap.skills.entry(ceo_id).or_default().insert(
                        sid,
                        TrainedSkillRecord {
                            type_id: sid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            });
            let mut rnd_sess = make_session(ceo_id, corp_id, rnd_sta, rnd_sys, ship_id);
            ctx.dispatch_rpc(
                &mut rnd_sess,
                "agentMgr",
                None,
                "DoAction",
                &[
                    PyRep::Integer(12),
                    PyRep::Integer(rnd_agent_id),
                    PyRep::Integer(skill_tid as i64),
                ],
                &kw,
                None,
            )
            .await
            .expect("StartResearch");
            // Immediately attempting to buy a datacore at t=0 must fail with InsufficientResearchPoints (0.0 initial RP)
            let buy_dc = ctx
                .dispatch_rpc(
                    &mut rnd_sess,
                    "agentMgr",
                    None,
                    "DoAction",
                    &[
                        PyRep::Integer(14),
                        PyRep::Integer(rnd_agent_id),
                        PyRep::Integer(1),
                    ],
                    &kw,
                    None,
                )
                .await;
            assert!(
                matches!(&buy_dc, Err(e) if e.code == "InsufficientResearchPoints"),
                "expected InsufficientResearchPoints at t=0 after StartResearch, got {buy_dc:?}"
            );
        }

        // #186: Locator agent rejects targets in Wormhole space (>= 31_000_000)
        if let Some((&loc_agent_id, loc_agent)) = ctx
            .sde
            .agents
            .iter()
            .find(|(_, a)| a.is_locator && a.level == 1 && a.solar_system_id.is_some())
        {
            let loc_sys = loc_agent.solar_system_id.unwrap();
            let loc_sta = loc_agent.station_id.or(Some(station_id));
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                    ch.solar_system_id = loc_sys;
                    ch.station_id = loc_sta;
                    ch.standings.insert(loc_agent_id, 5.0);
                    ch.standings.insert(loc_agent.corporation_id as i64, 5.0);
                }
                if let Some(target) = snap.characters.get_mut(&member_id) {
                    target.solar_system_id = 31_000_005; // J-space wormhole system
                    target.station_id = None;
                }
            });
            let mut loc_sess = make_session(ceo_id, corp_id, loc_sta, loc_sys, ship_id);
            let wh_locate = ctx
                .dispatch_rpc(
                    &mut loc_sess,
                    "agentMgr",
                    None,
                    "DoAction",
                    &[
                        PyRep::Integer(15),
                        PyRep::Integer(loc_agent_id),
                        PyRep::Integer(member_id),
                    ],
                    &kw,
                    None,
                )
                .await;
            assert!(
                matches!(&wh_locate, Err(e) if e.code == "LocatorTargetInUnknownSpace" || e.code == "TargetInUnreachableSpace"),
                "expected LocatorTargetInUnknownSpace when locating character in wormhole space, got {wh_locate:?}"
            );
        }
    }
}
