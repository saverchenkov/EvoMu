//! Integration tests for saved fitting legality, multifit, community fittings, and weapon linking.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    common::pin_clock(&ctx);
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
                ..Default::default()
            },
        );
    });
}

fn mk_fit(fid: i64, name: &str, ship_type: u32, owner: i64, items: Vec<(u32, InventoryFlag, i32)>) -> eve_db::SavedFittingRecord {
    eve_db::SavedFittingRecord { fitting_id: fid, name: name.into(), ship_type_id: ship_type, description: name.into(), owner_id: owner, items }
}

#[tokio::test]
async fn test_fitting_saved_multifit_and_community_fittings() {
    // parity: feat:fitting.fit-from-saved, rpc:corpFittingMgr.GetCommunityFittings
    eve_testkit::attest(&[
        "feat:fitting.fit-from-saved",
        "rpc:corpFittingMgr.GetCommunityFittings",
    ]);

    let ctx = new_ctx();
    let kw: HashMap<String, PyRep> = HashMap::new();
    let mut sess = SessionState::new(7112);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());
    let station_id = 60_003_760i64;
    sess.station_id = Some(station_id);

    // 1. Curated community fittings
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            101,
            eve_db::SavedFittingRecord {
                fitting_id: 101,
                name: "Community Starter Rifter".into(),
                ship_type_id: 587,
                description: "Curated community fit".into(),
                owner_id: 1, // Curated community
                items: vec![(486, InventoryFlag::HiSlot0, 1)],
            },
        );
        snap.saved_fittings.insert(
            102,
            eve_db::SavedFittingRecord {
                fitting_id: 102,
                name: "Corp Secret Fit".into(),
                ship_type_id: 587,
                description: "Corp fit".into(),
                owner_id: sess.corporation_id.unwrap_or(1000001),
                items: vec![(486, InventoryFlag::HiSlot0, 1)],
            },
        );
    });

    let comm_fittings = ctx
        .dispatch_rpc(&mut sess, "corpFittingMgr", None, "GetCommunityFittings", &[], &kw, None)
        .await
        .expect("GetCommunityFittings");

    let comm_str = format!("{comm_fittings:?}");
    assert!(comm_str.contains("Community Starter Rifter"));
    assert!(!comm_str.contains("Corp Secret Fit"), "Corp fittings must not appear in community store");

    // 2. Saved fitting: Rifter setup
    let fit_id = 201i64;
    let gun = 486u32; // 200mm AutoCannon I
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: fit_id,
                name: "Dual Gun Rifter".into(),
                ship_type_id: 587,
                description: "Dual autocannons".into(),
                owner_id: char_id,
                items: vec![
                    (gun, InventoryFlag::HiSlot0, 1),
                    (gun, InventoryFlag::HiSlot1, 1),
                ],
            },
        );
    });

    // Hull type mismatch error
    let merlin_id = 870_001i64;
    seed_item(&ctx, merlin_id, 603, char_id, station_id, InventoryFlag::Hangar, 1, true); // 603 = Merlin
    let mismatch_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[PyRep::List(vec![PyRep::Integer(merlin_id)]), PyRep::Integer(fit_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Hull type mismatch must fail");
    assert_eq!(mismatch_err.code, "UserError");
    assert!(mismatch_err.message.contains("ShipTypeMismatch"));

    // Hangar quantity summed across multiple stacks & multifit assembling packaged hulls
    let clean_hull1 = 870_011i64;
    let clean_hull2 = 870_012i64;
    seed_item(&ctx, clean_hull1, 587, char_id, station_id, InventoryFlag::Hangar, 1, false); // packaged
    seed_item(&ctx, clean_hull2, 587, char_id, station_id, InventoryFlag::Hangar, 1, false); // packaged

    // Seed 4 guns spread across 2 stacks in hangar
    let stack1 = 870_021i64;
    let stack2 = 870_022i64;
    seed_item(&ctx, stack1, gun, char_id, station_id, InventoryFlag::Hangar, 2, false);
    seed_item(&ctx, stack2, gun, char_id, station_id, InventoryFlag::Hangar, 2, false);

    let _ = ctx.drain_notifications_for_char(char_id);
    let mfit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[
                PyRep::List(vec![PyRep::Integer(clean_hull1), PyRep::Integer(clean_hull2)]),
                PyRep::Integer(fit_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Multifit must succeed");
    assert_eq!(mfit_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(2));

    // Both hulls must be assembled
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.get(&clean_hull1).unwrap().singleton);
        assert!(snap.items.get(&clean_hull2).unwrap().singleton);
    });

    // Notifications emitted: canonical OnMultipleItemChange with old location/flag
    let notifs = ctx.drain_notifications_for_char(char_id);
    let multi_notif = notifs.iter().find(|n| n.event == "OnMultipleItemChange").expect("OnMultipleItemChange emitted");
    let PyRep::List(ref moved_list) = multi_notif.args[0] else { panic!("expected items list"); };
    assert!(!moved_list.is_empty());
    for it in moved_list {
        assert_eq!(it.dict_get("ownerID").and_then(|v| v.as_i64()), Some(char_id));
    }
    let PyRep::Dict(ref change_dict) = multi_notif.args[1] else { panic!("expected change dict"); };
    assert!(change_dict.iter().any(|(k, v)| k.as_i64() == Some(3) && v.as_i64() == Some(station_id)));
    assert!(change_dict.iter().any(|(k, v)| k.as_i64() == Some(4) && v.as_i64() == Some(eve_db::InventoryFlag::Hangar as i64)));

    // Duplicate ship ID in target_ship_ids must be deduplicated: fittedCount == 1 and stock consumed once (NF-FIT-CLAIM)
    let dup_hull = 870_041i64;
    seed_item(&ctx, dup_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    let dup_mod = 870_042i64;
    seed_item(&ctx, dup_mod, gun, char_id, station_id, InventoryFlag::Hangar, 1, false);
    let dup_res = ctx
        .dispatch_rpc(
            &mut sess, "ship", None, "FitShips",
            &[PyRep::List(vec![PyRep::Integer(dup_hull), PyRep::Integer(dup_hull)]), PyRep::Integer(fit_id)],
            &kw, None,
        ).await.expect("Duplicate ship ID FitShips succeeds");
    assert_eq!(dup_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));
    ctx.db.with_snapshot(|snap| {
        let remaining_stock = snap.items.values().filter(|i| i.location_id == station_id && i.type_id == gun).map(|i| i.quantity).sum::<i32>();
        assert_eq!(remaining_stock, 0, "Stock must be consumed only once for deduplicated ship ID");
    });

    // Occupied slot rejection
    let occupied_hull = 870_031i64;
    seed_item(&ctx, occupied_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let existing_mod = 870_032i64;
    seed_item(&ctx, existing_mod, gun, char_id, occupied_hull, InventoryFlag::HiSlot0, 1, true);
    let occ_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[PyRep::List(vec![PyRep::Integer(occupied_hull)]), PyRep::Integer(fit_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Occupied slot must fail");
    assert_eq!(occ_err.code, "UserError");
    assert!(occ_err.message.contains("SlotAlreadyOccupied"));

    // Exceeding powergrid leaves module fitted offline
    let big_gun_fit_id = 202i64;
    let big_gun = 491u32; // 1400mm Howitzer Artillery (140 PG > 41 PG on Rifter)
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            big_gun_fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: big_gun_fit_id,
                name: "Overpower Rifter".into(),
                ship_type_id: 587,
                description: "Overpower".into(),
                owner_id: char_id,
                items: vec![(big_gun, InventoryFlag::HiSlot0, 1)],
            },
        );
    });
    let over_hull = 870_041i64;
    let big_gun_item = 870_042i64;
    seed_item(&ctx, over_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, big_gun_item, big_gun, char_id, station_id, InventoryFlag::Hangar, 1, false);

    let over_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[PyRep::List(vec![PyRep::Integer(over_hull)]), PyRep::Integer(big_gun_fit_id)],
            &kw,
            None,
        )
        .await
        .expect("FitShips with power overflow must succeed fitting offline");
    assert_eq!(over_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&big_gun_item), Some(&false), "Exceeding powergrid module must be offline");
    });

    // Exceeding hull slot counts rejection
    let invalid_slot_fit_id = 203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            invalid_slot_fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: invalid_slot_fit_id,
                name: "Too Many Slots".into(),
                ship_type_id: 587,
                description: "HiSlot7".into(),
                owner_id: char_id,
                items: vec![(gun, InventoryFlag::HiSlot7, 1)],
            },
        );
    });
    let slot_hull = 870_051i64;
    let slot_gun = 870_052i64;
    seed_item(&ctx, slot_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, slot_gun, gun, char_id, station_id, InventoryFlag::Hangar, 1, false);
    let slot_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[PyRep::List(vec![PyRep::Integer(slot_hull)]), PyRep::Integer(invalid_slot_fit_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Exceeding slot count must fail");
    assert_eq!(slot_err.code, "UserError");
    assert!(slot_err.message.contains("SlotIndexExceedsHullSlots"));
}

#[tokio::test]
async fn test_fitting_saved_sde_slot_effects_hardpoints_and_capacity_validation() {
    // parity: feat:fitting.fit-from-saved
    eve_testkit::attest(&["feat:fitting.fit-from-saved"]);

    let ctx = new_ctx();
    let kw: HashMap<String, PyRep> = HashMap::new();
    let mut sess = SessionState::new(7114);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix());
    let station_id = 60_003_760i64;
    sess.station_id = Some(station_id);

    let test_hull = 890_001i64;
    seed_item(&ctx, test_hull, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);

    // 1. Slot effect mismatch: med shield extender (3831, medPower 13) in low slot (flag 11)
    let bad_slot_fit = 301i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(bad_slot_fit, mk_fit(bad_slot_fit, "Bad Slot", 587, char_id, vec![(3831, InventoryFlag::LoSlot0, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(bad_slot_fit)], &kw, None).await.expect_err("Slot effect mismatch");
    assert_eq!(err.code, "UserError");
    assert!(err.message.contains("SlotEffectMismatch"));

    // 2. Rig size mismatch: Medium rig (4395, rigSize 2.0) on Small Rifter (rigSize 1.0)
    let bad_rig_fit = 302i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(bad_rig_fit, mk_fit(bad_rig_fit, "Bad Rig Size", 587, char_id, vec![(4395, InventoryFlag::RigSlot0, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(bad_rig_fit)], &kw, None).await.expect_err("Rig size mismatch");
    assert_eq!(err.code, "UserError");
    assert!(err.message.contains("RigSizeMismatch"));

    // 3. Rig calibration budget exceeded: 2x Drone Damage Rig II (26927, 300 cal each = 600 > 400 hull upgradeCapacity)
    let over_cal_fit = 303i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_cal_fit, mk_fit(over_cal_fit, "Over Cal", 587, char_id, vec![(26927, InventoryFlag::RigSlot0, 1), (26927, InventoryFlag::RigSlot1, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_cal_fit)], &kw, None).await.expect_err("Over calibration");
    assert_eq!(err.code, "UserError");
    assert!(err.message.contains("UpgradeCapacityExceeded"));

    // 4. Turret hardpoints limit exceeded: 3x 200mm AutoCannon I (486) on Probe (2 turret slots, 3 hi slots)
    let probe_hull = 890_002i64;
    seed_item(&ctx, probe_hull, 586, char_id, station_id, InventoryFlag::Hangar, 1, false);
    let over_turret_fit = 304i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_turret_fit, mk_fit(over_turret_fit, "Over Turrets", 586, char_id, vec![(486, InventoryFlag::HiSlot0, 1), (486, InventoryFlag::HiSlot1, 1), (486, InventoryFlag::HiSlot2, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(probe_hull)]), PyRep::Integer(over_turret_fit)], &kw, None).await.expect_err("Over turrets");
    assert_eq!(err.code, "UserError");
    assert!(err.message.contains("InsufficientTurretHardpoints"));

    // 5. Launcher hardpoints limit exceeded: 3x Light Missile Launcher I (499) on Rifter (2 launcher slots)
    let over_launcher_fit = 305i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_launcher_fit, mk_fit(over_launcher_fit, "Over Launchers", 587, char_id, vec![(499, InventoryFlag::HiSlot0, 1), (499, InventoryFlag::HiSlot1, 1), (499, InventoryFlag::HiSlot2, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_launcher_fit)], &kw, None).await.expect_err("Over launchers");
    assert_eq!(err.code, "UserError");
    assert!(err.message.contains("InsufficientLauncherHardpoints"));

    // 6. MaxGroupFitted exceeded: 2x Damage Control II (2048, maxGroupFitted 1544 = 1)
    let over_dc_fit = 306i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_dc_fit, mk_fit(over_dc_fit, "Dual DC", 587, char_id, vec![(2048, InventoryFlag::LoSlot0, 1), (2048, InventoryFlag::LoSlot1, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_dc_fit)], &kw, None).await.expect_err("Dual DC");
    assert_eq!(err.code, "UserError");
    assert!(err.message.contains("MaxGroupFittedExceeded"));

    // 7. Cargo capacity exceeded: 500 units of item 34 (1m3 each = 500m3 > 140m3 Rifter cargo)
    let over_cargo_fit = 307i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_cargo_fit, mk_fit(over_cargo_fit, "Over Cargo", 587, char_id, vec![(34, InventoryFlag::Cargo, 50_000)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_cargo_fit)], &kw, None).await.expect_err("Over cargo");
    assert_eq!(err.code, "CapacityExceeded");

    // 8. Drone bay category check: non-drone item (ammunition 34) in DroneBay (flag 87)
    let bad_drone_cat_fit = 308i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(bad_drone_cat_fit, mk_fit(bad_drone_cat_fit, "Ammo in Drone Bay", 587, char_id, vec![(34, InventoryFlag::DroneBay, 5)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(bad_drone_cat_fit)], &kw, None).await.expect_err("Ammo in drone bay");
    assert_eq!(err.code, "ItemNotADrone");

    // 9. Drone bay capacity exceeded: Rifter has droneCapacity == 0
    let over_drone_fit = 309i64;
    ctx.db.with_mut_snapshot(|s| { s.saved_fittings.insert(over_drone_fit, mk_fit(over_drone_fit, "Drones on Rifter", 587, char_id, vec![(2464, InventoryFlag::DroneBay, 1)])); });
    let err = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(test_hull)]), PyRep::Integer(over_drone_fit)], &kw, None).await.expect_err("Drones on Rifter");
    assert_eq!(err.code, "CapacityExceeded");
}

#[tokio::test]
async fn test_dogma_link_all_weapons_selective_grouping() {
    // parity: rpc:dogmaIM.LinkAllWeapons
    eve_testkit::attest(&["rpc:dogmaIM.LinkAllWeapons"]);

    let ctx = new_ctx();
    let kw: HashMap<String, PyRep> = HashMap::new();
    let mut sess = SessionState::new(7113);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let ship_id = 100_000_001i64;
    seed_item(&ctx, ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1, true);

    // Fit 2x 200mm AutoCannon II (2889), 2x Light Missile Launcher I (499), 1x Small Energy Neutralizer I (533)
    let ac1 = 880_001i64;
    let ac2 = 880_002i64;
    let lml1 = 880_003i64;
    let lml2 = 880_004i64;
    let neut1 = 880_005i64;

    seed_item(&ctx, ac1, 2889, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, ac2, 2889, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, lml1, 499, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    seed_item(&ctx, lml2, 499, char_id, ship_id, InventoryFlag::HiSlot3, 1, true);
    seed_item(&ctx, neut1, 533, char_id, ship_id, InventoryFlag::HiSlot4, 1, true);

    let link_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LinkAllWeapons",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("LinkAllWeapons");

    let PyRep::Dict(banks_dict) = link_res else {
        panic!("LinkAllWeapons must return dict, got {link_res:?}");
    };

    // Exactly 2 banks formed: AutoCannons and Launchers. Neut is not grouped.
    assert_eq!(banks_dict.len(), 2, "Only 2 banks should be created");

    let masters: Vec<i64> = banks_dict.iter().filter_map(|(k, _)| k.as_i64()).collect();
    assert!(masters.contains(&ac1) || masters.contains(&ac2));
    assert!(masters.contains(&lml1) || masters.contains(&lml2));
    assert!(!masters.contains(&neut1), "Neutralizer must not be grouped");

    ctx.db.with_snapshot(|snap| {
        let banks = snap.weapon_banks.get(&ship_id).expect("Persisted weapon banks");
        assert_eq!(banks.len(), 2);
    });
}

#[tokio::test]
async fn test_fitships_fitdata_flag_whitelist_and_specialized_holds() {
    let ctx = new_ctx();
    let mut sess = SessionState::new(7128);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let station_id = 60_003_760i64;
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });

    let ship_id = 870_050i64; // Venture (type 32880, has Ore Hold flag 134 capacity 5,000 m3)
    seed_item(&ctx, ship_id, 32880, char_id, station_id, InventoryFlag::Hangar, 1, true);

    // Helper to build fitData kw
    let fit_kw = |flg: i64, tid: i64, qty: i64| {
        let mut kw = HashMap::new();
        kw.insert("fitData".to_string(), PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(tid), PyRep::Integer(flg), PyRep::Integer(qty)]),
        ]));
        kw
    };

    // 1. Disallowed flag in fitData (flag 4 = Hangar) must fail with InvalidFittingFlag
    let err_flag = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(4, 2889, 1), None).await.expect_err("flag 4 must fail");
    assert_eq!(err_flag.code, "InvalidFittingFlag");

    // 2. Disallowed flag (flag 7) must fail with InvalidFittingFlag
    let err_flag7 = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(7, 2889, 1), None).await.expect_err("flag 7 must fail");
    assert_eq!(err_flag7.code, "InvalidFittingFlag");

    // 3. Incompatible item into specialized ore hold (flag 134 with module 2889) must fail with InvalidItemTypeForHold
    let err_type = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(134, 2889, 1), None).await.expect_err("Module in ore hold must fail");
    assert_eq!(err_type.code, "InvalidItemTypeForHold");

    // 4. Volume exceeding specialized hold capacity (ore volume 0.1 m3, qty 100,000 = 10,000 m3 > 5,000 m3 hold)
    let ore_tid = 1230; // Veldspar (category 25)
    let err_cap = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(134, ore_tid as i64, 100_000), None).await.expect_err("Excess ore volume must fail");
    assert_eq!(err_cap.code, "CapacityExceeded");

    // 5. Compatible specialized hold item fitting within capacity succeeds
    seed_item(&ctx, 870_051, ore_tid, char_id, station_id, InventoryFlag::Hangar, 1000, false);
    let ok_res = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(134, ore_tid as i64, 1000), None).await.expect("Fit ore within capacity");
    assert_eq!(ok_res.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(1));

    // 6. Tritanium (category 4) into flag 158 (FighterBay) must fail (NF-FIT-FLAG-BYPASS)
    let err_fighter = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(158, 34, 1), None).await.expect_err("Tritanium into FighterBay must fail");
    assert!(err_fighter.code == "ShipHasNoFighterBay" || err_fighter.code == "ItemNotAFighter");

    // 7. Structure service slot (flag 164) in fitData must fail with InvalidFittingFlag
    let err_service = ctx.dispatch_rpc(&mut sess, "ship", None, "FitShips", &[PyRep::List(vec![PyRep::Integer(ship_id)])], &fit_kw(164, 34, 1), None).await.expect_err("Structure service slot must fail");
    assert_eq!(err_service.code, "InvalidFittingFlag");
}

#[tokio::test]
async fn test_structure_fuel_bay_remote_proximity_and_fuel_type() {
    let ctx = new_ctx();
    let kw: HashMap<String, PyRep> = HashMap::new();
    let mut sess = SessionState::new(7135);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001i64;
    sess.character_id = Some(char_id);
    let sys_id = 30_000_142u32;
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;

    let ship_id = 870_060i64;
    sess.ship_id = Some(ship_id);
    seed_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    let struct_id = 1_000_000_000_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.structures.insert(struct_id, eve_db::UpwellStructureRecord {
            structure_id: struct_id, type_id: 35832, owner_corp_id: 2001, solar_system_id: sys_id,
            name: "Citadel".into(), position: [0.0, 0.0, 0.0], state: "ONLINE".into(),
            quantum_core_installed: true, fuel_blocks: 0, shield_hp: 100.0, armor_hp: 100.0,
            hull_hp: 100.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: struct_id, type_id: 35832, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 10_000_000.0, radius: 500.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    // Ship spawned at 10,000m (out of 5,000m range)
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let fuel_block = 870_061i64;
    seed_item(&ctx, fuel_block, 4051, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    let non_fuel = 870_062i64;
    seed_item(&ctx, non_fuel, 34, char_id, ship_id, InventoryFlag::Cargo, 100, false);

    // 1. Moving fuel into structure fuel bay while out of range (> 5,000m) must fail (NF-FUELBAY-REMOTE)
    let err_range = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "Add",
        &[PyRep::Integer(fuel_block), PyRep::Integer(struct_id), PyRep::Integer(133), PyRep::Integer(10)],
        &kw, None,
    ).await.expect_err("Out of range fuel move must fail");
    assert_eq!(err_range.code, "DestinationAccessDenied");

    // 2. Move ship into range (2,000m <= 5,000m)
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::new(2000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 3.2, mass: 1_250_000.0, radius: 35.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // 3. Moving non-fuel into structure fuel bay must fail with InvalidInventoryFlag (NF-FUELBAY-REMOTE)
    let err_type = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "Add",
        &[PyRep::Integer(non_fuel), PyRep::Integer(struct_id), PyRep::Integer(133), PyRep::Integer(100)],
        &kw, None,
    ).await.expect_err("Non-fuel into structure fuel bay must fail");
    assert_eq!(err_type.code, "InvalidInventoryFlag");

    // 4. Moving fuel into structure fuel bay while in range succeeds
    let ok_fuel = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "Add",
        &[PyRep::Integer(fuel_block), PyRep::Integer(struct_id), PyRep::Integer(133), PyRep::Integer(10)],
        &kw, None,
    ).await.expect("In-range fuel move must succeed");
    assert!(matches!(ok_fuel, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));
}
