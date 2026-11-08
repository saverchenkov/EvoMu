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
        .find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let high_sys = 30_000_142_u32;

    let find_mod_of_group = |grp: u32| -> u32 {
        ctx.sde
            .types
            .values()
            .find(|t| t.group_id == grp)
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
        .find(|t| t.category_id == 8 && t.group_id == req_charge_grp)
        .map(|t| t.type_id)
        .unwrap();
    let wrong_charge_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 8 && t.group_id != req_charge_grp)
        .map(|t| t.type_id)
        .unwrap();
    let unsupported_mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 66 && !matches!(t.group_id, 1327 | 1328 | 1330 | 1331 | 1333 | 1562))
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
        ext.structure_cap_state.remove(&st_id);
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
            .find(|t| t.category_id == 8 && t.group_id == burst_charge_grp)
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
