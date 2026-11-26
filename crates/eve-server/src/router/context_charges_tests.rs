//! Unit and mutation-killing tests for `router::context_charges`.

use crate::router::context::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag == InventoryFlag::None,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn synthetic_type(base: &InvType, type_id: u32, group_id: u32, category_id: u32) -> InvType {
    let mut t = base.clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.default_attributes.clear();
    t
}

#[test]
fn is_charge_compatible_with_weapon_checks_category_groups_mining_crystals_and_size() {
    let ctx = test_ctx();
    let tpl = ctx.sde.get_type(34).expect("Tritanium exists");

    // 1. Non-charge category_id != 8 always rejected
    let mut w = synthetic_type(tpl, 28_748, 54, 7);
    let mut c_non_charge = synthetic_type(tpl, 9001, 482, 7);
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w, &c_non_charge));
    c_non_charge.category_id = 9;
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w, &c_non_charge));

    // 2. Deep Core Mining Laser II (28748) accepts group 482 and 663, rejects other groups if no 604..610
    let c_482 = synthetic_type(tpl, 9002, 482, 8);
    let c_663 = synthetic_type(tpl, 9003, 663, 8);
    let c_483 = synthetic_type(tpl, 9004, 483, 8);
    assert!(ServerContext::is_charge_compatible_with_weapon(&w, &c_482));
    assert!(ServerContext::is_charge_compatible_with_weapon(&w, &c_663));
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w, &c_483));

    // Non-28748 weapon without chargeGroup or launcherGroup does not get mining_crystal_ok
    w.type_id = 28_749;
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w, &c_482));

    // 3. Each chargeGroup1..5 attribute (604, 605, 606, 609, 610) matches charge.group_id
    for &attr_id in &[604_u16, 605, 606, 609, 610] {
        let mut w_cg = synthetic_type(tpl, 5000, 55, 7);
        w_cg.default_attributes.insert(attr_id, 83.0);
        let c_match = synthetic_type(tpl, 6000, 83, 8);
        let mut c_other = synthetic_type(tpl, 6001, 84, 8);
        // Even if launcherGroup (137) matches weapon.group_id, having 604..610 on weapon disables fallback
        c_other.default_attributes.insert(137, 55.0);
        assert!(
            ServerContext::is_charge_compatible_with_weapon(&w_cg, &c_match),
            "attr {attr_id} must allow matching charge group"
        );
        assert!(
            !ServerContext::is_charge_compatible_with_weapon(&w_cg, &c_other),
            "attr {attr_id} mismatch must reject even if launcherGroup matches"
        );
    }

    // 4. Fallback to charge launcherGroup (137) when weapon has none of 604..610
    let w_no_cg = synthetic_type(tpl, 5100, 508, 7);
    let mut c_launcher = synthetic_type(tpl, 6100, 999, 8);
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w_no_cg, &c_launcher));
    c_launcher.default_attributes.insert(137, 509.0);
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w_no_cg, &c_launcher));
    c_launcher.default_attributes.insert(137, 508.0);
    assert!(ServerContext::is_charge_compatible_with_weapon(&w_no_cg, &c_launcher));

    // 5. Charge size (128) comparison: (rs - cs).abs() < 0.5 when both have attr 128
    let mut w_sz = synthetic_type(tpl, 5200, 55, 7);
    w_sz.default_attributes.insert(604, 83.0);
    let mut c_sz = synthetic_type(tpl, 6200, 83, 8);

    // Only weapon has 128 -> size_ok is true
    w_sz.default_attributes.insert(128, 2.0);
    assert!(ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));

    // Both have 128: difference 0.0 -> true, 0.49 -> true, 0.5 -> false, 1.0 -> false
    c_sz.default_attributes.insert(128, 2.0);
    assert!(ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));
    c_sz.default_attributes.insert(128, 2.49);
    assert!(ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));
    c_sz.default_attributes.insert(128, 1.51);
    assert!(ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));
    c_sz.default_attributes.insert(128, 2.5);
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));
    c_sz.default_attributes.insert(128, 1.5);
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));
    c_sz.default_attributes.insert(128, 3.0);
    assert!(!ServerContext::is_charge_compatible_with_weapon(&w_sz, &c_sz));
}

#[test]
fn weapon_slot_max_charge_capacity_handles_positive_zero_and_tiny_volumes() {
    let ctx = test_ctx();
    let tpl = ctx.sde.get_type(34).expect("Tritanium exists");

    let mut w = synthetic_type(tpl, 7000, 55, 7);
    let mut c = synthetic_type(tpl, 7001, 83, 8);

    // Explicit positive capacity and volume: floor(12.0 / 0.25) = 48
    w.capacity = 12.0;
    c.volume = 0.25;
    assert_eq!(ServerContext::weapon_slot_max_charge_capacity(&w, &c), 48);

    // Non-positive weapon capacity falls back to 5.0 m3: floor(5.0 / 0.5) = 10
    w.capacity = 0.0;
    c.volume = 0.5;
    assert_eq!(ServerContext::weapon_slot_max_charge_capacity(&w, &c), 10);
    w.capacity = -3.0;
    assert_eq!(ServerContext::weapon_slot_max_charge_capacity(&w, &c), 10);

    // Charge volume larger than weapon capacity clamps to at least 1
    w.capacity = 2.0;
    c.volume = 10.0;
    assert_eq!(ServerContext::weapon_slot_max_charge_capacity(&w, &c), 1);

    // Zero or negative charge volume uses 1e-6 floor
    w.capacity = 1.0;
    c.volume = 0.0;
    assert_eq!(ServerContext::weapon_slot_max_charge_capacity(&w, &c), 1_000_000);
}

#[test]
fn load_charge_into_weapon_slot_error_branches_and_slot_boundaries() {
    let ctx = test_ctx();
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let dock = Some(station_id);
    let ship_id = 920_000_001_i64;

    // Invalid slot flags (< 11 or > 34)
    for bad_slot in [InventoryFlag::None, InventoryFlag::Hangar, InventoryFlag::Cargo, InventoryFlag::DroneBay] {
        let err = ctx
            .load_charge_into_weapon_slot(char1, ship_id, bad_slot, 1, dock)
            .expect_err("out-of-range slot flag must fail");
        assert!(format!("{err:?}").contains("InvalidWeaponSlot"));
    }

    // Missing ship
    let no_ship = ctx
        .load_charge_into_weapon_slot(char1, 999_888_777, InventoryFlag::LoSlot0, 1, dock)
        .expect_err("missing ship must fail");
    assert!(format!("{no_ship:?}").contains("ShipNotFound"));

    // Ship owned by someone else -> InvalidFittingTarget
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char2, station_id, InventoryFlag::Hangar, 1),
        );
    });
    let wrong_owner = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::LoSlot0, 1, dock)
        .expect_err("unowned ship must fail");
    assert!(format!("{wrong_owner:?}").contains("InvalidFittingTarget"));

    // Owned by char1 and colocated, but no weapon in slot 11 (LoSlot0) or 34 (HiSlot7) -> NoWeaponFitted
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char1) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char1, station_id, InventoryFlag::Hangar, 1),
        );
    });
    for valid_boundary_slot in [InventoryFlag::LoSlot0, InventoryFlag::HiSlot7] {
        let no_wep = ctx
            .load_charge_into_weapon_slot(char1, ship_id, valid_boundary_slot, 1, dock)
            .expect_err("empty weapon slot must fail with NoWeaponFitted");
        assert!(format!("{no_wep:?}").contains("NoWeaponFitted"));
    }

    // Find a valid projectile turret and two compatible charge types
    let (w_tid, c1_tid, c2_tid) = ctx
        .sde
        .types
        .values()
        .find_map(|w| {
            if w.category_id == 7 && w.group_id == 55 && w.capacity >= 5.0 {
                let charges: Vec<u32> = ctx
                    .sde
                    .types
                    .values()
                    .filter(|c| {
                        ServerContext::is_charge_compatible_with_weapon(w, c) && c.volume <= 0.05
                    })
                    .map(|c| c.type_id)
                    .take(2)
                    .collect();
                (charges.len() == 2).then(|| (w.type_id, charges[0], charges[1]))
            } else {
                None
            }
        })
        .expect("SDE has projectile turret and two ammo types");

    let wep_id = 920_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wep_id,
            make_item(wep_id, w_tid, char1, ship_id, InventoryFlag::HiSlot7, 1),
        );
        // Grant all required skills for w_tid, c1_tid, and c2_tid at level 5
        for tid in [w_tid, c1_tid, c2_tid] {
            for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                snap.skills.entry(char1).or_default().insert(
                    sk_tid,
                    TrainedSkillRecord {
                        type_id: sk_tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });

    // Missing charge item -> ItemNotFound
    let no_charge = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, 999_777_666, dock)
        .expect_err("missing charge item must fail");
    assert!(format!("{no_charge:?}").contains("ItemNotFound"));

    // Charge already in target slot -> returns early with unchanged version
    let already_id = 920_000_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut item = make_item(already_id, c1_tid, char1, ship_id, InventoryFlag::HiSlot7, 25);
        item.version = 7;
        snap.items.insert(already_id, item);
    });
    let (ret_id, ret_tid, ret_qty, ret_ver) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, already_id, dock)
        .expect("already-loaded charge returns Ok");
    assert_eq!((ret_id, ret_tid, ret_qty, ret_ver), (already_id, c1_tid, 25, 7));

    // Charge in unreachable station -> AmmoNotReachable
    let far_charge_id = 920_000_004_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&already_id);
        snap.items.insert(
            far_charge_id,
            make_item(far_charge_id, c1_tid, char1, 60_000_004, InventoryFlag::Hangar, 10),
        );
    });
    let unreachable = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, far_charge_id, dock)
        .expect_err("unreachable charge must fail");
    assert!(format!("{unreachable:?}").contains("AmmoNotReachable"));

    // Incompatible item type -> IncompatibleAmmo
    let bad_ammo_id = 920_000_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bad_ammo_id,
            make_item(bad_ammo_id, 34, char1, ship_id, InventoryFlag::Cargo, 10),
        );
    });
    let incompat = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, bad_ammo_id, dock)
        .expect_err("Tritanium is not compatible ammo");
    assert!(format!("{incompat:?}").contains("IncompatibleAmmo"));

    // Same charge type top-up: full stack merge, partial stack top-up, and room == 0 early return
    let max_cap = ServerContext::weapon_slot_max_charge_capacity(
        ctx.sde.get_type(w_tid).unwrap(),
        ctx.sde.get_type(c1_tid).unwrap(),
    );
    assert!(max_cap > 20);
    let slot_charge_id = 920_000_010_i64;
    let small_stack_id = 920_000_011_i64;
    let large_stack_id = 920_000_012_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            slot_charge_id,
            make_item(slot_charge_id, c1_tid, char1, ship_id, InventoryFlag::HiSlot7, 10),
        );
        snap.items.insert(
            small_stack_id,
            make_item(small_stack_id, c1_tid, char1, ship_id, InventoryFlag::Cargo, 5),
        );
        snap.items.insert(
            large_stack_id,
            make_item(large_stack_id, c1_tid, char1, ship_id, InventoryFlag::Cargo, max_cap + 50),
        );
    });

    // 1) Full merge of small_stack_id (5 units) into slot_charge_id (10 -> 15)
    let (id1, tid1, qty1, ver1) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, small_stack_id, dock)
        .expect("small stack merge must succeed");
    assert_eq!((id1, tid1, qty1, ver1), (slot_charge_id, c1_tid, 15, 2));
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&small_stack_id)));

    // 2) Partial top-up from large_stack_id (15 -> max_cap), leaving 65 in large_stack_id
    let (id2, tid2, qty2, ver2) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, large_stack_id, dock)
        .expect("partial top-up must succeed");
    assert_eq!((id2, tid2, qty2, ver2), (slot_charge_id, c1_tid, max_cap, 3));
    let rem_qty = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&large_stack_id).unwrap().quantity);
    assert_eq!(rem_qty, 65);

    // 3) room == 0 early return when already at max_cap
    let (id3, tid3, qty3, ver3) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, large_stack_id, dock)
        .expect("full slot early return must succeed");
    assert_eq!((id3, tid3, qty3, ver3), (slot_charge_id, c1_tid, max_cap, 3));
    let rem_qty_after = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&large_stack_id).unwrap().quantity);
    assert_eq!(rem_qty_after, 65);

    // 4) Swap to different charge type c2_tid when Cargo already has a stack of c1_tid (large_stack_id):
    //    slot_charge_id (max_cap units of c1_tid) merges into large_stack_id in Cargo and is removed!
    let c2_small_id = 920_000_013_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            c2_small_id,
            make_item(c2_small_id, c2_tid, char1, ship_id, InventoryFlag::Cargo, 12),
        );
    });
    let (id4, tid4, qty4, ver4) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, c2_small_id, dock)
        .expect("swapping to c2_small_id must succeed");
    assert_eq!((id4, tid4, qty4, ver4), (c2_small_id, c2_tid, 12, 2));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&slot_charge_id));
        assert_eq!(snap.items.get(&large_stack_id).unwrap().quantity, 65 + max_cap);
        let loaded = snap.items.get(&c2_small_id).unwrap();
        assert_eq!(loaded.location_id, ship_id);
        assert_eq!(loaded.flag, InventoryFlag::HiSlot7);
        assert!(!loaded.singleton);
    });
}

#[test]
fn load_charge_into_weapon_slot_emits_exact_item_change_diffs_and_splits_from_hangar() {
    let ctx = test_ctx();
    let char1 = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let dock = Some(station_id);
    let ship_id = 921_000_001_i64;
    let wep_id = 921_000_002_i64;

    let (w_tid, c1_tid, c2_tid) = ctx
        .sde
        .types
        .values()
        .find_map(|w| {
            if w.category_id == 7 && w.group_id == 55 && w.capacity >= 5.0 {
                let charges: Vec<u32> = ctx
                    .sde
                    .types
                    .values()
                    .filter(|c| ServerContext::is_charge_compatible_with_weapon(w, c) && c.volume <= 0.05)
                    .map(|c| c.type_id)
                    .take(2)
                    .collect();
                (charges.len() == 2).then(|| (w.type_id, charges[0], charges[1]))
            } else {
                None
            }
        })
        .expect("SDE has projectile turret and two ammo types");

    let max_cap = ServerContext::weapon_slot_max_charge_capacity(
        ctx.sde.get_type(w_tid).unwrap(),
        ctx.sde.get_type(c1_tid).unwrap(),
    );
    let hangar_stack_id = 921_000_010_i64;
    let c2_cargo_id = 921_000_011_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char1) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char1, station_id, InventoryFlag::Hangar, 1));
        snap.items.insert(wep_id, make_item(wep_id, w_tid, char1, ship_id, InventoryFlag::HiSlot7, 1));
        // Oversized stack in Station Hangar (location_id != ship_id) so pre_map MUST capture it via i.item_id == charge_item_id
        snap.items.insert(
            hangar_stack_id,
            make_item(hangar_stack_id, c1_tid, char1, station_id, InventoryFlag::Hangar, max_cap + 17),
        );
        snap.items.insert(
            c2_cargo_id,
            make_item(c2_cargo_id, c2_tid, char1, ship_id, InventoryFlag::Cargo, 9),
        );
        for tid in [w_tid, c1_tid, c2_tid] {
            for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                snap.skills.entry(char1).or_default().insert(
                    sk_tid,
                    TrainedSkillRecord { type_id: sk_tid, level: 5, skill_points: 256_000.0 },
                );
            }
        }
    });

    let uid = ctx.db.with_snapshot(|s| s.characters[&char1].user_id);
    ctx.db
        .acquire_online_lock(uid, 501, Some(char1), true, ctx.now_unix())
        .expect("online lock");

    // Helper: extract (item_id, change_cols_dict) from OnItemChange notifications
    let item_changes = |notifs: Vec<crate::router::PendingMachoNotification>| -> HashMap<i64, Vec<(i64, i64)>> {
        let mut out = HashMap::new();
        for n in notifs {
            if n.event != "OnItemChange" {
                continue;
            }
            let iid = match &n.args[0] {
                eve_proto::PyRep::DBRow { columns, .. } => columns
                    .iter()
                    .find(|(k, _)| k == "itemID")
                    .and_then(|(_, v)| v.as_i64())
                    .unwrap(),
                _ => panic!("expected DBRow"),
            };
            let cols = match &n.args[1] {
                eve_proto::PyRep::Dict(entries) => entries
                    .iter()
                    .filter_map(|(k, v)| Some((k.as_i64()?, v.as_i64()?)))
                    .collect(),
                _ => panic!("expected Dict"),
            };
            out.insert(iid, cols);
        }
        out
    };

    // 1) Split oversized Hangar stack into an EMPTY weapon slot -> brand new loaded_id (`!touched_ids.contains(&loaded.0)`)
    let _ = ctx.drain_notifications_for_char(char1);
    let (new_loaded_id, tid_out, qty_out, ver_out) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, hangar_stack_id, dock)
        .expect("split from hangar into empty weapon slot");
    assert_eq!((tid_out, qty_out, ver_out), (c1_tid, max_cap, 1));
    assert_ne!(new_loaded_id, hangar_stack_id);
    let ch1 = item_changes(ctx.drain_notifications_for_char(char1));
    // Hangar stack MUST have pre captured -> exact delta column (9 = ixStackSize -> max_cap + 17)
    assert_eq!(ch1.get(&hangar_stack_id), Some(&vec![(9, (max_cap + 17) as i64)]));
    // New loaded stack MUST be in notifications with create diff `ixLocationID=3 -> 0` (#1026)
    assert_eq!(ch1.get(&new_loaded_id), Some(&vec![(3, 0)]));

    // 2) Swap to c2_tid when NO c1_tid exists in Cargo (moves new_loaded_id to Cargo, c2_cargo_id to HiSlot7)
    let _ = ctx.drain_notifications_for_char(char1);
    let (swapped_id, swapped_tid, swapped_qty, swapped_ver) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, c2_cargo_id, dock)
        .expect("swap charge into weapon slot moving previous to cargo");
    assert_eq!((swapped_id, swapped_tid, swapped_qty, swapped_ver), (c2_cargo_id, c2_tid, 9, 2));
    let ch2 = item_changes(ctx.drain_notifications_for_char(char1));
    // Old loaded charge had pre captured in HiSlot7 -> change col 4 (ixFlag = HiSlot7 = 34)
    assert_eq!(
        ch2.get(&new_loaded_id),
        Some(&vec![(4, InventoryFlag::HiSlot7 as i64)])
    );
    // New charge from Cargo had pre captured -> change col 4 (ixFlag = Cargo = 5)
    assert_eq!(
        ch2.get(&c2_cargo_id),
        Some(&vec![(4, InventoryFlag::Cargo as i64)])
    );

    // 3) Swap BACK to c1_tid (using remainder in hangar_stack_id) when Cargo ALSO already has a stack of c2_tid!
    // This exercises pre_map capture for BOTH `i.flag == flag` (c2_cargo_id) AND `i.flag == Cargo` (c2_cargo_merge_id).
    let c2_cargo_merge_id = 921_000_012_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            c2_cargo_merge_id,
            make_item(c2_cargo_merge_id, c2_tid, char1, ship_id, InventoryFlag::Cargo, 4),
        );
    });
    let _ = ctx.drain_notifications_for_char(char1);
    let (back_id, back_tid, back_qty, back_ver) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot7, hangar_stack_id, dock)
        .expect("swap back merging old weapon charge into Cargo stack");
    assert_eq!((back_id, back_tid, back_qty, back_ver), (hangar_stack_id, c1_tid, 17, 3));
    let ch3 = item_changes(ctx.drain_notifications_for_char(char1));
    // Existing Cargo merge target stack had pre captured (quantity was 4 before +9 = 13) -> col 9 = 4
    assert_eq!(ch3.get(&c2_cargo_merge_id), Some(&vec![(9, 4)]));
    // Unloaded old weapon charge c2_cargo_id was deleted into Cargo merge -> tombstone (loc 6, col 9 = 9)
    assert_eq!(
        ch3.get(&c2_cargo_id),
        Some(&vec![
            (3, ship_id),
            (4, InventoryFlag::HiSlot7 as i64),
            (9, 9),
        ])
    );
    // Hangar stack moved into weapon slot -> cols 3 (station_id) & 4 (Hangar)
    assert_eq!(
        ch3.get(&hangar_stack_id),
        Some(&vec![(3, station_id), (4, InventoryFlag::Hangar as i64)])
    );
}

#[test]
fn load_charge_enforces_item_class_policy_atomically_and_preserves_crystal_and_script_state() {
    let ctx = test_ctx();
    let char1 = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let dock = Some(station_id);
    let ship_id = 922_000_001_i64;
    let turret_id = 922_000_002_i64;
    let laser_id = 922_000_003_i64;
    let sebo_id = 922_000_004_i64;

    // 1. Projectile turret (484 = 125mm Gatling AutoCannon I, ammo 178 = Carbonized Lead S)
    // 2. Energy laser (450 = Gatling Pulse Laser I, crystals 239 = Radio S, 246 = Multifrequency S)
    // 3. Sensor Booster (1973 = Sensor Booster I, script 29011 = Scan Resolution Script)
    let w_proj_tid = 484_u32;
    let c_proj_tid = 178_u32;
    let w_laser_tid = 450_u32;
    let c_crys1_tid = 239_u32;
    let c_crys2_tid = 246_u32;
    let w_sebo_tid = 1973_u32;
    let c_script_tid = 29_011_u32;

    let loaded_proj_id = 922_000_010_i64;
    let bpc_ammo_id = 922_000_011_i64;
    let mutaplasmid_ammo_id = 922_000_012_i64;
    let attr_mutated_ammo_id = 922_000_013_i64;
    let insured_ammo_id = 922_000_014_i64;
    let courier_ammo_id = 922_000_015_i64;
    let singleton_ammo_id = 922_000_016_i64;

    let crystal_stack_id = 922_000_020_i64;
    let worn_crystal_id = 922_000_021_i64;
    let alt_crystal_id = 922_000_022_i64;
    let mut_crystal_id = 922_000_023_i64;
    let ins_crystal_id = 922_000_024_i64;
    let wear_ns_crystal_id = 922_000_025_i64;

    let script_id = 922_000_030_i64;
    let mut_script_id = 922_000_031_i64;
    let ins_script_id = 922_000_032_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char1) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char1, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            turret_id,
            make_item(turret_id, w_proj_tid, char1, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            laser_id,
            make_item(laser_id, w_laser_tid, char1, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            sebo_id,
            make_item(sebo_id, w_sebo_tid, char1, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.items.insert(
            loaded_proj_id,
            make_item(loaded_proj_id, c_proj_tid, char1, ship_id, InventoryFlag::HiSlot0, 25),
        );

        // Bad stackable ammo variants in Cargo
        let mut bpc = make_item(bpc_ammo_id, c_proj_tid, char1, ship_id, InventoryFlag::Cargo, 10);
        bpc.is_blueprint_copy = true;
        bpc.blueprint_runs_remaining = Some(3);
        snap.items.insert(bpc_ammo_id, bpc);

        let mut mut_m = make_item(mutaplasmid_ammo_id, c_proj_tid, char1, ship_id, InventoryFlag::Cargo, 10);
        mut_m.source_mutaplasmid_id = Some(47_740);
        snap.items.insert(mutaplasmid_ammo_id, mut_m);

        let mut mut_a = make_item(attr_mutated_ammo_id, c_proj_tid, char1, ship_id, InventoryFlag::Cargo, 10);
        mut_a.dynamic_attributes.insert(51, 99.0);
        snap.items.insert(attr_mutated_ammo_id, mut_a);

        let mut ins = make_item(insured_ammo_id, c_proj_tid, char1, ship_id, InventoryFlag::Cargo, 10);
        ins.insured_payout_isk = 500.0;
        snap.items.insert(insured_ammo_id, ins);

        snap.items.insert(
            courier_ammo_id,
            make_item(courier_ammo_id, c_proj_tid, char1, ship_id, InventoryFlag::Cargo, 10),
        );
        snap.item_custom_names.insert(courier_ammo_id, "courier_cargo:9001".to_string());

        let mut sing = make_item(singleton_ammo_id, c_proj_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        sing.singleton = true;
        snap.items.insert(singleton_ammo_id, sing);

        // Crystal fixtures
        snap.items.insert(
            crystal_stack_id,
            make_item(crystal_stack_id, c_crys1_tid, char1, ship_id, InventoryFlag::Cargo, 5),
        );

        let mut worn = make_item(worn_crystal_id, c_crys1_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        worn.singleton = true;
        worn.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.5);
        snap.items.insert(worn_crystal_id, worn);

        let mut wear_ns = make_item(wear_ns_crystal_id, c_crys1_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        wear_ns.singleton = false;
        wear_ns.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.25);
        snap.items.insert(wear_ns_crystal_id, wear_ns);

        snap.items.insert(
            alt_crystal_id,
            make_item(alt_crystal_id, c_crys2_tid, char1, ship_id, InventoryFlag::Cargo, 3),
        );

        let mut mut_c = make_item(mut_crystal_id, c_crys1_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        mut_c.singleton = true;
        mut_c.source_mutaplasmid_id = Some(47_740);
        mut_c.dynamic_attributes.insert(51, 999.0);
        snap.items.insert(mut_crystal_id, mut_c);

        let mut ins_c = make_item(ins_crystal_id, c_crys1_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        ins_c.singleton = true;
        ins_c.insured_payout_isk = 250.0;
        snap.items.insert(ins_crystal_id, ins_c);

        // Script fixtures
        let mut s_ok = make_item(script_id, c_script_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        s_ok.singleton = true;
        snap.items.insert(script_id, s_ok);

        let mut s_mut = make_item(mut_script_id, c_script_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        s_mut.singleton = true;
        s_mut.dynamic_attributes.insert(51, 999.0);
        snap.items.insert(mut_script_id, s_mut);

        let mut s_ins = make_item(ins_script_id, c_script_tid, char1, ship_id, InventoryFlag::Cargo, 1);
        s_ins.singleton = true;
        s_ins.insured_payout_isk = 100.0;
        snap.items.insert(ins_script_id, s_ins);

        for tid in [
            w_proj_tid,
            c_proj_tid,
            w_laser_tid,
            c_crys1_tid,
            c_crys2_tid,
            w_sebo_tid,
            c_script_tid,
        ] {
            for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                snap.skills.entry(char1).or_default().insert(
                    sk_tid,
                    TrainedSkillRecord {
                        type_id: sk_tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });

    // 1. Atomic rejection of invalid charges: loaded charge in HiSlot0 stays untouched!
    for bad_id in [
        bpc_ammo_id,
        mutaplasmid_ammo_id,
        attr_mutated_ammo_id,
        insured_ammo_id,
        courier_ammo_id,
        singleton_ammo_id,
    ] {
        let err = ctx
            .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot0, bad_id, dock)
            .expect_err("invalid charge must be rejected");
        assert_eq!(err.code, "FittingValidationFailed");
        ctx.db.with_snapshot(|snap| {
            let cur = &snap.items[&loaded_proj_id];
            assert_eq!(cur.location_id, ship_id);
            assert_eq!(cur.flag, InventoryFlag::HiSlot0);
            assert_eq!(cur.quantity, 25);
            assert_eq!(cur.version, 1);
            assert_eq!(snap.items[&bad_id].flag, InventoryFlag::Cargo);
        });
    }

    // 2. Loading crystal from stack (qty=5) creates singleton=true row (qty=1) & decrements stack (5->4)
    let (c1_id, c1_tid, c1_qty, c1_ver) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot1, crystal_stack_id, dock)
        .expect("loading crystal from pristine stack succeeds");
    assert_ne!(c1_id, crystal_stack_id);
    assert_eq!((c1_tid, c1_qty, c1_ver), (c_crys1_tid, 1, 1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&crystal_stack_id].quantity, 4);
        let loaded = &snap.items[&c1_id];
        assert_eq!((loaded.location_id, loaded.flag, loaded.quantity, loaded.singleton), (ship_id, InventoryFlag::HiSlot1, 1, true));
    });

    // 3. Atomic rejection of mutated & insured crystals keeps c1_id untouched in HiSlot1
    for bad_crys in [mut_crystal_id, ins_crystal_id] {
        let err = ctx
            .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot1, bad_crys, dock)
            .expect_err("mutated or insured crystal must fail");
        assert_eq!(err.code, "FittingValidationFailed");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items[&c1_id].flag, InventoryFlag::HiSlot1);
            assert_eq!(snap.items[&bad_crys].flag, InventoryFlag::Cargo);
        });
    }

    // 4. Reload worn singleton=true crystal into HiSlot1 -> swaps c1_id to Cargo (without merging into crystal_stack_id!)
    //    and preserves worn_crystal_id's DAMAGE=0.5 + singleton=true
    let (w1_id, w1_tid, w1_qty, _) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot1, worn_crystal_id, dock)
        .expect("loading worn crystal succeeds");
    assert_eq!((w1_id, w1_tid, w1_qty), (worn_crystal_id, c_crys1_tid, 1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&crystal_stack_id].quantity, 4);
        assert_eq!((snap.items[&c1_id].flag, snap.items[&c1_id].singleton), (InventoryFlag::Cargo, true));
        let loaded = &snap.items[&worn_crystal_id];
        assert_eq!((loaded.flag, loaded.singleton), (InventoryFlag::HiSlot1, true));
        assert_eq!(loaded.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE), Some(&0.5));
    });

    // 5. Unload worn crystal by swapping in alt_crystal_id (when crystal_stack_id of c_crys1_tid is in Cargo):
    //    worn crystal must NOT merge into pristine crystal_stack_id and must preserve singleton=true + DAMAGE=0.5
    let (alt_loaded_id, _, alt_qty, _) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot1, alt_crystal_id, dock)
        .expect("swap worn crystal to alt crystal succeeds");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&crystal_stack_id].quantity, 4);
        let unloaded = &snap.items[&worn_crystal_id];
        assert_eq!((unloaded.flag, unloaded.singleton), (InventoryFlag::Cargo, true));
        assert_eq!(unloaded.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE), Some(&0.5));
        assert_eq!(snap.items[&alt_crystal_id].quantity, 2);
        assert_eq!((snap.items[&alt_loaded_id].flag, snap.items[&alt_loaded_id].singleton, alt_qty), (InventoryFlag::HiSlot1, true, 1));
    });

    // 6. Wear-only non-singleton crystal reloads cleanly and self-heals singleton=true
    let (ns_id, _, ns_qty, _) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::HiSlot1, wear_ns_crystal_id, dock)
        .expect("wear-only non-singleton crystal loads and self-heals");
    assert_eq!((ns_id, ns_qty), (wear_ns_crystal_id, 1));
    ctx.db.with_snapshot(|snap| {
        let loaded = &snap.items[&wear_ns_crystal_id];
        assert_eq!((loaded.flag, loaded.singleton), (InventoryFlag::HiSlot1, true));
        assert_eq!(loaded.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE), Some(&0.25));
    });

    // 7. Single singleton=true script loads cleanly while mutated/insured scripts fail atomically
    for bad_script in [mut_script_id, ins_script_id] {
        let err = ctx
            .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::MedSlot0, bad_script, dock)
            .expect_err("mutated or insured script must fail");
        assert_eq!(err.code, "FittingValidationFailed");
    }
    let (s_out_id, s_out_tid, s_out_qty, _) = ctx
        .load_charge_into_weapon_slot(char1, ship_id, InventoryFlag::MedSlot0, script_id, dock)
        .expect("clean singleton script loads into MedSlot0");
    assert_eq!((s_out_id, s_out_tid, s_out_qty), (script_id, c_script_tid, 1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            (snap.items[&script_id].flag, snap.items[&script_id].singleton),
            (InventoryFlag::MedSlot0, true)
        );
    });
}
