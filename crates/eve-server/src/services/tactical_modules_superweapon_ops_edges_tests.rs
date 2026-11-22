//! Comprehensive unit tests covering edge cases for `services::tactical_modules_superweapon_ops`.

use super::*;
use crate::router::ServerContext;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, DamageVector, MovementMode, TargetLock};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
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
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn find_nullsec_sys(sde: &SdeRegistry) -> u32 {
    if sde
        .solar_systems
        .get(&30_004_759)
        .is_some_and(|s| s.security_status <= 0.0)
    {
        return 30_004_759;
    }
    sde.solar_systems
        .values()
        .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759)
}

fn find_lowsec_sys(sde: &SdeRegistry) -> u32 {
    if sde
        .solar_systems
        .get(&30_002_813)
        .is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45)
    {
        return 30_002_813;
    }
    sde.solar_systems
        .values()
        .filter(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.security_status < 0.45
        })
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_813)
}

fn find_highsec_sys(_sde: &SdeRegistry) -> u32 {
    30_000_142
}

/// Deterministically selects a Titan hull type ID (`group_id == 30`).
fn find_titan_type_id(sde: &SdeRegistry) -> u32 {
    if sde.types.get(&11_567).is_some_and(|t| t.group_id == 30) {
        return 11_567;
    }
    sde.types
        .values()
        .filter(|t| t.group_id == 30)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(11_567)
}

/// Deterministically selects a non-Lancer Doomsday module type ID (`group_id == 588`).
fn find_doomsday_type_id(sde: &SdeRegistry) -> u32 {
    if sde
        .types
        .get(&24_550)
        .is_some_and(|t| t.group_id == 588 && !matches!(t.type_id, 77_398..=77_401))
    {
        return 24_550;
    }
    sde.types
        .values()
        .filter(|t| t.group_id == 588 && !matches!(t.type_id, 77_398..=77_401))
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(24_550)
}

/// Deterministically selects a Dreadnought hull type ID (`group_id == 485`) with capital HP attributes.
fn find_dread_type_id(sde: &SdeRegistry) -> u32 {
    if sde
        .types
        .get(&19_720)
        .is_some_and(|t| t.group_id == 485 && t.default_attributes.contains_key(&263))
    {
        return 19_720;
    }
    sde.types
        .values()
        .filter(|t| t.group_id == 485 && t.default_attributes.contains_key(&263))
        .min_by_key(|t| t.type_id)
        .or_else(|| {
            sde.types
                .values()
                .filter(|t| t.group_id == 485)
                .min_by_key(|t| t.type_id)
        })
        .map(|t| t.type_id)
        .unwrap_or(19_720)
}

/// Normalizes test superweapon attributes so non-EM damage channels from unrelated SDE types do not leak.
fn configure_test_superweapon_attrs(t: &mut eve_sde::InvType, fuel_qty: f64, range_m: f64) {
    t.group_id = 588;
    t.category_id = 7;
    t.default_attributes.insert(713, 16_274.0);
    t.default_attributes.insert(714, fuel_qty);
    t.default_attributes.insert(6, 1_000.0);
    t.default_attributes.insert(114, 50_000.0);
    t.default_attributes.insert(116, 0.0);
    t.default_attributes.insert(117, 0.0);
    t.default_attributes.insert(118, 0.0);
    t.default_attributes.insert(54, range_m);
}

fn setup_ship_and_module(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    mod_id: i64,
    mod_type_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.capacitor = cs.max_capacitor.max(200_000.0);
    cs.max_capacitor = cs.capacitor;
    cs.base_max_capacitor = Some(cs.capacitor);
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[test]
fn parse_vec3_and_fuel_check_deduct_refund_and_target_eligibility() {
    let mut ctx = test_ctx();
    let ship_id = 100_800_001;
    let char_id = 90_000_001;

    // parse_vec3_arg branches
    assert!(parse_vec3_arg(&[], 0).unwrap().is_none());
    assert!(parse_vec3_arg(&[PyRep::Integer(5)], 0).is_err());
    assert!(parse_vec3_arg(&[PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(2.0)])], 0).is_err());
    assert!(parse_vec3_arg(&[PyRep::Tuple(vec![
        PyRep::Real(1.0),
        PyRep::Real(f64::NAN),
        PyRep::Real(3.0)
    ])], 0)
    .is_err());
    assert_eq!(
        parse_vec3_arg(&[PyRep::List(vec![
            PyRep::Real(1.0),
            PyRep::Real(2.0),
            PyRep::Real(3.0)
        ])], 0).unwrap(),
        Some(DVec3::new(1.0, 2.0, 3.0))
    );

    // Fuel checks: missing -> partial -> fuel bay priority + cargo -> refund merges & creates new
    let err_missing = check_superweapon_fuel_available(&ctx, ship_id, 16274, 100, "superweapon")
        .unwrap_err();
    assert_eq!(err_missing.code, "InsufficientFuel");
    assert!(err_missing.message.contains("Missing required"));

    ctx.db.with_mut_snapshot(|snap| {
        // Locked stack should be ignored
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Locked,
                quantity: 500,
                ..Default::default()
            },
        );
        // Fuel bay (133) with 40, Cargo (5) with 20 -> total 60 < 100
        snap.items.insert(
            880_002,
            InventoryItemRecord {
                item_id: 880_002,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 40,
                ..Default::default()
            },
        );
        snap.items.insert(
            880_003,
            InventoryItemRecord {
                item_id: 880_003,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 20,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    let err_partial = check_superweapon_fuel_available(&ctx, ship_id, 16274, 100, "superweapon")
        .unwrap_err();
    assert_eq!(err_partial.code, "InsufficientFuel");
    assert!(err_partial.message.contains("Insufficient superweapon fuel"));

    assert_eq!(
        deduct_superweapon_fuel(&ctx, char_id, ship_id, 16274, 100, "superweapon")
            .unwrap_err()
            .code,
        "InsufficientFuel"
    );
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .all(|n| n.event != "OnItemChange"),
        "Failed deduct_superweapon_fuel must not emit OnItemChange"
    );

    // Deduct 50 -> takes all 40 from FuelBay (removes stack 880_002) and 10 from Cargo (leaves 10)
    let (taken, consumed) =
        deduct_superweapon_fuel(&ctx, char_id, ship_id, 16274, 50, "superweapon").unwrap();
    assert_eq!(taken, 50);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&880_002));
        assert_eq!(snap.items[&880_003].quantity, 10);
    });
    let deduct_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        deduct_notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(880_002))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(0))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "Deleted FuelBay stack 880_002 must emit OnItemChange tombstone"
    );
    assert!(
        deduct_notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(880_003))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(10))
        }),
        "Updated Cargo stack 880_003 must emit OnItemChange quantity update"
    );

    // Refund merges 10 back into existing Cargo stack and creates new FuelBay stack for 40
    refund_superweapon_fuel(&ctx, ship_id, char_id, 16274, &[]);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    refund_superweapon_fuel(&ctx, ship_id, char_id, 16274, &consumed);
    ctx.db.with_snapshot(|snap| {
        let total: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == 16274 && i.flag != InventoryFlag::Locked)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(total, 60);
    });
    let refund_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        refund_notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(880_003))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(20))
        }),
        "Refund merging into Cargo stack 880_003 must emit OnItemChange quantity update"
    );
    assert!(
        refund_notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("flagID")
                    == Some(&PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(40))
                && n.args[1] == PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))])
        }),
        "Refund creating new FuelBay stack must emit OnItemChange creation with ixLocationID=0"
    );

    // is_eligible_single_target_doomsday_target
    let struct_id = 1_000_800_999;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                solar_system_id: 30_000_142,
                owner_corp_id: 98_000_001,
                ..Default::default()
            },
        );
    });
    assert!(is_eligible_single_target_doomsday_target(&ctx, struct_id, 587));
    assert!(!is_eligible_single_target_doomsday_target(&ctx, 100_800_111, 587)); // frigate
    assert!(!is_eligible_single_target_doomsday_target(&ctx, 100_800_112, 999_999_999)); // unknown
    // Capital Dreadnought (group 485)
    let dread_tid = find_dread_type_id(&ctx.sde);
    assert!(is_eligible_single_target_doomsday_target(&ctx, 100_800_113, dread_tid));
    let _ = Arc::make_mut(&mut ctx.sde);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_superweapon_titan_single_and_slash_and_error_guards() {
    let mut ctx = test_ctx();
    let null_sys = find_nullsec_sys(&ctx.sde);
    let low_sys = find_lowsec_sys(&ctx.sde);
    let high_sys = find_highsec_sys(&ctx.sde);
    let char_id = 90_000_001;
    let victim_char = 90_000_002;
    let titan_id = 100_810_001;
    let mod_id = 100_810_002;

    // Pick real Titan (group 30) and Doomsday (group 588) from SDE, or synthesize if absent
    let titan_tid = find_titan_type_id(&ctx.sde);
    let dd_tid = find_doomsday_type_id(&ctx.sde);
    let dread_tid = find_dread_type_id(&ctx.sde);

    // Ensure dd_tid has reasonable attributes for testing
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        if let Some(m) = sde_mut.types.get_mut(&dd_tid) {
            configure_test_superweapon_attrs(m, 100.0, 250_000.0);
        }
    }

    // 1. ModuleInReactivationDelay
    ctx.record_module_reactivation_delay(titan_id, mod_id, 60.0);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "ModuleInReactivationDelay"
    );
    ctx.module_reactivation_cooldowns.write().unwrap().clear();

    // 2. InvalidShipGroup (frigate 587)
    setup_ship_and_module(&ctx, null_sys, char_id, titan_id, 587, mod_id, dd_tid, DVec3::ZERO);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "InvalidShipGroup"
    );

    // 3. Security checks (High-sec & Low-sec)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&titan_id).unwrap().type_id = titan_tid;
    });
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, high_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "CannotActivateDoomsdayInHighSec"
    );
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, low_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "CannotActivateLanceInLowSec"
    );

    // 4. ModuleNotFound & InvalidModuleGroup
    // Remove synthetic/SDE type 0 (#System) so missing module yields ModuleNotFound.
    Arc::make_mut(&mut ctx.sde).types.remove(&0);
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, 999_666, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "ModuleNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = 587;
    });
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "InvalidModuleGroup"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = dd_tid;
    });

    // 5. ShipNotInSpace
    let unspawned_titan = 100_810_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            unspawned_titan,
            InventoryItemRecord {
                item_id: unspawned_titan,
                type_id: titan_tid,
                owner_id: char_id,
                location_id: null_sys as i64,
                ..Default::default()
            },
        );
        snap.items.insert(
            100_810_098,
            InventoryItemRecord {
                item_id: 100_810_098,
                type_id: dd_tid,
                owner_id: char_id,
                location_id: unspawned_titan,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        activate_superweapon_or_lance(
            &ctx,
            char_id,
            unspawned_titan,
            null_sys,
            100_810_098,
            "Activate",
            None
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // 6. Tether guard
    let sys = ctx.systems.get_or_spawn(null_sys);
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&titan_id).unwrap().is_tethered = true;
    }
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&titan_id).unwrap().is_tethered = false;
    }

    // 7. Single-target error checks: TargetRequired, InvalidTarget (self), TargetNotInSpace
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        activate_superweapon_or_lance(
            &ctx,
            char_id,
            titan_id,
            null_sys,
            mod_id,
            "Activate",
            Some(titan_id)
        )
        .await
        .unwrap_err()
        .code,
        "InvalidTarget"
    );
    assert_eq!(
        activate_superweapon_or_lance(
            &ctx,
            char_id,
            titan_id,
            null_sys,
            mod_id,
            "Activate",
            Some(999_555_444)
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotInSpace"
    );

    // 8. Target cloaked, tethered, not locked, ineligible hull, out of range
    let tgt_id = 100_810_050;
    setup_ship_and_module(
        &ctx,
        null_sys,
        victim_char,
        tgt_id,
        587, // frigate first
        100_810_051,
        519,
        DVec3::new(10_000.0, 0.0, 0.0),
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&tgt_id).unwrap().is_cloaked = true;
    }
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
            .await
            .unwrap_err()
            .code,
        "CannotTargetCloaked"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&tgt_id).unwrap();
        cs.is_cloaked = false;
        cs.is_tethered = true;
    }
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&tgt_id).unwrap().is_tethered = false;
    }
    // Not locked -> TargetNotLocked
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
            .await
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );
    // Lock target -> frigate -> InvalidTarget
    let _ = sys
        .add_target_lock(
            titan_id,
            TargetLock {
                target_id: tgt_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );

    // Upgrade target to Dreadnought (485) and move out of range -> TargetOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&tgt_id).unwrap().type_id = dread_tid;
    });
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&tgt_id).unwrap().position = DVec3::new(300_000.0, 0.0, 0.0);
    }
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // Move target into range, insufficient cap -> NotEnoughCapacitor
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&tgt_id).unwrap().position = DVec3::new(10_000.0, 0.0, 0.0);
        bp.combat_states.get_mut(&titan_id).unwrap().capacitor = 10.0;
    }
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, titan_id, null_sys, mod_id, "Activate", Some(tgt_id))
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // Restore cap, add fuel, and fire single-target Doomsday!
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&titan_id).unwrap().capacitor = 200_000.0;
        let tcs = bp.combat_states.get_mut(&tgt_id).unwrap();
        tcs.shield_hp = 100.0;
        tcs.armor_hp = 100.0;
        tcs.structure_hp = 100.0;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_100,
            InventoryItemRecord {
                item_id: 880_100,
                type_id: 16274,
                owner_id: char_id,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                ..Default::default()
            },
        );
    });
    let dd_rep = activate_superweapon_or_lance(
        &ctx,
        char_id,
        titan_id,
        null_sys,
        mod_id,
        "Activate",
        Some(tgt_id),
    )
    .await
    .unwrap();
    assert_eq!(dd_rep.dict_get("slashMode").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(dd_rep.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(100));

    // 9. Slash mode: invalid direction vector -> InvalidDirectionVector, then valid slash fire!
    sys.step_manual(35.0).await.unwrap();
    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    assert_eq!(
        activate_superweapon_with_args(
            &ctx,
            char_id,
            titan_id,
            null_sys,
            mod_id,
            "ActivateSlashModule",
            None,
            &[
                PyRep::None,
                PyRep::Tuple(vec![PyRep::Real(f64::INFINITY), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidDirectionVector"
    );

    let slash_target = 100_810_060;
    setup_ship_and_module(
        &ctx,
        null_sys,
        victim_char,
        slash_target,
        587,
        100_810_061,
        519,
        DVec3::new(50_000.0, 0.0, 0.0),
    );
    let slash_rep = activate_superweapon_with_args(
        &ctx,
        char_id,
        titan_id,
        null_sys,
        mod_id,
        "ActivateSlashModule",
        None,
        &[
            PyRep::None,
            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Real(10_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(slash_rep.dict_get("slashMode").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lancer_dreadnought_activation_and_fire_lance_warmup_in_ballpark() {
    let mut ctx = test_ctx();
    let low_sys = find_lowsec_sys(&ctx.sde);
    let char_id = 90_000_001;
    let victim_char = 90_000_002;
    let lancer_id = 100_820_001;
    let lance_mod = 100_820_002;
    let siege_mod = 100_820_003;
    let target_id = 100_820_050;

    let lancer_tid = 77281u32; // Bane / Lancer Dreadnought or synthetic
    let lance_tid = 77398u32;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut ship_t = sde_mut.types[&587].clone();
        ship_t.type_id = lancer_tid;
        ship_t.group_id = 4594;
        ship_t.default_attributes.insert(5418, 20.0);
        ship_t.default_attributes.insert(5417, 10.0);
        sde_mut.types.insert(lancer_tid, ship_t);

        let mut mod_t = sde_mut.types[&587].clone();
        mod_t.type_id = lance_tid;
        mod_t.group_id = 588;
        mod_t.default_attributes.insert(6, 1_000.0);
        mod_t.default_attributes.insert(54, 50_000.0);
        mod_t.default_attributes.insert(713, 16274.0);
        mod_t.default_attributes.insert(714, 100.0);
        mod_t.default_attributes.insert(114, 5_000.0);
        mod_t.default_attributes.insert(2264, 5_000.0);
        mod_t.default_attributes.insert(2265, 1_000.0);
        mod_t.default_attributes.insert(2263, 2_500.0);
        mod_t.default_attributes.insert(2259, 10_000.0);
        sde_mut.types.insert(lance_tid, mod_t);
    }

    setup_ship_and_module(&ctx, low_sys, char_id, lancer_id, lancer_tid, lance_mod, lance_tid, DVec3::ZERO);

    // 1. No active siege -> SiegeRequiredForLance
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "SiegeRequiredForLance"
    );

    // 2. Add active Siege (20280), missing skills -> SkillMissing
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siege_mod,
            InventoryItemRecord {
                item_id: siege_mod,
                type_id: 20280,
                owner_id: char_id,
                location_id: lancer_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(siege_mod);
    });
    assert_eq!(
        activate_superweapon_or_lance(&ctx, char_id, lancer_id, low_sys, lance_mod, "Activate", None)
            .await
            .unwrap_err()
            .code,
        "SkillMissing"
    );

    // 3. Grant skills + fuel + target in beam cone -> Lancer succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            77739,
            eve_db::TrainedSkillRecord {
                type_id: 77739,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            77738,
            eve_db::TrainedSkillRecord {
                type_id: 77738,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            880_200,
            InventoryItemRecord {
                item_id: 880_200,
                type_id: 16274,
                owner_id: char_id,
                location_id: lancer_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                ..Default::default()
            },
        );
    });
    setup_ship_and_module(
        &ctx,
        low_sys,
        victim_char,
        target_id,
        587,
        100_820_051,
        519,
        DVec3::new(10_000.0, 0.0, 0.0),
    );
    let rep = activate_superweapon_or_lance(
        &ctx,
        char_id,
        lancer_id,
        low_sys,
        lance_mod,
        "Activate",
        Some(target_id),
    )
    .await
    .unwrap();
    assert_eq!(rep.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));

    // 4. Test fire_lance_warmup_in_ballpark directly (including missing ship, hit & destroy)
    let target_id_2 = 100_820_052;
    setup_ship_and_module(
        &ctx,
        low_sys,
        victim_char,
        target_id_2,
        587,
        100_820_053,
        519,
        DVec3::new(0.0, 10_000.0, 0.0),
    );
    let sys = ctx.systems.get_or_spawn(low_sys);
    let mut bp = sys.ballpark.write().unwrap();
    let mut pending = Vec::new();
    let mut destroyed = Vec::new();

    // Missing caster ship -> no-op
    let missing_cycle = ActiveModuleCycle {
        ship_id: 999_111_222,
        module_id: lance_mod,
        module_type_id: lance_tid,
        ..Default::default()
    };
    fire_lance_warmup_in_ballpark(
        &mut bp,
        &missing_cycle,
        &Some(Arc::clone(&ctx.sde)),
        &Some(Arc::clone(&ctx.db)),
        &ctx.tactical_state,
        &mut pending,
        &mut destroyed,
    );
    assert!(pending.is_empty());

    // Valid caster cycle hits & destroys target_id_2 along +Y
    if let Some(tcs) = bp.combat_states.get_mut(&target_id_2) {
        tcs.shield_hp = 10.0;
        tcs.armor_hp = 10.0;
        tcs.structure_hp = 10.0;
    }
    let valid_cycle = ActiveModuleCycle {
        ship_id: lancer_id,
        module_id: lance_mod,
        module_type_id: lance_tid,
        owner_char_id: Some(char_id),
        damage_vector: DamageVector::new(100_000.0, 0.0, 0.0, 0.0),
        lance_beam_dir: Some(DVec3::Y),
        ..Default::default()
    };
    fire_lance_warmup_in_ballpark(
        &mut bp,
        &valid_cycle,
        &Some(Arc::clone(&ctx.sde)),
        &Some(Arc::clone(&ctx.db)),
        &ctx.tactical_state,
        &mut pending,
        &mut destroyed,
    );
    assert_eq!(pending.len(), 1);
    assert!(destroyed.contains(&(target_id_2, Some(char_id))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doomsday_activation_exact_fuel_deletion_emits_on_item_change_tombstone_gh1038() {
    let mut ctx = test_ctx();
    let null_sys = find_nullsec_sys(&ctx.sde);
    let char_id = 90_000_001_i64;
    let victim_char = 90_000_002_i64;
    let titan_id = 100_830_001_i64;
    let mod_id = 100_830_002_i64;
    let tgt_id = 100_830_050_i64;
    let fuel_stack_id = 1_897_129_274_i64;

    let titan_tid = find_titan_type_id(&ctx.sde);
    let dd_tid = find_doomsday_type_id(&ctx.sde);
    let dread_tid = find_dread_type_id(&ctx.sde);

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        if let Some(m) = sde_mut.types.get_mut(&dd_tid) {
            configure_test_superweapon_attrs(m, 50_000.0, 250_000.0);
        }
    }

    setup_ship_and_module(&ctx, null_sys, char_id, titan_id, titan_tid, mod_id, dd_tid, DVec3::ZERO);
    setup_ship_and_module(
        &ctx,
        null_sys,
        victim_char,
        tgt_id,
        dread_tid,
        100_830_051,
        519,
        DVec3::new(10_000.0, 0.0, 0.0),
    );
    let sys = ctx.systems.get_or_spawn(null_sys);
    let _ = sys
        .add_target_lock(
            titan_id,
            TargetLock {
                target_id: tgt_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_stack_id,
            InventoryItemRecord {
                item_id: fuel_stack_id,
                type_id: 16274,
                owner_id: char_id,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    let rep = activate_superweapon_or_lance(
        &ctx,
        char_id,
        titan_id,
        null_sys,
        mod_id,
        "Activate",
        Some(tgt_id),
    )
    .await
    .expect("Doomsday activation with exact 50,000 Helium Isotopes must succeed");
    assert_eq!(rep.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(50_000));
    assert!(
        ctx.db.with_snapshot(|snap| !snap.items.contains_key(&fuel_stack_id)),
        "Consumed fuel stack must be removed from WorldSnapshot"
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(fuel_stack_id))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(0))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "Doomsday exact fuel stack deletion must emit OnItemChange tombstone for client invCache"
    );
}

#[tokio::test]
async fn f02_superweapon_and_lance_preserve_gate_cloak_on_rejections_and_break_on_commit() {
    let mut ctx = test_ctx();
    let null_sys = find_nullsec_sys(&ctx.sde);
    let low_sys = find_lowsec_sys(&ctx.sde);
    let char_id = 90_000_001_i64;
    let victim_char = 90_000_002_i64;

    let lancer_tid = 77_281_u32;
    let lance_tid = 77_398_u32;
    let titan_tid = find_titan_type_id(&ctx.sde);
    let dd_tid = find_doomsday_type_id(&ctx.sde);
    let dread_tid = find_dread_type_id(&ctx.sde);

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let fallback_ship = sde_mut.types.get(&dread_tid).cloned().unwrap_or_else(|| {
            sde_mut
                .types
                .values()
                .min_by_key(|t| t.type_id)
                .cloned()
                .expect("seed type")
        });
        let mut lancer_ship_t = sde_mut.types.get(&lancer_tid).cloned().unwrap_or(fallback_ship.clone());
        lancer_ship_t.type_id = lancer_tid;
        lancer_ship_t.group_id = 4594;
        lancer_ship_t.category_id = 6;
        sde_mut.types.insert(lancer_tid, lancer_ship_t);

        let fallback_mod = sde_mut.types.get(&dd_tid).cloned().unwrap_or(fallback_ship);
        let mut dd_t = sde_mut.types.get(&dd_tid).cloned().unwrap_or(fallback_mod.clone());
        dd_t.type_id = dd_tid;
        configure_test_superweapon_attrs(&mut dd_t, 100.0, 250_000.0);
        sde_mut.types.insert(dd_tid, dd_t);

        let mut lance_t = sde_mut.types.get(&lance_tid).cloned().unwrap_or(fallback_mod);
        lance_t.type_id = lance_tid;
        configure_test_superweapon_attrs(&mut lance_t, 100.0, 150_000.0);
        sde_mut.types.insert(lance_tid, lance_t);
    }

    // 1. Disruptive Lance (is_lancer == true): rejected tether/cap/fuel paths preserve gate cloak; valid breaks it.
    let lancer_id = 100_830_001_i64;
    let lance_mod = 100_830_002_i64;
    let siege_mod = 100_830_003_i64;
    setup_ship_and_module(&ctx, low_sys, char_id, lancer_id, lancer_tid, lance_mod, lance_tid, DVec3::ZERO);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siege_mod,
            InventoryItemRecord {
                item_id: siege_mod,
                type_id: 20_280,
                owner_id: char_id,
                location_id: lancer_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(siege_mod);
        for sk in [77_739_u32, 77_738_u32] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let sys_low = ctx.systems.get_or_spawn(low_sys);
    let _ = ctx
        .db
        .acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());
    let set_lancer_gate_cloak = |tethered: bool, cap: f64| {
        let mut bp = sys_low.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&lancer_id).unwrap();
        cs.is_tethered = tethered;
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 30;
        cs.undock_invulnerable_remaining_ticks = 15;
        cs.capacitor = cap;
        cs.max_capacitor = 200_000.0;
        drop(bp);
        crate::services::space_destiny::grant_location_change_invulnerability(
            &ctx,
            char_id,
            ctx.now_unix() + 60,
        );
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    for (tethered, cap, expected_code, label) in [
        (true, 200_000.0, "CantActivateWhileTethered", "tethered"),
        (false, 10.0, "NotEnoughCapacitor", "capacitor"),
        (false, 200_000.0, "InsufficientFuel", "fuel"),
    ] {
        set_lancer_gate_cloak(tethered, cap);
        let err = activate_superweapon_with_args(&ctx, char_id, lancer_id, low_sys, lance_mod, "Activate", None, &[])
            .await
            .expect_err("rejected Lancer activation must return Err");
        assert_eq!(err.code, expected_code, "Lancer {label} rejection");
        let cs = sys_low.get_combat_state(lancer_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "Lancer {label} must preserve is_cloaked");
        assert_eq!(cs.gate_cloak_remaining_ticks, 30, "Lancer {label} must preserve gate cloak");
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 15, "Lancer {label} must preserve undock invuln");
        assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()), "Lancer {label} must preserve location_change_invuln");
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_830_004,
            InventoryItemRecord {
                item_id: 100_830_004,
                type_id: 16_274,
                owner_id: char_id,
                location_id: lancer_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    set_lancer_gate_cloak(false, 200_000.0);
    activate_superweapon_with_args(&ctx, char_id, lancer_id, low_sys, lance_mod, "Activate", None, &[])
        .await
        .expect("valid gate-cloaked Lancer activation must succeed");
    let cs_lance_ok = sys_low.get_combat_state(lancer_id).await.unwrap().unwrap();
    assert!(!cs_lance_ok.is_cloaked);
    assert_eq!(cs_lance_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_lance_ok.undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    let lance_notes = ctx.drain_notifications_for_char(char_id);
    assert!(lance_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert_eq!(lance_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    // 2. Titan Doomsday (!is_lancer): rejected tether/lock/cap/fuel paths preserve gate cloak; valid breaks it.
    let titan_id = 100_830_011_i64;
    let dd_mod = 100_830_012_i64;
    let tgt_id = 100_830_050_i64;
    setup_ship_and_module(&ctx, null_sys, char_id, titan_id, titan_tid, dd_mod, dd_tid, DVec3::ZERO);
    setup_ship_and_module(
        &ctx,
        null_sys,
        victim_char,
        tgt_id,
        dread_tid,
        100_830_051,
        519,
        DVec3::new(10_000.0, 0.0, 0.0),
    );
    let sys_null = ctx.systems.get_or_spawn(null_sys);
    let _ = ctx
        .db
        .acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());
    let set_titan_gate_cloak = |tethered: bool, cap: f64| {
        let mut bp = sys_null.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&titan_id).unwrap();
        cs.is_tethered = tethered;
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 30;
        cs.undock_invulnerable_remaining_ticks = 15;
        cs.capacitor = cap;
        cs.max_capacitor = 200_000.0;
        drop(bp);
        crate::services::space_destiny::grant_location_change_invulnerability(
            &ctx,
            char_id,
            ctx.now_unix() + 60,
        );
    };
    let _ = ctx.drain_notifications_for_char(char_id);

    set_titan_gate_cloak(true, 200_000.0);
    let dd_teth_err = activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(tgt_id), &[])
        .await
        .expect_err("tethered Doomsday must reject");
    assert_eq!(dd_teth_err.code, "CantActivateWhileTethered");
    let cs_teth = sys_null.get_combat_state(titan_id).await.unwrap().unwrap();
    assert_eq!(cs_teth.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_teth.undock_invulnerable_remaining_ticks, 15);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled"));

    set_titan_gate_cloak(false, 200_000.0);
    let dd_lock_err = activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(tgt_id), &[])
        .await
        .expect_err("unlocked target Doomsday must reject");
    assert_eq!(dd_lock_err.code, "TargetNotLocked");
    let cs_lock = sys_null.get_combat_state(titan_id).await.unwrap().unwrap();
    assert_eq!(cs_lock.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_lock.undock_invulnerable_remaining_ticks, 15);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled"));

    sys_null
        .add_target_lock(
            titan_id,
            TargetLock {
                target_id: tgt_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    for (cap, expected_code, label) in [
        (10.0, "NotEnoughCapacitor", "capacitor"),
        (200_000.0, "InsufficientFuel", "fuel"),
    ] {
        set_titan_gate_cloak(false, cap);
        let err = activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(tgt_id), &[])
            .await
            .expect_err("rejected Doomsday activation must return Err");
        assert_eq!(err.code, expected_code, "Doomsday {label} rejection");
        let cs = sys_null.get_combat_state(titan_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "Doomsday {label} must preserve is_cloaked");
        assert_eq!(cs.gate_cloak_remaining_ticks, 30, "Doomsday {label} must preserve gate cloak");
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 15, "Doomsday {label} must preserve undock invuln");
        assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()), "Doomsday {label} must preserve location_change_invuln");
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_830_014,
            InventoryItemRecord {
                item_id: 100_830_014,
                type_id: 16_274,
                owner_id: char_id,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    set_titan_gate_cloak(false, 200_000.0);
    activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(tgt_id), &[])
        .await
        .expect("valid gate-cloaked Doomsday activation must succeed");
    let cs_dd_ok = sys_null.get_combat_state(titan_id).await.unwrap().unwrap();
    assert!(!cs_dd_ok.is_cloaked);
    assert_eq!(cs_dd_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_dd_ok.undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    let dd_notes = ctx.drain_notifications_for_char(char_id);
    assert!(dd_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert_eq!(dd_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    // Non-gate-cloaked undock invuln (gate_cloak=0, undock_invuln=15) + location_change_invuln preserved on rejection, cleared on commit.
    assert!(sys_null.has_module_cycle(titan_id, dd_mod).await);
    sys_null.step_manual(35.0).await.unwrap();
    assert!(!sys_null.has_module_cycle(titan_id, dd_mod).await);
    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    {
        let mut bp = sys_null.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&titan_id).unwrap();
        cs.is_tethered = true;
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 15;
        cs.capacitor = 200_000.0;
    }
    crate::services::space_destiny::grant_location_change_invulnerability(&ctx, char_id, ctx.now_unix() + 60);
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(tgt_id), &[])
            .await
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );
    assert_eq!(sys_null.get_combat_state(titan_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);
    assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled"));

    {
        let mut bp = sys_null.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&titan_id).unwrap();
        cs.is_tethered = false;
    }
    activate_superweapon_with_args(&ctx, char_id, titan_id, null_sys, dd_mod, "Activate", Some(tgt_id), &[])
        .await
        .expect("non-gate undock invuln Doomsday commit must succeed");
    assert_eq!(sys_null.get_combat_state(titan_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 0);
    assert!(!crate::services::space_destiny::is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    let nc_dd_notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(nc_dd_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    assert!(!nc_dd_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
}

struct SwLancerIds {
    null_sys: u32,
    low_sys: u32,
    char_id: i64,
    titan_id: i64,
    dd_mod: i64,
    dd_tid: u32,
    tgt_id: i64,
    titan_fuel: i64,
    lancer_id: i64,
    lance_mod: i64,
    lance_tgt: i64,
    lance_fuel: i64,
}

async fn setup_superweapon_lancer_fixture(base: i64) -> (ServerContext, SwLancerIds) {
    let mut ctx = test_ctx();
    let (null_sys, low_sys) = (find_nullsec_sys(&ctx.sde), find_lowsec_sys(&ctx.sde));
    let (char_id, victim_char) = (90_000_001_i64, 90_000_002_i64);
    let ids = SwLancerIds {
        null_sys, low_sys, char_id,
        titan_id: base + 1, dd_mod: base + 2, dd_tid: find_doomsday_type_id(&ctx.sde), tgt_id: base + 50, titan_fuel: base + 901,
        lancer_id: base + 101, lance_mod: base + 102, lance_tgt: base + 150, lance_fuel: base + 902,
    };
    let (titan_tid, dread_tid, lancer_tid, lance_tid, siege_tid) = (find_titan_type_id(&ctx.sde), find_dread_type_id(&ctx.sde), 77_281_u32, 77_398_u32, 4_292_u32);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        if let Some(m) = sde.types.get_mut(&ids.dd_tid) { configure_test_superweapon_attrs(m, 100.0, 250_000.0); }
        for (tid, grp, l_attrs) in [(lancer_tid, 4594, true), (lance_tid, 588, false), (siege_tid, 515, false), (crate::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE, 4644, false)] {
            let mut t = sde.types[&587].clone();
            t.type_id = tid; t.group_id = grp;
            if l_attrs { t.default_attributes.insert(5418, 20.0); t.default_attributes.insert(5417, 10.0); }
            if grp == 588 { configure_test_superweapon_attrs(&mut t, 100.0, 150_000.0); t.default_attributes.insert(6, 1_000.0); }
            sde.types.insert(tid, t);
        }
    }
    setup_ship_and_module(&ctx, null_sys, char_id, ids.titan_id, titan_tid, ids.dd_mod, ids.dd_tid, DVec3::ZERO);
    setup_ship_and_module(&ctx, null_sys, victim_char, ids.tgt_id, dread_tid, base + 51, 519, DVec3::new(10_000.0, 0.0, 0.0));
    setup_ship_and_module(&ctx, low_sys, char_id, ids.lancer_id, lancer_tid, ids.lance_mod, lance_tid, DVec3::ZERO);
    setup_ship_and_module(&ctx, low_sys, victim_char, ids.lance_tgt, dread_tid, base + 151, 519, DVec3::new(10_000.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(base + 103, InventoryItemRecord { item_id: base + 103, type_id: siege_tid, owner_id: char_id, location_id: ids.lancer_id, flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default() });
        snap.module_active_states.insert(base + 103);
        for (fid, loc) in [(ids.titan_fuel, ids.titan_id), (ids.lance_fuel, ids.lancer_id)] {
            snap.items.insert(fid, InventoryItemRecord { item_id: fid, type_id: 16_274, owner_id: char_id, location_id: loc, flag: InventoryFlag::Cargo, quantity: 500, ..Default::default() });
        }
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3439_u32, 76079, 76080, 77738, 77739] { sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 }); }
        if let Some(m) = ctx.sde.types.get(&ids.dd_tid) {
            for aid in [182u16, 183, 184] { if let Some(&sid) = m.default_attributes.get(&aid) { sk.insert(sid as u32, eve_db::TrainedSkillRecord { type_id: sid as u32, level: 5, skill_points: 256_000.0 }); } }
        }
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Red); });
    let _ = ctx.db.acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.systems.get_or_spawn(null_sys).add_target_lock(ids.titan_id, TargetLock { target_id: ids.tgt_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    (ctx, ids)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doomsday_and_lancer_reject_active_cycle_warp_and_cloak_atomically_bf02() {
    let (ctx, f) = setup_superweapon_lancer_fixture(100_840_000).await;
    let (sys_null, sys_low) = (ctx.systems.get_or_spawn(f.null_sys), ctx.systems.get_or_spawn(f.low_sys));
    let init_tgt = sys_null.get_combat_state(f.tgt_id).await.unwrap().unwrap().shield_hp;
    let init_ltgt = sys_low.get_combat_state(f.lance_tgt).await.unwrap().unwrap().shield_hp;

    // 1. Doomsday: active cycle, warp, non-gate cloak reject atomically BEFORE damage/fuel/cap/invuln.
    sys_null.add_module_cycle(ActiveModuleCycle { ship_id: f.titan_id, module_id: f.dd_mod, module_type_id: f.dd_tid, owner_char_id: Some(f.char_id), effect_name: "superWeaponImmobilize".into(), duration_s: 30.0, immobilize_self: true, ..Default::default() }).await.unwrap();
    for (case, expected_code) in [(0, "ModuleAlreadyActive"), (1, "CantActivateInWarp"), (2, "CannotActivateCloaked")] {
        if case == 1 { sys_null.step_manual(35.0).await.unwrap(); }
        {
            let mut bp = sys_null.ballpark.write().unwrap();
            bp.balls.get_mut(&f.titan_id).unwrap().mode = if case == 1 { MovementMode::Warp { origin: DVec3::ZERO, destination: DVec3::new(1_000_000.0, 0.0, 0.0), warp_speed_au_s: 3.0, total_distance: 1_000_000.0, elapsed_warp_time: 1.0, phase: eve_destiny::WarpPhase::Cruising } } else { MovementMode::Stop };
            let cs = bp.combat_states.get_mut(&f.titan_id).unwrap();
            cs.capacitor = 200_000.0; cs.is_cloaked = case == 2; cs.gate_cloak_remaining_ticks = if case == 0 { 30 } else { 0 }; cs.undock_invulnerable_remaining_ticks = 15;
        }
        crate::services::space_destiny::grant_location_change_invulnerability(&ctx, f.char_id, ctx.now_unix() + 60);
        let _ = ctx.drain_notifications_for_char(f.char_id);
        assert_eq!(activate_superweapon_with_args(&ctx, f.char_id, f.titan_id, f.null_sys, f.dd_mod, "Activate", Some(f.tgt_id), &[]).await.unwrap_err().code, expected_code);
        let cs = sys_null.get_combat_state(f.titan_id).await.unwrap().unwrap();
        assert_eq!((cs.capacitor, cs.undock_invulnerable_remaining_ticks, cs.is_cloaked), (200_000.0, 15, case == 2));
        assert_eq!((sys_null.get_combat_state(f.tgt_id).await.unwrap().unwrap().shield_hp, ctx.db.with_snapshot(|s| s.items[&f.titan_fuel].quantity)), (init_tgt, 500));
        assert!(crate::services::space_destiny::is_location_change_invulnerable(&ctx, f.char_id, ctx.now_unix()));
        assert!(ctx.drain_notifications_for_char(f.char_id).is_empty());
    }

    // 2. Lancer (`P2-03`): warp & non-gate cloak reject BEFORE damage/fuel/cap/invuln.
    for (in_warp, expected_code) in [(true, "CantActivateInWarp"), (false, "CannotActivateCloaked")] {
        {
            let mut bp = sys_low.ballpark.write().unwrap();
            bp.balls.get_mut(&f.lancer_id).unwrap().mode = if in_warp { MovementMode::Warp { origin: DVec3::ZERO, destination: DVec3::new(1_000_000.0, 0.0, 0.0), warp_speed_au_s: 3.0, total_distance: 1_000_000.0, elapsed_warp_time: 1.0, phase: eve_destiny::WarpPhase::Cruising } } else { MovementMode::Stop };
            let cs = bp.combat_states.get_mut(&f.lancer_id).unwrap();
            cs.capacitor = 200_000.0; cs.is_cloaked = !in_warp; cs.gate_cloak_remaining_ticks = 0; cs.undock_invulnerable_remaining_ticks = 12;
        }
        let _ = ctx.drain_notifications_for_char(f.char_id);
        assert_eq!(activate_superweapon_with_args(&ctx, f.char_id, f.lancer_id, f.low_sys, f.lance_mod, "Activate", None, &[]).await.unwrap_err().code, expected_code);
        let cs = sys_low.get_combat_state(f.lancer_id).await.unwrap().unwrap();
        assert_eq!((cs.capacitor, cs.undock_invulnerable_remaining_ticks, cs.is_cloaked), (200_000.0, 12, !in_warp));
        assert_eq!((sys_low.get_combat_state(f.lance_tgt).await.unwrap().unwrap().shield_hp, ctx.db.with_snapshot(|s| s.items[&f.lance_fuel].quantity)), (init_ltgt, 500));
        assert!(ctx.drain_notifications_for_char(f.char_id).is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn superweapon_concurrent_lancer_fob_precheck_and_reserved_fuel_f01_f05() {
    let (ctx, f) = setup_superweapon_lancer_fixture(100_850_000).await;
    let (sys_low, sys_null) = (ctx.systems.get_or_spawn(f.low_sys), ctx.systems.get_or_spawn(f.null_sys));
    let init_ltgt = sys_low.get_combat_state(f.lance_tgt).await.unwrap().unwrap().shield_hp;

    // 1. Concurrent Lancer double-fire via tokio::join! breaks gate cloak once and deducts cap/fuel once.
    {
        let mut bp = sys_low.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&f.lancer_id).unwrap();
        cs.capacitor = 200_000.0; cs.is_cloaked = true; cs.gate_cloak_remaining_ticks = 20; cs.undock_invulnerable_remaining_ticks = 10;
    }
    let _ = ctx.drain_notifications_for_char(f.char_id);
    let (r1, r2) = tokio::join!(
        activate_superweapon_with_args(&ctx, f.char_id, f.lancer_id, f.low_sys, f.lance_mod, "Activate", None, &[]),
        activate_superweapon_with_args(&ctx, f.char_id, f.lancer_id, f.low_sys, f.lance_mod, "Activate", None, &[]),
    );
    let (ok_rep, err_code) = match (r1, r2) { (Ok(rep), Err(e)) | (Err(e), Ok(rep)) => (rep, e.code), other => panic!("expected 1 ok/1 err, got {other:?}") };
    assert_eq!(err_code, "ModuleInReactivationDelay");
    assert_eq!(ok_rep.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    let lcs = sys_low.get_combat_state(f.lancer_id).await.unwrap().unwrap();
    assert_eq!((ctx.db.with_snapshot(|s| s.items[&f.lance_fuel].quantity), lcs.is_cloaked, lcs.undock_invulnerable_remaining_ticks), (400, false, 0));
    assert!((lcs.capacitor - 199_250.0).abs() < 1e-3 && sys_low.get_combat_state(f.lance_tgt).await.unwrap().is_none_or(|cs| cs.shield_hp < init_ltgt));
    let lnotes = ctx.drain_notifications_for_char(f.char_id);
    assert_eq!((lnotes.iter().filter(|n| n.event == "OnJumpCloakCancelled").count(), lnotes.iter().filter(|n| n.event == "OnInvulnCancelled").count()), (1, 1));

    // 2. Pirate FOB friendly-militia Doomsday rejection occurs BEFORE fuel/cap/invuln mutation.
    let (fob_id, now) = (crate::services::insurgency_services::PIRATE_FOB_STRUCTURE_BASE_ID + 77, ctx.now_unix());
    let camp_sys: Vec<u32> = (0..5).map(|i| f.null_sys + i).collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&f.char_id).unwrap().war_faction_id = Some(500_011);
        snap.structures.insert(fob_id, eve_db::UpwellStructureRecord {
            structure_id: fob_id, type_id: crate::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE,
            solar_system_id: f.null_sys, state: "ShieldVulnerable".into(), shield_hp: 2_500.0, armor_hp: 2_500.0, hull_hp: 3_500.0, ..Default::default()
        });
        snap.insurgency_runtime.campaigns.insert(77, eve_db::PersistedInsurgencyCampaign {
            campaign_id: 77, pirate_faction_id: 500_011, target_faction_id: 500_001, fob_solar_system_id: f.null_sys,
            suppression_points: camp_sys.iter().map(|&s| (s, 1_500_i64)).collect(), systems: camp_sys,
            created_at_unix: now.saturating_sub(2_000), forecasting_end_unix: now.saturating_sub(1_000), active_end_unix: now + 9_000, winddown_end_unix: now + 10_000, ..Default::default()
        });
    });
    sys_null.try_spawn_ball_with_combat(
        Ball { id: fob_id, type_id: crate::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 5_000.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        eve_destiny::CombatState { shield_hp: 2_500.0, max_shield_hp: 2_500.0, armor_hp: 2_500.0, max_armor_hp: 2_500.0, structure_hp: 3_500.0, max_structure_hp: 3_500.0, ..Default::default() },
    );
    let _ = sys_null.add_target_lock(f.titan_id, TargetLock { target_id: fob_id, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    { let mut bp = sys_null.ballpark.write().unwrap(); let cs = bp.combat_states.get_mut(&f.titan_id).unwrap(); cs.capacitor = 200_000.0; cs.undock_invulnerable_remaining_ticks = 9; }
    let _ = ctx.drain_notifications_for_char(f.char_id);
    assert_eq!(activate_superweapon_with_args(&ctx, f.char_id, f.titan_id, f.null_sys, f.dd_mod, "Activate", Some(fob_id), &[]).await.unwrap_err().code, "CannotAttackFriendlyStructure");
    let tcs = sys_null.get_combat_state(f.titan_id).await.unwrap().unwrap();
    assert_eq!((tcs.capacitor, tcs.undock_invulnerable_remaining_ticks, ctx.db.with_snapshot(|s| s.items[&f.titan_fuel].quantity), ctx.db.with_snapshot(|s| s.structures[&fob_id].shield_hp)), (200_000.0, 9, 500, 2_500.0));
    assert!(ctx.drain_notifications_for_char(f.char_id).is_empty());

    // 3. Superweapon fuel reservation rollback mints clean stack when source stack locks/moves (F-01) and emits OnItemChange diffs (F1486-2B-R5-01).
    let _ = ctx.db.acquire_online_lock(1, 1, Some(f.char_id), true, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(f.char_id);
    let (_, _, reserved, _) = deduct_superweapon_fuel_with_diffs(&ctx, f.titan_id, 16_274, 100, "superweapon").unwrap();
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&f.titan_fuel).unwrap().flag = InventoryFlag::Locked; });
    refund_superweapon_fuel_reserved(&ctx, f.char_id, &reserved);
    let (lq, tq) = ctx.db.with_snapshot(|s| (s.items[&f.titan_fuel].quantity, s.items.values().filter(|i| i.location_id == f.titan_id && i.type_id == 16_274).map(|i| i.quantity).sum::<i32>()));
    assert_eq!((lq, tq), (400, 500));
    let diverged_notifs = ctx.drain_notifications_for_char(f.char_id);
    assert_eq!(diverged_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 2, "diverged rollback must sync both reserved and minted stacks");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn superweapon_overlapping_reserve_rollback_and_deferred_emit_bf1486_2b_r6_02() {
    let (ctx, f) = setup_superweapon_lancer_fixture(100_860_000).await;
    let _ = ctx.db.acquire_online_lock(1, 1, Some(f.char_id), true, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&f.titan_fuel).unwrap().quantity = 50_000;
    });
    let _ = ctx.drain_notifications_for_char(f.char_id);

    // 1. Call A reserves 25_000 (50_000 -> 25_000); Call B reserves remaining 25_000 (25_000 -> None).
    let (_, _, res_a, _) =
        deduct_superweapon_fuel_with_diffs(&ctx, f.titan_id, 16_274, 25_000, "superweapon")
            .unwrap();
    let (_, _, res_b, diffs_b) =
        deduct_superweapon_fuel_with_diffs(&ctx, f.titan_id, 16_274, 25_000, "superweapon")
            .unwrap();
    assert_eq!(diffs_b.len(), 1);
    assert!(
        diffs_b[0].1.is_none(),
        "Call B must have captured pre-await None on full depletion"
    );

    // 2. Call A fails pre-commit and rolls back 25_000; Call B succeeds and emits deferred diffs.
    refund_superweapon_fuel_reserved(&ctx, f.char_id, &res_a);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&f.titan_fuel].quantity),
        25_000
    );
    emit_superweapon_fuel_item_diffs(&ctx, f.char_id, &diffs_b);

    // 3. Authoritative DB stays 25_000, ZERO locJunkyard tombstones emitted, and client syncs to live 25_000.
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&f.titan_fuel].quantity),
        25_000
    );
    let col_i64 = |row: &PyRep, col: &str| -> Option<i64> {
        match row {
            PyRep::DBRow { columns, .. } => columns
                .iter()
                .find(|(k, _)| k == col)
                .and_then(|(_, v)| v.as_i64()),
            PyRep::KeyVal(map) => map.get(col).and_then(PyRep::as_i64),
            _ => None,
        }
    };
    let item_notifs: Vec<_> = ctx
        .drain_notifications_for_char(f.char_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert!(
        !item_notifs.is_empty(),
        "deferred emit after overlapping rollback must emit live quantity convergence OnItemChange"
    );
    assert!(
        item_notifs
            .iter()
            .all(|n| col_i64(&n.args[0], "locationID") != Some(6)),
        "BF-1486-2B-R6-02: deferred emit must NEVER emit stale locJunkyard (locationID=6) tombstone when rollback restored fuel_id"
    );
    let last = item_notifs.last().unwrap();
    assert_eq!(col_i64(&last.args[0], "itemID"), Some(f.titan_fuel));
    assert_eq!(col_i64(&last.args[0], "locationID"), Some(f.titan_id));
    assert_eq!(col_i64(&last.args[0], "quantity"), Some(25_000));
    drop(res_b);
}

