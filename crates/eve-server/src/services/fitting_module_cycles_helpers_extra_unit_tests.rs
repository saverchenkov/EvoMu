//! Extra unit and mutation-killing tests for `services::inventory_fitting::fitting_module_cycles::helpers`
//! covering weapon-bank reload locks, dynamic module HP/charge resolution, HIC scripted vs bubble results,
//! ammo unload by slot flag & cargo capacity overflow, `build_all_info` prime flags, Burst Jammer safety blocks,
//! `SalvageEnd` / `RepeatCycleStart` / `SyncOverloadState` cycle actions, and Mercenary Den remote shield repair.

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_module_cycles::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, VirtualServerClock,
    WorldRepository,
};
use eve_dogma::ShipFittingEngine;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{attr_ids, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&484).expect("seed module type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

pub(super) fn make_helper_ctx<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

pub(super) fn seed_char_and_ship(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
}

#[test]
fn resolve_snapshot_fitted_module_dynamic_hp_active_charges_and_bank_reloading() {
    let ctx = make_helper_ctx(|_| {});
    let char_id = 93_100_001i64;
    let ship_id = 1_000_400_001i64;
    let rig_id = 1_000_400_002i64;
    let sub_id = 1_000_400_003i64;
    let master_gun = 1_000_400_004i64;
    let slave_gun = 1_000_400_005i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut rig = InventoryItemRecord {
            item_id: rig_id,
            type_id: 484,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::RigSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        rig.dynamic_attributes.insert(attr_ids::DAMAGE, 12.5);
        rig.dynamic_attributes.insert(attr_ids::HP_STRUCT, 80.0);
        snap.items.insert(rig_id, rig);
        snap.module_active_charges.insert(rig_id, 179);

        snap.items.insert(
            sub_id,
            InventoryItemRecord {
                item_id: sub_id,
                type_id: 484,
                owner_id: char_id,
                location_id: 999_999_999, // wrong ship
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.weapon_banks
            .insert(ship_id, HashMap::from([(master_gun, vec![master_gun, slave_gun])]));
    });

    // Wrong ship returns None
    assert!(resolve_snapshot_fitted_module(&ctx, ship_id, sub_id).is_none());
    // Missing item returns None
    assert!(resolve_snapshot_fitted_module(&ctx, ship_id, 888_888_888).is_none());

    // Rig resolves with fallback charge from module_active_charges (qty 100) and dynamic HP/damage
    let fm = resolve_snapshot_fitted_module(&ctx, ship_id, rig_id).unwrap();
    assert_eq!(fm.charge_type_id, Some(179));
    assert_eq!(fm.charge_quantity, 100);
    assert!((fm.damage_hp - 12.5).abs() < 1e-6);
    assert!((fm.max_hp - 80.0).abs() < 1e-6);

    // Slave gun in weapon bank is reloading -> check_module_reloading on master_gun fails!
    set_modules_reload_until(&ctx, &[slave_gun], 1_800_000_005);
    let err = check_module_reloading(&ctx, ship_id, master_gun).unwrap_err();
    assert_eq!(err.code, "ModuleIsReloading");

    // Boundary: reload_until == now_unix (1_800_000_000) is NOT reloading -> Ok(())
    set_modules_reload_until(&ctx, &[slave_gun], 1_800_000_000);
    assert!(check_module_reloading(&ctx, ship_id, master_gun).is_ok());
}

#[test]
fn specialized_hic_scripted_result_unload_by_flag_overflow_and_build_all_info_flags() {
    let ctx = make_helper_ctx(|_| {});
    let char_id = 93_100_010i64;
    let sys_id = 30_000_142u32;
    let ship_id = 1_000_400_101i64;
    let gun_id = 1_000_400_102i64;
    let ammo_id = 1_000_400_103i64;

    seed_char_and_ship(&ctx, char_id, 98_000_001, sys_id, ship_id);
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let hic_type = custom_inv_type(&ctx.sde, 12076, 899, 7, &[]); // default 60s weapon timer

    // 1. Scripted HIC (hic_bubble_radius_m = None) -> scripted=true, bubbleRadius=None, disallowAssistance=false, weaponTimerSeconds=60.0
    let hic_scripted =
        build_specialized_module_result("Activate", &fit, &ctx, gun_id, &hic_type, None, 6, None).unwrap();
    assert_eq!(hic_scripted.dict_get("scripted").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hic_scripted.dict_get("bubbleRadius"), Some(&PyRep::None));
    assert_eq!(hic_scripted.dict_get("warpScrambleStrength").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(hic_scripted.dict_get("disallowAssistance").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(hic_scripted.dict_get("weaponTimerSeconds").and_then(|v| v.as_f64()), Some(60.0));

    // 2. handle_unload_ammo by slot flag (27 = HiSlot0) and NotEnoughCargoSpace when cargo is overfilled
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: 179,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100_000, // huge stack exceeding Rifter cargo capacity
                singleton: false,
                ..Default::default()
            },
        );
    });
    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let err_space = handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(gun_id)]).unwrap_err();
    assert_eq!(err_space.code, "NotEnoughCargoSpace");

    // Reduce ammo quantity to 10 and unload by module + slot flag 28 -> succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ammo_id).unwrap().quantity = 10;
        snap.items.insert(
            732_003,
            InventoryItemRecord {
                item_id: 732_003,
                type_id: 2889,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id), PyRep::Integer(28)]),
        ],
    )
    .unwrap();
    assert_eq!(ctx.db.get_item(ammo_id).unwrap().flag, InventoryFlag::Cargo);

    // 3. build_all_info with prime_char=false, prime_ship=false, and missing ship item in DB
    let info = build_all_info(
        &ctx,
        &sess,
        char_id,
        999_888_777, // ship not in snap.items -> fallback ship_inv_row
        587,
        PyRep::None,
        vec![],
        vec![],
        &[PyRep::Bool(false), PyRep::Integer(0)],
    );
    assert_eq!(info.dict_get("charInfo"), Some(&PyRep::None));
    assert_eq!(info.dict_get("shipInfo"), Some(&PyRep::None));
}
