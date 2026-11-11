//! Deep branch-coverage and mutation-killing unit tests for `structure_weapons_ops`.

#![forbid(unsafe_code)]

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use crate::services::structure_weapons_ops::*;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn target_locking_error_branches_structure_vs_structure_and_max_locked_targets() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();

    let gunner_id = 950_001_i64;
    let hostile_id = 950_002_i64;
    let corp_id = 98_950_001_i64;
    let st_id = 1_000_000_950_001_i64;
    let other_st_id = 1_000_000_950_002_i64;
    let ship_id = 950_100_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let g = clone_char_with(snap, gunner_id, "Gunner", corp_id, null_sys, st_id);
        let h = clone_char_with(snap, hostile_id, "Hostile", 98_950_002, null_sys, ship_id);
        snap.characters.insert(gunner_id, g);
        snap.characters.insert(hostile_id, h);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = gunner_id;
                __corp },
        );
        for (sid, pos) in [(st_id, [0.0, 0.0, 0.0]), (other_st_id, [50_000.0, 0.0, 0.0])] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
                    solar_system_id: null_sys,
                    name: format!("S-{sid}"),
                    position: pos,
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner_id);
        save_ext_state(snap, &ext);
    });

    let sess = SessionState::new(1);

    // 1. StructureNotFound
    assert_eq!(
        handle_structure_add_target(&ctx, &sess, gunner_id, 999_999_999, ship_id)
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    // 2. Non-operational states rejected with StructureNotOperational
    for bad_state in ["Destroyed", "Unanchored", "AnchorVulnerable", "Anchoring", "NoCore"] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&st_id).unwrap().state = bad_state.into();
        });
        assert_eq!(
            handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
                .await
                .unwrap_err()
                .code,
            "StructureNotOperational"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().state = "ShieldVulnerable".into();
    });

    // 3. Target ball not on grid -> TargetingFailed
    assert_eq!(
        handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );

    // 4. Target is another Upwell structure -> auto-spawns target structure ball and locks it!
    assert!(
        handle_structure_add_target(&ctx, &sess, gunner_id, st_id, other_st_id)
            .await
            .is_ok()
    );

    // 5. Spawn ship ball: tethered or un-gate-cloaked rejected with TargetInvulnerable; out-of-range rejected with TargetingFailed
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(20_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                is_tethered: true,
                ..CombatState::new()
            },
        )
        .await;
    assert_eq!(
        handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );

    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_tethered = false;
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    });
    assert_eq!(
        handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );

    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
    });
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(900_000.0, 0.0, 0.0);
    }
    assert_eq!(
        handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );

    // 6. Fill target_locks to max_locks -> MaxLockedTargets
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(20_000.0, 0.0, 0.0);
        let locks = bp.target_locks.entry(st_id).or_default();
        for idx in 0..10 {
            locks.push(TargetLock {
                target_id: 800_000 + idx,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(300_000.0),
            });
        }
    }
    assert_eq!(
        handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
            .await
            .unwrap_err()
            .code,
        "MaxLockedTargets"
    );
}

#[tokio::test]
async fn standup_weapon_highsec_superweapon_charge_capacitor_and_burst_projector_branches() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap();
    let high_sys = 30_000_142_u32;

    let find_mod_of_group = |grp: u32| -> u32 {
        ctx.sde
            .types
            .values()
            .filter(|t| t.group_id == grp)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap()
    };
    let superweapon_tid = find_mod_of_group(1333);
    let burst_projector_tid = find_mod_of_group(1331);
    let launcher_tid = find_mod_of_group(1327);
    let req_charge_grp = ctx.sde.types[&launcher_tid]
        .default_attributes
        .get(&604)
        .copied()
        .unwrap_or(0.0) as u32;
    let valid_charge_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 8 && t.group_id == req_charge_grp)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap();
    let wrong_charge_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 8 && t.group_id != req_charge_grp)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap();
    let unsupported_mod_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 66
                && !matches!(t.group_id, 1327 | 1328 | 1330 | 1331 | 1333 | 1562)
                && t.default_attributes.get(&604).copied().unwrap_or(0.0) <= 0.0
                && t.default_attributes.get(&6).copied().unwrap_or(0.0) <= 500_000.0
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(35_892);

    let gunner_id = 951_001_i64;
    let hostile_id = 951_002_i64;
    let corp_id = 98_951_001_i64;
    let st_id = 1_000_000_951_001_i64;
    let ship_id = 951_100_i64;
    let mod_super = 951_201_i64;
    let mod_burst = 951_202_i64;
    let mod_launch = 951_203_i64;
    let mod_unsup = 951_204_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let g = clone_char_with(snap, gunner_id, "Gunner", corp_id, high_sys, st_id);
        let h = clone_char_with(snap, hostile_id, "Hostile", 98_951_002, null_sys, ship_id);
        snap.characters.insert(gunner_id, g);
        snap.characters.insert(hostile_id, h);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = gunner_id;
                __corp },
        );
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832, // Astrahus (non-Keepstar)
                owner_corp_id: corp_id,
                solar_system_id: high_sys,
                name: "Test Structure".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (mid, tid, flg) in [
            (mod_super, superweapon_tid, InventoryFlag::HiSlot0),
            (mod_burst, burst_projector_tid, InventoryFlag::HiSlot1),
            (mod_launch, launcher_tid, InventoryFlag::HiSlot2),
            (mod_unsup, unsupported_mod_tid, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: st_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner_id);
        save_ext_state(snap, &ext);
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(gunner_id as u64);
    sess.character_id = Some(gunner_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(high_sys);
    sess.ship_id = Some(st_id);

    // 1. Superweapon on Astrahus -> SuperweaponRequiresKeepstar
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_super, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "SuperweaponRequiresKeepstar"
    );

    // 2. Burst projector in High-Sec -> IllegalHighSecAoEWeapon
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_burst, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "IllegalHighSecAoEWeapon"
    );

    // Move structure to null_sys and upgrade to Keepstar (35_834)
    sess.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&st_id).unwrap();
        st.solar_system_id = null_sys;
        st.type_id = 35_834;
        snap.characters.get_mut(&gunner_id).unwrap().solar_system_id = null_sys;
        snap.module_online_states.insert(mod_launch, false);
    });

    // 3. ModuleNotFound and ModuleOffline
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, 999_999, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "ModuleOffline"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_launch, true);
    });

    // 4. TargetRequired & TargetNotLocked
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", None, false)
            .await
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );

    // Spawn target ship, lock it on structure
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 5_000.0,
                max_capacitor: 5_000.0,
                shield_hp: 20_000.0,
                max_shield_hp: 20_000.0,
                armor_hp: 20_000.0,
                max_armor_hp: 20_000.0,
                structure_hp: 20_000.0,
                max_structure_hp: 20_000.0,
                ..CombatState::new()
            },
        )
        .await;
    handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
        .await
        .unwrap();
    for _ in 0..60 {
        if sys_handle
            .get_target_locks(st_id)
            .await
            .unwrap_or_default()
            .iter()
            .any(|l| l.target_id == ship_id && l.locked)
        {
            break;
        }
        sys_handle.step_manual(1.0).await.unwrap();
    }

    // 5. OutOfRange check
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(800_000.0, 0.0, 0.0);
    }
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(15_000.0, 0.0, 0.0);
    }

    // 6. NoChargesLoaded & InvalidChargeGroup
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "NoChargesLoaded"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            951_301,
            InventoryItemRecord {
                item_id: 951_301,
                type_id: wrong_charge_tid,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 5,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
            .await
            .unwrap_err()
            .code,
        "InvalidChargeGroup"
    );

    // 7. NotEnoughCapacitor when capacitor drained to 0 at current timestamp
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&951_301);
        let mut ext = load_ext_state(snap);
        ext.structure_cap_state.insert(st_id, (0.0, 1_800_000_000));
        save_ext_state(snap, &ext);
    });
    if ctx.sde.types[&launcher_tid].default_attributes.get(&6).copied().unwrap_or(0.0) > 0.0 {
        assert_eq!(
            handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
                .await
                .unwrap_err()
                .code,
            "NotEnoughCapacitor"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.structure_cap_state.insert(st_id, (500_000.0, 1_800_000_000));
        save_ext_state(snap, &ext);
    });

    // 8. Activate Burst Projector (group 1331) with ammo in SpecializedAmmoHold -> hits ship_id in AoE!
    let burst_charge_grp = ctx.sde.types[&burst_projector_tid]
        .default_attributes
        .get(&604)
        .copied()
        .unwrap_or(0.0) as u32;
    if burst_charge_grp > 0 {
        let burst_charge_tid = ctx
            .sde
            .types
            .values()
            .filter(|t| t.category_id == 8 && t.group_id == burst_charge_grp)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                951_302,
                InventoryItemRecord {
                    item_id: 951_302,
                    type_id: burst_charge_tid,
                    owner_id: corp_id,
                    location_id: st_id,
                    flag: InventoryFlag::SpecializedAmmoHold,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
    }
    let PyRep::KeyVal(burst_kv) = handle_structure_module_activate(
        &ctx,
        &mut sess,
        gunner_id,
        st_id,
        mod_burst,
        "fx",
        Some(ship_id),
        false,
    )
    .await
    .expect("burst projector activates") else {
        panic!("expected keyval");
    };
    assert_eq!(burst_kv.get("targetsHit"), Some(&PyRep::Integer(1)));

    // 9. Unsupported module group -> UnsupportedStructureModule
    let _ = valid_charge_tid;
    assert_eq!(
        handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_unsup, "fx", None, false)
            .await
            .unwrap_err()
            .code,
        "UnsupportedStructureModule"
    );
}

#[tokio::test]
async fn auto_defense_on_grid_ammo_cap_destruction_and_neut_ecm_burst_branches() {
    let mut ctx = test_ctx_with_clock(1_800_000_000);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();

    let neut_burst_tid = 952_001_u32;
    let ecm_burst_tid = 952_002_u32;
    let launcher_tid = 952_003_u32;
    let missile_tid = 952_004_u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let base_mod = sde.types.values().find(|t| t.category_id == 66).cloned().unwrap();
        let mut nb = base_mod.clone();
        nb.type_id = neut_burst_tid;
        nb.group_id = 1331;
        nb.category_id = 66;
        nb.default_attributes = HashMap::from([
            (6_u16, 10.0),
            (51, 5000.0),
            (54, 100_000.0),
            (99, 10_000.0),
            (97, 500.0), // neut_gj
        ]);
        sde.types.insert(neut_burst_tid, nb);

        let mut eb = base_mod.clone();
        eb.type_id = ecm_burst_tid;
        eb.group_id = 1331;
        eb.category_id = 66;
        eb.default_attributes = HashMap::from([
            (6_u16, 10.0),
            (51, 5000.0),
            (54, 100_000.0),
            (99, 10_000.0),
            (238, 100.0), // jam strength
            (73, 5000.0),
        ]);
        sde.types.insert(ecm_burst_tid, eb);

        let mut ln = base_mod;
        ln.type_id = launcher_tid;
        ln.group_id = 1327;
        ln.category_id = 66;
        ln.default_attributes = HashMap::from([
            (6_u16, 10.0),
            (51, 2000.0),
            (54, 150_000.0),
            (64, 1.0),
            (604, 384_u32 as f64),
        ]);
        sde.types.insert(launcher_tid, ln);

        let mut mis = sde.types.values().find(|t| t.category_id == 8).cloned().unwrap();
        mis.type_id = missile_tid;
        mis.group_id = 384;
        mis.category_id = 8;
        mis.default_attributes = HashMap::from([(114_u16, 5_000.0), (654, 100.0)]);
        sde.types.insert(missile_tid, mis);
    }

    let gunner_id = 952_101_i64;
    let hostile_id = 952_102_i64;
    let corp_id = 98_952_001_i64;
    let st_id = 1_000_000_952_001_i64;
    let ship_id = 952_201_i64;
    let ship_fragile_id = 952_202_i64;
    let mod_neut = 952_301_i64;
    let mod_ecm = 952_302_i64;
    let mod_launch = 952_303_i64;
    let charge_launch = 952_304_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(gunner_id, clone_char_with(snap, gunner_id, "Gunner2", corp_id, null_sys, st_id));
        snap.characters.insert(hostile_id, clone_char_with(snap, hostile_id, "Hostile2", 98_952_002, null_sys, ship_id));
        snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = gunner_id; __corp.ensure_wallet_divisions(); __corp });
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35_834, owner_corp_id: corp_id, solar_system_id: null_sys,
            name: "Weapons Keepstar".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 500, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            ..Default::default()
        });
        for (mid, tid, flg) in [
            (mod_neut, neut_burst_tid, InventoryFlag::HiSlot0),
            (mod_ecm, ecm_burst_tid, InventoryFlag::HiSlot1),
            (mod_launch, launcher_tid, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: tid, owner_id: corp_id, location_id: st_id, flag: flg, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
        snap.items.insert(charge_launch, InventoryItemRecord {
            item_id: charge_launch, type_id: missile_tid, owner_id: corp_id, location_id: st_id,
            flag: InventoryFlag::HiSlot2, quantity: 3, ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner_id);
        ext.structure_cap_state.insert(st_id, (500_000.0, 1_800_000_000));
        save_ext_state(snap, &ext);
        let mut def = crate::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        def.auto_defense_enabled = true;
        def.auto_target_attackers = true;
        crate::services::structure_acl_defense_rules::save_defense_settings(snap, &def);
    });

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    for (bid, x, hp) in [(ship_id, 5_000.0, 50_000.0), (ship_fragile_id, 6_000.0, 5.0)] {
        let _ = sys_handle.spawn_ball_with_combat(
            Ball {
                id: bid, type_id: 587, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
                max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 2_000.0, max_capacitor: 2_000.0,
                shield_hp: hp, max_shield_hp: hp, armor_hp: hp, max_armor_hp: hp, structure_hp: hp, max_structure_hp: hp,
                ..CombatState::new()
            },
        ).await;
    }

    // 1. Trigger auto-defense on grid: fires mod_launch with charge_launch -> hits ship_id (decrements charge)
    trigger_structure_auto_defense_on_grid(&ctx, &sys_handle, null_sys, st_id, Some(hostile_id), 1_800_000_005).await;
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp < 50_000.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_launch].quantity), 2);

    // 2. Neut & ECM burst projector activation against locked ship_id
    let mut sess = SessionState::new(1);
    sess.user_id = Some(gunner_id as u64);
    sess.character_id = Some(gunner_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(st_id);
    let _ = sys_handle.add_target_lock(st_id, TargetLock {
        target_id: ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(200_000.0),
    }).await;
    let PyRep::KeyVal(neut_res) = handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_neut, "fx", Some(ship_id), false).await.unwrap() else {
        panic!("expected keyval");
    };
    assert!(neut_res.get("capacitorDrained").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    let PyRep::KeyVal(ecm_res) = handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_ecm, "fx", Some(ship_id), false).await.unwrap() else {
        panic!("expected keyval");
    };
    assert_eq!(ecm_res.get("targetsHit"), Some(&PyRep::Integer(2)));

    // 3. Standup launcher destroys fragile ship_fragile_id (clear cycle timer from step 1)
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.standup_module_cycle_until_unix.remove(&mod_launch);
        save_ext_state(snap, &ext);
    });
    let _ = sys_handle.add_target_lock(st_id, TargetLock {
        target_id: ship_fragile_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(200_000.0),
    }).await;
    let _ = handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_fragile_id), false).await.unwrap();
    assert!(sys_handle.get_combat_state(ship_fragile_id).await.unwrap().is_none());
}

#[tokio::test]
async fn add_target_success_charge_id_online_map_and_destroyed_guards() {
    let mut ctx = test_ctx_with_clock(1_800_000_500);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();

    let launcher_tid = 953_003_u32;
    let missile_tid = 953_004_u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let base_mod = sde.types.values().find(|t| t.category_id == 66).cloned().unwrap();
        let mut ln = base_mod;
        ln.type_id = launcher_tid;
        ln.group_id = 1327;
        ln.category_id = 66;
        ln.default_attributes = HashMap::from([
            (6_u16, 10.0),
            (51, 2_000.0),
            (54, 150_000.0),
            (64, 1.0),
            (604, 384_u32 as f64),
        ]);
        sde.types.insert(launcher_tid, ln);

        let mut mis = sde.types.values().find(|t| t.category_id == 8).cloned().unwrap();
        mis.type_id = missile_tid;
        mis.group_id = 384;
        mis.category_id = 8;
        mis.default_attributes = HashMap::from([(114_u16, 4_000.0), (654, 100.0)]);
        sde.types.insert(missile_tid, mis);
    }

    let gunner_id = 953_101_i64;
    let hostile_id = 953_102_i64;
    let corp_id = 98_953_001_i64;
    let st_id = 1_000_000_953_001_i64;
    let ship_id = 953_201_i64;
    let mod_launch = 953_301_i64;
    let charge_launch = 953_302_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(gunner_id, clone_char_with(snap, gunner_id, "Gunner3", corp_id, null_sys, st_id));
        snap.characters.insert(hostile_id, clone_char_with(snap, hostile_id, "Hostile3", 98_953_002, null_sys, ship_id));
        snap.corporations.insert(corp_id, {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.ceo_id = gunner_id;
            c.ensure_wallet_divisions();
            c
        });
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id,
            type_id: 35_834,
            owner_corp_id: corp_id,
            solar_system_id: null_sys,
            name: "Exact Guard Fortizar".into(),
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            ..Default::default()
        });
        snap.items.insert(mod_launch, InventoryItemRecord {
            item_id: mod_launch,
            type_id: launcher_tid,
            owner_id: corp_id,
            location_id: st_id,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(charge_launch, InventoryItemRecord {
            item_id: charge_launch,
            type_id: missile_tid,
            owner_id: corp_id,
            location_id: st_id,
            flag: InventoryFlag::HiSlot0,
            quantity: 5,
            ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner_id);
        ext.structure_cap_state.insert(st_id, (500_000.0, 1_800_000_500));
        save_ext_state(snap, &ext);
        let mut def = crate::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        def.auto_defense_enabled = true;
        def.auto_target_attackers = true;
        crate::services::structure_acl_defense_rules::save_defense_settings(snap, &def);
    });

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle.spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(15_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 50_000.0,
            max_shield_hp: 50_000.0,
            armor_hp: 50_000.0,
            max_armor_hp: 50_000.0,
            structure_hp: 50_000.0,
            max_structure_hp: 50_000.0,
            ..CombatState::new()
        },
    ).await;

    let mut sess = SessionState::new(9);
    sess.user_id = Some(gunner_id as u64);
    sess.character_id = Some(gunner_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(st_id);

    // 1. `handle_structure_add_target` returns exact Tuple(1, []) and locks ship
    let lock_rep = handle_structure_add_target(&ctx, &sess, gunner_id, st_id, ship_id)
        .await
        .unwrap();
    assert_eq!(lock_rep, PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())]));
    for _ in 0..60 {
        if sys_handle
            .get_target_locks(st_id)
            .await
            .unwrap_or_default()
            .iter()
            .any(|l| l.target_id == ship_id && l.locked)
        {
            break;
        }
        sys_handle.step_manual(1.0).await.unwrap();
    }

    // 2. Omitting `module_online_states[mod_launch]` passes L695 (`unwrap_or(true)`) and hits L804 (`!= Some(&true)` -> ModuleOffline; kills `|| -> &&`)
    let offline_err = handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
        .await
        .unwrap_err();
    assert_eq!(offline_err.code, "ModuleOffline");

    // 3. Explicit `module_online_states[mod_launch] = true` succeeds cleanly (`L798 == -> !=` and `L804 != -> ==`) and returns exact `chargeTypeID == missile_tid`
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_launch, true);
    });
    let act_rep = handle_structure_module_activate(&ctx, &mut sess, gunner_id, st_id, mod_launch, "fx", Some(ship_id), false)
        .await
        .unwrap();
    let PyRep::KeyVal(kvs) = act_rep else { panic!("expected KeyVal") };
    assert_eq!(kvs.get("moduleID"), Some(&PyRep::Integer(mod_launch)));
    assert_eq!(kvs.get("targetID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(kvs.get("chargeTypeID"), Some(&PyRep::Integer(i64::from(missile_tid))));
    assert!(kvs.get("damageDealt").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_launch].quantity), 4);

    // 4. Direct `consume_structure_charge_in_snap` returns exact `missile_tid` (not 0 or 1)
    let consumed_tid = ctx.db.try_with_mut_snapshot(|snap| {
        super::super::consume_structure_charge_in_snap(
            &ctx,
            snap,
            st_id,
            corp_id,
            mod_launch,
            InventoryFlag::HiSlot0,
            384,
        )
    }).unwrap();
    assert_eq!(consumed_tid, missile_tid);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_launch].quantity), 3);

    // 5. Clear cooldown & fire `trigger_structure_auto_defense_on_grid` -> records attacker, locks target, consumes charge
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.standup_module_cycle_until_unix.remove(&mod_launch);
        save_ext_state(snap, &ext);
    });
    let shield_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    trigger_structure_auto_defense_on_grid(&ctx, &sys_handle, null_sys, st_id, Some(hostile_id), 1_800_000_510).await;
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp < shield_before);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_launch].quantity), 2);
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert_eq!(ext.structure_auto_targets.get(&st_id), Some(&vec![ship_id]));
        let def = crate::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        assert!(def.known_attacker_timestamps.contains_key(&hostile_id));
    });
}
