//! Modules (Doomsday, Command Bursts, Analyzers, Scanners, Entosis, Clone Vat Bay, Mutaplasmids)
//! and Dogma (`dbuff` Aggregation, System-Wide Wormhole/Storm Effects, Attribute Change Notifications)
//! integration test suite (11 rows).


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
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

fn extract_attr_from_dict(dict_rep: &PyRep, attr_id: i64) -> f64 {
    if let PyRep::Dict(entries) = dict_rep {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) {
                return v.as_f64().unwrap_or(0.0);
            }
        }
    }
    0.0
}

#[tokio::test]
async fn test_titan_doomsday_single_target_and_lance_slash() {
    // parity: feat:modules.doomsday, rpc:superWeaponMgr.ActivateSinglePointTargetedModule, rpc:superWeaponMgr.ActivateSlashModule, notify:OnSpecialFX, notify:OnDamageMessages, notify:OnDamageMessage
    eve_testkit::attest(&["feat:modules.doomsday", "rpc:superWeaponMgr.ActivateSinglePointTargetedModule", "rpc:superWeaponMgr.ActivateSlashModule", "notify:OnSpecialFX", "notify:OnDamageMessages", "notify:OnDamageMessage"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7201);
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
    let null_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);

    // Seed Avatar Titan (11567), 'Judgment' Electromagnetic Doomsday (24550), 'Holy Destiny' Lance (40631), and Helium Isotopes (16274)
    let titan_id = 840_001i64;
    let dd_mod_id = 840_002i64;
    let lance_mod_id = 840_003i64;
    let fuel_id = 840_004i64;
    let dread_target_id = 840_005i64;
    for tid in [11567u32, 24550, 40631] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    seed_item(&ctx, titan_id, 11567, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, dd_mod_id, 24550, char_id, titan_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, lance_mod_id, 40631, char_id, titan_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, dread_target_id, 19720, 90_000_002, null_sys_id as i64, InventoryFlag::None, 1, true);

    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(titan_id)], &kw, None)
        .await
        .expect("Board Avatar Titan");

    // 1. Highsec activation must be rejected (`CannotActivateDoomsdayInHighSec`)
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });
    let hs_err = ctx
        .dispatch_rpc(
            &mut sess,
            "superWeaponMgr",
            None,
            "ActivateSinglePointTargetedModule",
            &[PyRep::Integer(dd_mod_id), PyRep::Integer(dread_target_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Doomsday in highsec must fail");
    assert_eq!(hs_err.code, "CannotActivateDoomsdayInHighSec");

    // 2. Move Titan and target Dreadnought to nullsec and fire single-target Doomsday + AoE Slash Lance
    sess.solar_system_id = Some(null_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = null_sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&titan_id) {
            sh.location_id = null_sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    let sys = ctx.systems.get_or_spawn(null_sys_id);
    let _ = sys
        .spawn_ball(
            Ball {
                id: titan_id,
                type_id: 11567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 60.0,
                inertia: 1.0,
                mass: 2_400_000_000.0,
                radius: 9_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: dread_target_id,
                type_id: 19720,
                position: DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 80.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 2_500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500_000.0,
                max_shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                max_armor_hp: 500_000.0,
                structure_hp: 500_000.0,
                max_structure_hp: 500_000.0,
                ..CombatState::default()
            },
        )
        .await;

    // Firing without Helium Isotopes (16274) must fail with InsufficientFuel
    let no_fuel_err = ctx
        .dispatch_rpc(
            &mut sess,
            "superWeaponMgr",
            None,
            "ActivateSinglePointTargetedModule",
            &[PyRep::Integer(dd_mod_id), PyRep::Integer(dread_target_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Doomsday without fuel must fail");
    assert_eq!(no_fuel_err.code, "InsufficientFuel");

    seed_item(&ctx, fuel_id, 16274, char_id, titan_id, InventoryFlag::Cargo, 200_000, false);

    let dd_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "superWeaponMgr",
            None,
            "ActivateSinglePointTargetedModule",
            &[PyRep::Integer(dd_mod_id), PyRep::Integer(dread_target_id)],
            &kw,
            None,
        )
        .await
        .expect("ActivateSinglePointTargetedModule Doomsday");
    assert!(dd_rep.dict_get("totalDamageApplied").and_then(|v| v.as_f64()).unwrap_or(0.0) > 100_000.0);
    assert_eq!(dd_rep.dict_get("immobilizeSeconds").and_then(|v| v.as_f64()), Some(30.0));

    // The single-point doomsday may destroy the first dreadnought; give the lance its own target in range.
    let lance_target_id = dread_target_id + 1;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: lance_target_id,
                type_id: 19720,
                position: DVec3::new(20_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 80.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 2_500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500_000.0,
                max_shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                max_armor_hp: 500_000.0,
                structure_hp: 500_000.0,
                max_structure_hp: 500_000.0,
                ..CombatState::default()
            },
        )
        .await;

    let slash_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "superWeaponMgr",
            None,
            "ActivateSlashModule",
            &[
                PyRep::Integer(lance_mod_id),
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                PyRep::Tuple(vec![PyRep::Real(30_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("ActivateSlashModule Lance");
    assert!(
        matches!(slash_rep.dict_get("hitTargetIDs"), Some(PyRep::List(l)) if l.iter().any(|v| v.as_i64() == Some(lance_target_id))),
        "lance must hit the dreadnought on its path: {:?}",
        slash_rep.dict_get("hitTargetIDs")
    );
}

#[tokio::test]
async fn test_command_bursts_dbuff_aggregation_and_system_wide_effects() {
    // parity: feat:modules.command-bursts, feat:dogma.dbuffs, feat:dogma.system-wide-effects, rpc:dogmaIM.LoadAmmo, notify:OnModuleAttributeChanges, notify:OnGodmaShipEffect, notify:OnGodmaFlushLocation, notify:ProcessGodmaPrimeLocation, notify:OnSpecialFX
    eve_testkit::attest(&["feat:modules.command-bursts", "feat:dogma.dbuffs", "feat:dogma.system-wide-effects", "rpc:dogmaIM.LoadAmmo", "notify:OnModuleAttributeChanges", "notify:OnGodmaShipEffect", "notify:OnGodmaFlushLocation", "notify:ProcessGodmaPrimeLocation", "notify:OnSpecialFX"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7202);
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
    let sys_id = 30_000_142u32;

    // 1. Fit Shield Command Burst I (42529) + Shield Extension Charge (42696, dbuff 12: +8% shieldCapacity 263)
    let burst_mod_id = 850_001i64;
    let burst_charge_id = 850_002i64;
    for tid in [42529u32, 42696] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    seed_item(&ctx, burst_mod_id, 42529, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    seed_item(&ctx, burst_charge_id, 42696, char_id, ship_id, InventoryFlag::HiSlot2, 100, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(burst_mod_id, true);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
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

    // Record baseline shield capacity before Command Burst
    let info_before = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[], &kw, None)
        .await
        .expect("GetAllInfo before burst");
    let shield_before = extract_attr_from_dict(
        info_before.dict_get("shipAttributes").unwrap(),
        attr_ids::SHIELD_CAPACITY as i64,
    );

    // Activate Shield Command Burst I
    let burst_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(burst_mod_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Shield Command Burst I");
    assert_eq!(burst_rep.dict_get("chargeTypeID").and_then(|v| v.as_i64()), Some(42696));

    // Verify dbuff 12 is active via GetDbuffStateForClient and GetMappedDbuffsForItem
    let dbuff_state = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetDbuffStateForClient",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetDbuffStateForClient");
    assert!(matches!(dbuff_state.dict_get("dbuffs"), Some(PyRep::Dict(d)) if !d.is_empty()));
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetMappedDbuffsForItem",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetMappedDbuffsForItem");

    // Apply a weaker overlapping dbuff 12 (+4%) and a stronger overlapping dbuff 12 (+15%) to verify Maximum aggregation
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "ApplyBuffsToItems",
        &[
            PyRep::List(vec![PyRep::Integer(ship_id)]),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(12), PyRep::Real(4.0)]),
                PyRep::Tuple(vec![PyRep::Integer(12), PyRep::Real(15.0)]),
            ]),
            PyRep::Real(120.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("ApplyBuffsToItems");

    let info_after_dbuff = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[], &kw, None)
        .await
        .expect("GetAllInfo after dbuff");
    let shield_after_dbuff = extract_attr_from_dict(
        info_after_dbuff.dict_get("shipAttributes").unwrap(),
        attr_ids::SHIELD_CAPACITY as i64,
    );
    assert!(
        (shield_after_dbuff - shield_before * 1.15).abs() < 1.0,
        "Expected Maximum dbuff 12 (+15%) to scale shieldCapacity from {shield_before} to {}, got {shield_after_dbuff}",
        shield_before * 1.15
    );

    // 2. Apply Wormhole Class 1 Pulsar system-wide effect (30844: +30% Shield HP)
    let sys_eff = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RefreshSystemWideEnvironment",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(30844)],
            &kw,
            None,
        )
        .await
        .expect("RefreshSystemWideEnvironment Pulsar 30844");
    assert_eq!(sys_eff.dict_get("effectTypeID").and_then(|v| v.as_i64()), Some(30844));
    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsOnShip", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("GetSystemWideEffectsOnShip");
    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsInfo", &[PyRep::Integer(sys_id as i64)], &kw, None)
        .await
        .expect("GetSystemWideEffectsInfo");

    let info_pulsar = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[], &kw, None)
        .await
        .expect("GetAllInfo in Pulsar");
    let shield_pulsar = extract_attr_from_dict(
        info_pulsar.dict_get("shipAttributes").unwrap(),
        attr_ids::SHIELD_CAPACITY as i64,
    );
    assert!(
        shield_pulsar > shield_after_dbuff * 1.25,
        "Pulsar system-wide effect must further increase shieldCapacity (got {shield_pulsar} vs {shield_after_dbuff})"
    );

    // Clear system-wide effect and verify shieldCapacity returns to pre-Pulsar level
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "ResetSystemWideEffects",
        &[PyRep::Integer(sys_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("ResetSystemWideEffects");
    let info_reset = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[], &kw, None)
        .await
        .expect("GetAllInfo after ResetSystemWideEffects");
    let shield_reset = extract_attr_from_dict(
        info_reset.dict_get("shipAttributes").unwrap(),
        attr_ids::SHIELD_CAPACITY as i64,
    );
    assert!((shield_reset - shield_after_dbuff).abs() < 1.0);

    // Advance virtual clock past dbuff duration and verify dbuffs expire back to baseline
    ctx.advance_virtual_clock(200);
    let dbuff_expired = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetDbuffStateForClient",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetDbuffStateForClient after expiry");
    assert_eq!(dbuff_expired.dict_get("activeCount").and_then(|v| v.as_i64()), Some(0));
}

#[tokio::test]
async fn test_analyzers_cargo_ship_scanners_entosis_and_clone_vat_bay() {
    // parity: feat:modules.data-relic-analyzer, feat:modules.entosis-link, feat:modules.clone-vat-bay, rpc:jumpCloneSvc.OfferShipCloneInstallation, rpc:jumpCloneSvc.AcceptShipCloneInstallation, rpc:jumpCloneSvc.GetShipCloneState, notify:OnShipJumpCloneCacheInvalidated, notify:OnJumpCloneCacheInvalidated, notify:OnSpecialFX, notify:OnGodmaShipEffect
    eve_testkit::attest(&["feat:modules.data-relic-analyzer", "feat:modules.entosis-link", "feat:modules.clone-vat-bay", "rpc:jumpCloneSvc.OfferShipCloneInstallation", "rpc:jumpCloneSvc.AcceptShipCloneInstallation", "rpc:jumpCloneSvc.GetShipCloneState", "notify:OnShipJumpCloneCacheInvalidated", "notify:OnJumpCloneCacheInvalidated", "notify:OnSpecialFX", "notify:OnGodmaShipEffect"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7203);
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
    let sys_id = 30_000_142u32;

    // Fit Data Analyzer I (22175), Cargo Scanner I (442), Ship Scanner I (443), Entosis Link I (34593), and Clone Vat Bay I (23735)
    let data_mod_id = 860_001i64;
    let cargo_scan_id = 860_002i64;
    let ship_scan_id = 860_003i64;
    let entosis_id = 860_004i64;
    let stront_id = 860_005i64;
    let target_ship_id = 860_010i64;
    let target_cargo_id = 860_011i64;
    let target_mod_id = 860_012i64;

    for tid in [22175u32, 442, 443, 34593, 23735] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    seed_item(&ctx, data_mod_id, 22175, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, cargo_scan_id, 442, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    seed_item(&ctx, ship_scan_id, 443, char_id, ship_id, InventoryFlag::MedSlot2, 1, true);
    seed_item(&ctx, entosis_id, 34593, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    seed_item(&ctx, stront_id, 16275, char_id, ship_id, InventoryFlag::Cargo, 500, false);

    seed_item(&ctx, target_ship_id, 587, 90_000_002, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, target_cargo_id, 34, 90_000_002, target_ship_id, InventoryFlag::Cargo, 777, false);
    seed_item(&ctx, target_mod_id, 2048, 90_000_002, target_ship_id, InventoryFlag::LoSlot0, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        for mid in [data_mod_id, cargo_scan_id, ship_scan_id, entosis_id] {
            snap.module_online_states.insert(mid, true);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
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
                id: target_ship_id,
                type_id: 587,
                position: DVec3::new(2_000.0, 0.0, 0.0),
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

    // 1. Data Analyzer I (`22175`)
    let hack_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(data_mod_id),
                PyRep::String("dataHacking".into()),
                PyRep::Integer(target_ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Data Analyzer I");
    assert_eq!(hack_rep.dict_get("containerUnlocked").and_then(|v| v.as_bool()), Some(true));
    assert!(hack_rep.dict_get("virusCoherence").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 40.0);

    // 2. Cargo Scanner I (`442`)
    let cscan_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(cargo_scan_id),
                PyRep::String("cargoScan".into()),
                PyRep::Integer(target_ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Cargo Scanner I");
    assert_eq!(cscan_rep.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(777));

    // 3. Ship Scanner I (`443`)
    let sscan_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(ship_scan_id),
                PyRep::String("shipScan".into()),
                PyRep::Integer(target_ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Ship Scanner I");
    assert!(matches!(sscan_rep.dict_get("fittedModules"), Some(PyRep::List(l)) if !l.is_empty()));

    // 4. Entosis Link I (`34593`): consumes Strontium Clathrates and self-immobilizes
    let ent_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(entosis_id),
                PyRep::String("entosisLink".into()),
                PyRep::Integer(target_ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Entosis Link I");
    assert_eq!(ent_rep.dict_get("cannotWarp").and_then(|v| v.as_bool()), Some(true));

    // 5. Clone Vat Bay (`23735` on Rorqual `28352`) + JumpClone installation
    let rorq_id = 860_020i64;
    let cvb_id = 860_021i64;
    let ozone_id = 860_022i64;
    seed_item(&ctx, rorq_id, 28352, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, cvb_id, 23735, char_id, rorq_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, ozone_id, 16273, char_id, rorq_id, InventoryFlag::Cargo, 500, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cvb_id, true);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = rorq_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
    });
    sess.ship_id = Some(rorq_id);
    ctx.invalidate_ship_fitting(rorq_id);

    let cvb_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_id)], &kw, None)
        .await
        .expect("Activate Clone Vat Bay I");
    assert_eq!(cvb_rep.dict_get("cloneVatBayActive").and_then(|v| v.as_bool()), Some(true));

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("OfferShipCloneInstallation");
    let acc_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "AcceptShipCloneInstallation",
            &[PyRep::Integer(rorq_id)],
            &kw,
            None,
        )
        .await
        .expect("AcceptShipCloneInstallation");
    assert_eq!(acc_rep.dict_get("installed").and_then(|v| v.as_bool()), Some(true));
    let state_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetShipCloneState",
            &[PyRep::Integer(rorq_id)],
            &kw,
            None,
        )
        .await
        .expect("GetShipCloneState");
    let PyRep::Rowset {
        columns: ship_clone_cols,
        rows: ship_clone_rows,
    } = state_rep
    else {
        panic!("Expected Rowset from GetShipCloneState, got {state_rep:?}");
    };
    assert_eq!(
        ship_clone_cols,
        vec!["jumpCloneID", "ownerID", "locationID", "cloneName"]
    );
    assert_eq!(ship_clone_rows.len(), 1);
    assert_eq!(ship_clone_rows[0][1].as_i64(), Some(90_000_001));
    assert_eq!(ship_clone_rows[0][2].as_i64(), Some(rorq_id));
}

#[tokio::test]
async fn test_abyssal_mutaplasmid_mutation_and_inspection() {
    // parity: feat:modules.mutaplasmids, feat:modules.abyssal-modules-trade, rpc:dynamicItemService.CreateDynamicItem, rpc:dynamicItemService.GetDynamicItemInfo, notify:OnItemChange
    eve_testkit::attest(&["feat:modules.mutaplasmids", "feat:modules.abyssal-modules-trade", "rpc:dynamicItemService.CreateDynamicItem", "rpc:dynamicItemService.GetDynamicItemInfo", "notify:OnItemChange"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7204);
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

    // Seed 1MN Y-S8 Compact Afterburner (5975) and Decayed 1MN Afterburner Mutaplasmid (47297 -> 47408)
    let ab_item_id = 870_001i64;
    let muta_item_id = 870_002i64;
    seed_item(&ctx, ab_item_id, 5975, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, muta_item_id, 47297, char_id, station_id, InventoryFlag::Hangar, 1, false);

    // 1. Reject incompatible mutaplasmid (e.g. Warp Scrambler 447 with Afterburner mutaplasmid 47297)
    let scram_item_id = 870_003i64;
    seed_item(&ctx, scram_item_id, 447, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let inapp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47297), PyRep::Integer(scram_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Incompatible mutaplasmid must fail");
    assert_eq!(inapp_err.code, "InapplicableMutaplasmid");

    // 2. Apply mutaplasmid 47297 to 1MN Y-S8 Compact Afterburner (5975) -> 1MN Abyssal Afterburner (47408)
    let mut_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47297), PyRep::Integer(ab_item_id)],
            &kw,
            None,
        )
        .await
        .expect("CreateDynamicItem");
    let mutated_id = mut_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(mut_rep.dict_get("resultingTypeID").and_then(|v| v.as_i64()), Some(47408));

    // 3. Inspect dynamic attributes via dynamicItemService.GetDynamicItemInfo & dogmaIM.ItemGetInfo
    let dyn_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicItemService",
            None,
            "GetDynamicItemInfo",
            &[PyRep::Integer(mutated_id)],
            &kw,
            None,
        )
        .await
        .expect("dynamicItemService.GetDynamicItemInfo");
    assert_eq!(dyn_info.dict_get("mutaplasmidTypeID").and_then(|v| v.as_i64()), Some(47297));
    assert_eq!(dyn_info.dict_get("sourceTypeID").and_then(|v| v.as_i64()), Some(5975));

    let item_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(mutated_id)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.ItemGetInfo on mutated item");
    assert_eq!(item_info.dict_get("typeID").and_then(|v| v.as_i64()), Some(47408));
    assert!(matches!(item_info.dict_get("dynamicAttributes"), Some(PyRep::Dict(d)) if !d.is_empty()));
}

#[tokio::test]
async fn test_cargo_and_ship_scanner_show_target_cargo_fitting_and_capacitor() {
    // parity: feat:modules.cargo-scanner, feat:modules.ship-scanner
    eve_testkit::attest(&["feat:modules.cargo-scanner", "feat:modules.ship-scanner"]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7205);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let (cargo_scan_id, ship_scan_id) = (880_001i64, 880_002i64);
    let (target_ship_id, far_target_id) = (880_010i64, 880_020i64);
    for tid in [442u32, 443] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    seed_item(&ctx, cargo_scan_id, 442, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    seed_item(&ctx, ship_scan_id, 443, char_id, ship_id, InventoryFlag::MedSlot2, 1, true);
    // Target Rifter: cargo (Tritanium x777, Pyerite x55), fitted Damage Control (2048) and a
    // 200mm AutoCannon I (2873) loaded with EMP S (185) charges.
    seed_item(&ctx, target_ship_id, 587, 90_000_002, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, 880_011, 34, 90_000_002, target_ship_id, InventoryFlag::Cargo, 777, false);
    seed_item(&ctx, 880_012, 35, 90_000_002, target_ship_id, InventoryFlag::Cargo, 55, false);
    seed_item(&ctx, 880_013, 2048, 90_000_002, target_ship_id, InventoryFlag::LoSlot0, 1, true);
    seed_item(&ctx, 880_014, 2873, 90_000_002, target_ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, 880_015, 185, 90_000_002, target_ship_id, InventoryFlag::HiSlot0, 100, false);
    seed_item(&ctx, far_target_id, 587, 90_000_002, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        for mid in [cargo_scan_id, ship_scan_id] {
            snap.module_online_states.insert(mid, true);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let rifter = |id: i64, x: f64| Ball {
        id,
        type_id: 587,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_350_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(rifter(ship_id, 0.0)).await;
    let target_cs = CombatState { capacitor: 123.0, max_capacitor: 250.0, ..CombatState::default() };
    let _ = sys.spawn_ball_with_combat(rifter(target_ship_id, 5_000.0), target_cs).await;
    let _ = sys.spawn_ball(rifter(far_target_id, 400_000.0)).await;
    let _ = ctx.drain_notifications_for_char(char_id);

    let scan = |module_id: i64, effect: &str, target: i64| {
        vec![PyRep::Integer(module_id), PyRep::String(effect.into()), PyRep::Integer(target)]
    };
    // Out of SDE cargoScanRange / shipScanRange.
    for (mid, eff) in [(cargo_scan_id, "cargoScan"), (ship_scan_id, "shipScan")] {
        let e = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &scan(mid, eff, far_target_id), &kw, None)
            .await
            .expect_err("target beyond scan range");
        assert_eq!(e.code, "TargetOutOfRange");
    }

    // Cargo scan lists exactly the target's cargo (not its fitted modules or charges).
    let cargo = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &scan(cargo_scan_id, "cargoScan", target_ship_id), &kw, None)
        .await
        .expect("cargoScan");
    let Some(PyRep::List(items)) = cargo.dict_get("scannedItems") else { panic!("scannedItems") };
    let mut seen: Vec<(i64, i64)> = items
        .iter()
        .map(|e| (e.dict_get("typeID").unwrap().as_i64().unwrap(), e.dict_get("quantity").unwrap().as_i64().unwrap()))
        .collect();
    seen.sort();
    assert_eq!(seen, vec![(34, 777), (35, 55)]);
    let notes = ctx.drain_notifications_for_char(char_id);
    let n = notes.iter().find(|n| n.event == "OnCargoScanComplete").expect("OnCargoScanComplete");
    assert_eq!(n.args[0].as_i64(), Some(target_ship_id));
    assert!(matches!(&n.args[1], PyRep::List(l) if l.len() == 2));

    // Ship scan lists fitted modules + loaded charges and the target's capacitor.
    let ship = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &scan(ship_scan_id, "shipScan", target_ship_id), &kw, None)
        .await
        .expect("shipScan");
    let type_ids = |key: &str| -> Vec<i64> {
        let Some(PyRep::List(l)) = ship.dict_get(key) else { panic!("{key}") };
        let mut v: Vec<i64> = l.iter().map(|e| e.dict_get("typeID").unwrap().as_i64().unwrap()).collect();
        v.sort();
        v
    };
    assert_eq!(type_ids("fittedModules"), vec![2048, 2873]);
    assert_eq!(type_ids("charges"), vec![185]);
    assert_eq!(ship.dict_get("capacitor").and_then(|v| v.as_f64()), Some(123.0));
    assert_eq!(ship.dict_get("maxCapacitor").and_then(|v| v.as_f64()), Some(250.0));
    let notes = ctx.drain_notifications_for_char(char_id);
    let n = notes.iter().find(|n| n.event == "OnShipScanCompleted").expect("OnShipScanCompleted");
    assert_eq!(n.args[0].as_i64(), Some(target_ship_id));
    assert_eq!(n.args[1].as_f64(), Some(123.0));
    assert_eq!(n.args[2].as_f64(), Some(250.0));
    assert!(matches!(&n.args[3], PyRep::List(l) if l.len() == 3));
}
