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
    sde.solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759)
}

fn find_lowsec_sys(sde: &SdeRegistry) -> u32 {
    sde.solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.security_status < 0.45
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_813)
}

fn find_highsec_sys(_sde: &SdeRegistry) -> u32 {
    30_000_142
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
    let dread_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19720);
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
    let titan_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 30)
        .map(|t| t.type_id)
        .unwrap_or(11567); // Avatar
    let dd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 588 && !matches!(t.type_id, 77398..=77401))
        .map(|t| t.type_id)
        .unwrap_or(24550);
    let dread_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19720);

    // Ensure dd_tid has reasonable attributes for testing
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        if let Some(m) = sde_mut.types.get_mut(&dd_tid) {
            m.group_id = 588;
            m.default_attributes.insert(713, 16274.0);
            m.default_attributes.insert(714, 100.0);
            m.default_attributes.insert(6, 1_000.0);
            m.default_attributes.insert(114, 50_000.0);
            m.default_attributes.insert(54, 250_000.0);
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

    let titan_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 30)
        .map(|t| t.type_id)
        .unwrap_or(11567);
    let dd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 588 && !matches!(t.type_id, 77398..=77401))
        .map(|t| t.type_id)
        .unwrap_or(24550);
    let dread_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19720);

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        if let Some(m) = sde_mut.types.get_mut(&dd_tid) {
            m.group_id = 588;
            m.default_attributes.insert(713, 16274.0);
            m.default_attributes.insert(714, 50_000.0);
            m.default_attributes.insert(6, 1_000.0);
            m.default_attributes.insert(114, 50_000.0);
            m.default_attributes.insert(54, 250_000.0);
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
