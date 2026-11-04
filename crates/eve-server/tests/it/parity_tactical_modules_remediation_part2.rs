//! Dedicated integration tests for remediated tactical modules parity rows (Part 2):
//! - `feat:modules.command-bursts`
//! - `feat:modules.remote-armor-repairer`

#![forbid(unsafe_code)]

use crate::sovereignty_equinox_gameplay_closure::{
    open_sqlite_ctx, register_online_pilot, temp_sqlite_path, ALLIANCE, CORP,
};
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::TargetLock;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}

fn extract_list_ints(rep: &PyRep, key: &str) -> Vec<i64> {
    match rep {
        PyRep::KeyVal(entries) => {
            if let Some(PyRep::List(items)) = entries.get(key) {
                return items
                    .iter()
                    .filter_map(|it| match it {
                        PyRep::Integer(i) => Some(*i),
                        _ => None,
                    })
                    .collect();
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                if let PyRep::String(s) = k {
                    if s == key {
                        if let PyRep::List(items) = v {
                            return items
                                .iter()
                                .filter_map(|it| match it {
                                    PyRep::Integer(i) => Some(*i),
                                    _ => None,
                                })
                                .collect();
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Vec::new()
}

fn extract_bool_field(rep: &PyRep, key: &str) -> bool {
    match rep {
        PyRep::KeyVal(entries) => {
            if let Some(PyRep::Bool(b)) = entries.get(key) {
                return *b;
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                if let PyRep::String(s) = k {
                    if s == key {
                        if let PyRep::Bool(b) = v {
                            return *b;
                        }
                    }
                }
            }
        }
        _ => {}
    }
    false
}

#[tokio::test]
async fn test_command_bursts_parity_remediation() {
    // parity: feat:modules.command-bursts
    eve_testkit::attest(&["feat:modules.command-bursts"]);

    let db_path = temp_sqlite_path("parity_command_bursts_remediation");
    let ctx = open_sqlite_ctx(&db_path, true);
    let low_sys = 30_002_718u32;

    let booster_char = 90_000_301i64;
    let booster_ship = 100_000_301i64;
    let fleet_near_char = 90_000_302i64;
    let fleet_near_ship = 100_000_302i64;
    let fleet_far_char = 90_000_303i64;
    let fleet_far_ship = 100_000_303i64;
    let neutral_char = 90_000_304i64;
    let neutral_ship = 100_000_304i64;

    let mut _s_booster = register_online_pilot(
        &ctx, 88_301, "booster", "sec", booster_char, CORP, Some(ALLIANCE),
        low_sys, booster_ship, 22_444, // Eos (Command Ship, group 540)
        DVec3::ZERO, 0,
    ).await;

    let _s_near = register_online_pilot(
        &ctx, 88_302, "near", "sec", fleet_near_char, CORP, Some(ALLIANCE),
        low_sys, fleet_near_ship, 624, // Maller (5 km away, within AoE range)
        DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;

    let mut _s_far = register_online_pilot(
        &ctx, 88_303, "far", "sec", fleet_far_char, CORP, Some(ALLIANCE),
        low_sys, fleet_far_ship, 624, // Maller (150 km away, out of range)
        DVec3::new(150_000.0, 0.0, 0.0), 0,
    ).await;

    let mut _s_neutral = register_online_pilot(
        &ctx, 88_304, "neutral", "sec", neutral_char, CORP, Some(ALLIANCE),
        low_sys, neutral_ship, 624, // Maller (5 km away, but not in fleet)
        DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;

    let burst_mod_id = 988_901i64;
    let charge_item_id = 988_902i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_301,
            FleetRecord {
                fleet_id: 77_301,
                boss_id: booster_char,
                members: vec![booster_char, fleet_near_char, fleet_far_char],
                ..Default::default()
            },
        );
        snap.items.insert(
            burst_mod_id,
            InventoryItemRecord {
                item_id: burst_mod_id,
                type_id: 42_529, // Shield Command Burst I (group 1770)
                owner_id: booster_char,
                location_id: booster_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_item_id,
            InventoryItemRecord {
                item_id: charge_item_id,
                type_id: 42_695, // Shield Harmonizing Charge (group 1769)
                owner_id: booster_char,
                location_id: booster_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        let b_skills = snap.skills.entry(booster_char).or_default();
        b_skills.insert(3350, TrainedSkillRecord { type_id: 3350, level: 5, skill_points: 256_000.0 });
        b_skills.insert(3351, TrainedSkillRecord { type_id: 3351, level: 5, skill_points: 256_000.0 });
        b_skills.insert(11574, TrainedSkillRecord { type_id: 11574, level: 5, skill_points: 256_000.0 });
    });
    ctx.invalidate_ship_fitting(booster_ship);

    let sys_h = ctx.systems.get_or_spawn(low_sys);
    let fit = ctx.get_or_rebuild_ship_fitting(booster_char, booster_ship).unwrap();
    let mod_state = eve_dogma::DogmaItemState::from_sde(burst_mod_id, 42_529, &ctx.sde);
    let mod_type = ctx.sde.types.get(&42_529).unwrap();

    let res = eve_server::services::tactical_modules::activate_command_burst(
        &ctx, &sys_h, booster_char, booster_ship, low_sys,
        burst_mod_id, mod_type, &mod_state, Some(42_695), &fit,
    )
    .await
    .expect("activate Shield Command Burst");

    assert!(extract_bool_field(&res, "active"), "Burst must be active");
    let affected_ships = extract_list_ints(&res, "affectedShipIDs");
    assert!(affected_ships.contains(&booster_ship), "Booster must receive its own AoE burst");
    assert!(affected_ships.contains(&fleet_near_ship), "Fleet member within AoE range must receive burst");
    assert!(!affected_ships.contains(&fleet_far_ship), "Fleet member beyond AoE range must NOT receive burst");
    assert!(!affected_ships.contains(&neutral_ship), "Non-fleet pilot must NOT receive fleet burst");

    // Charge consumption: 1 charge consumed (5 -> 4).
    let rem_charges = ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).unwrap().quantity);
    assert_eq!(rem_charges, 4, "Activating command burst consumes 1 loaded charge");

    // Notifications: near fleetmate receives OnDbuffUpdated.
    let near_notes = ctx.drain_notifications_for_char(fleet_near_char);
    assert!(
        near_notes.iter().any(|n| n.event == "OnDbuffUpdated"),
        "Affected fleet member must receive OnDbuffUpdated notification: {near_notes:?}"
    );
}

#[tokio::test]
async fn test_remote_armor_repairer_parity_remediation() {
    // parity: feat:modules.remote-armor-repairer
    eve_testkit::attest(&["feat:modules.remote-armor-repairer"]);

    let db_path = temp_sqlite_path("parity_rar_remediation");
    let ctx = open_sqlite_ctx(&db_path, true);
    let low_sys = 30_002_718u32;

    let logi_char = 90_000_401i64;
    let logi_ship = 100_000_401i64;
    let ally_char = 90_000_402i64;
    let ally_ship = 100_000_402i64;
    let rar_id = 988_951i64;

    let mut sess_logi = register_online_pilot(
        &ctx, 88_401, "logi", "sec", logi_char, CORP, Some(ALLIANCE),
        low_sys, logi_ship, 11_989, // Oneiros (Logistics Cruiser, group 900)
        DVec3::ZERO, 0,
    ).await;

    let mut _sess_ally = register_online_pilot(
        &ctx, 88_402, "ally", "sec", ally_char, CORP, Some(ALLIANCE),
        low_sys, ally_ship, 624, // Maller (friendly ally ship, 5 km away)
        DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rar_id,
            InventoryItemRecord {
                item_id: rar_id,
                type_id: 11_357, // Medium Remote Armor Repairer I (group 325)
                owner_id: logi_char,
                location_id: logi_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let l_skills = snap.skills.entry(logi_char).or_default();
        l_skills.insert(16069, TrainedSkillRecord { type_id: 16069, level: 5, skill_points: 256_000.0 }); // Remote Armor Repair Systems V
        snap.fleets.insert(
            77_401,
            FleetRecord {
                fleet_id: 77_401,
                boss_id: logi_char,
                members: vec![logi_char, ally_char],
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(logi_ship);

    let sys_h = ctx.systems.get_or_spawn(low_sys);

    // Set damaged armor on ally ship.
    let mut ally_cs = ctx.build_ship_combat_state(ally_char, ally_ship, 624);
    ally_cs.max_armor_hp = 1_500.0;
    ally_cs.armor_hp = 300.0;
    sys_h.set_combat_state(ally_ship, ally_cs).await.unwrap();

    let kw = HashMap::new();

    // 1. Activating without target fails with TargetRequired.
    let no_target_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::None, PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(no_target_err), "TargetRequired");

    // 2. Activating on self fails with CannotActivateOnSelf.
    let self_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(logi_ship), PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(self_err), "CannotActivateOnSelf");

    // 3. Activating on friendly target without lock fails with TargetNotLocked.
    let unlock_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(unlock_err), "TargetNotLocked");

    // Lock friendly target.
    sys_h.add_target_lock(
        logi_ship,
        TargetLock {
            target_id: ally_ship,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(100_000.0),
        },
    ).await.unwrap();

    // 4. Insufficient capacitor fails with NotEnoughCapacitor.
    sys_h.set_capacitor(logi_ship, 5.0).await.unwrap();
    let cap_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(cap_err), "NotEnoughCapacitor");

    // 5. Sufficient capacitor: activation consumes capacitor.
    sys_h.set_capacitor(logi_ship, 500.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)],
        &kw, None,
    ).await.expect("Activate Remote Armor Repairer");

    let cap_after_act = sys_h.get_combat_state(logi_ship).await.unwrap().unwrap().capacitor;
    assert!(cap_after_act < 500.0, "Capacitor must be consumed on cycle start: {cap_after_act} < 500.0");

    // 6. Armor is repaired at cycle completion (Medium RAR duration is 6s).
    for _ in 0..6 {
        sys_h.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }

    let ally_armor_repaired = sys_h.get_combat_state(ally_ship).await.unwrap().unwrap().armor_hp;
    assert!(
        ally_armor_repaired > 300.0,
        "Friendly target armor must be repaired after cycle completion (got {ally_armor_repaired} vs initial 300.0)"
    );
    let hp_repaired = ally_armor_repaired - 300.0;
    assert!(
        hp_repaired >= 190.0,
        "Medium RAR I must repair at least 190 HP (got {hp_repaired})"
    );
}
