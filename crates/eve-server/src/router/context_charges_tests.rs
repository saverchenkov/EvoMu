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
