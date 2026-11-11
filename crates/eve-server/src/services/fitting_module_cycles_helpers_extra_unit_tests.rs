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

fn make_seeded_helper_ctx<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    make_helper_ctx(mutate_sde).with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

async fn spawn_helper_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    owner_id: i64,
    pos: glam::DVec3,
    cloaked: bool,
    tethered: bool,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = eve_destiny::Ball {
        id,
        type_id,
        position: pos,
        velocity: glam::DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let mut cs = eve_destiny::CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    cs.is_cloaked = cloaked;
    cs.is_tethered = tethered;
    let meta = eve_destiny::SlimItemMeta {
        group_id,
        category_id,
        owner_id,
        char_id: (category_id == 6).then_some(owner_id),
        corp_id: 98_000_001,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ball");
}

#[test]
fn resolve_unload_eval_dogma_prime_and_online_effects_edge_paths() {
    const SYN_OFFLINE_MOD: u32 = 997_101;
    const SYN_ONLINE_MOD: u32 = 997_102;
    const SYN_HIC_CUSTOM_TIMER: u32 = 997_103;
    const SYN_COMMAND_BURST: u32 = 997_104;

    let ctx = make_seeded_helper_ctx(|sde| {
        let mut offline_t = custom_inv_type(sde, SYN_OFFLINE_MOD, 40, 7, &[(30, 10.0), (50, 5.0)]);
        offline_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_OFFLINE_MOD, offline_t);

        let mut online_t = custom_inv_type(sde, SYN_ONLINE_MOD, 40, 7, &[(30, 10.0), (50, 5.0)]);
        online_t.effect_ids = vec![13, 16, 4];
        online_t.default_effect_id = Some(4); // has no attr 73 -> falls back to 1_000.0ms
        sde.types.insert(SYN_ONLINE_MOD, online_t);

        sde.types.insert(
            SYN_HIC_CUSTOM_TIMER,
            custom_inv_type(sde, SYN_HIC_CUSTOM_TIMER, 899, 7, &[(2037, 45_000.0)]),
        );
        sde.types.insert(
            SYN_COMMAND_BURST,
            custom_inv_type(sde, SYN_COMMAND_BURST, 1770, 7, &[]),
        );
    });

    let char_id = 93_200_001i64;
    let other_char = 93_200_002i64;
    let sys_id = 30_000_142u32;
    let ship_id = 1_000_450_001i64;
    let mod_cargo = 1_000_450_002i64;
    let mod_offline = 1_000_450_003i64;
    let mod_online = 1_000_450_004i64;
    let non_charge_in_slot = 1_000_450_005i64;

    seed_char_and_ship(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_cargo,
            InventoryItemRecord {
                item_id: mod_cargo,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_offline,
            InventoryItemRecord {
                item_id: mod_offline,
                type_id: SYN_OFFLINE_MOD,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_offline, false);

        snap.items.insert(
            mod_online,
            InventoryItemRecord {
                item_id: mod_online,
                type_id: SYN_ONLINE_MOD,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_online, true);
        snap.module_active_states.insert(mod_online);

        // Non-category-8 item sitting in MedSlot1 must NOT be unloaded by handle_unload_ammo
        snap.items.insert(
            non_charge_in_slot,
            InventoryItemRecord {
                item_id: non_charge_in_slot,
                type_id: 34, // Tritanium (category 4)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot1,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Cargo flag is rejected by resolve_snapshot_fitted_module
    assert!(resolve_snapshot_fitted_module(&ctx, ship_id, mod_cargo).is_none());

    // Offline module resolves as Offline with default HP 40.0
    let fm_off = resolve_snapshot_fitted_module(&ctx, ship_id, mod_offline).unwrap();
    assert_eq!(fm_off.state, eve_dogma::ModuleState::Offline);
    assert!((fm_off.max_hp - 40.0).abs() < 1e-6);

    // evaluate_ship_dogma_attributes reports attr 2 = 0.0 for offline, 1.0 for online
    let (_, _, _, mod_entries) =
        evaluate_ship_dogma_attributes(&ctx, char_id, ship_id).expect("eval ship attrs");
    let find_online_attr = |mid: i64| -> Option<f64> {
        mod_entries.iter().find_map(|(k, v)| {
            if k.as_i64() == Some(mid) {
                if let PyRep::Dict(pairs) = v {
                    return pairs
                        .iter()
                        .find(|(ak, _)| ak.as_i64() == Some(2))
                        .and_then(|(_, av)| av.as_f64());
                }
            }
            None
        })
    };
    assert_eq!(find_online_attr(mod_offline), Some(0.0));
    assert_eq!(find_online_attr(mod_online), Some(1.0));

    // prime_ship_on_board valid & invalid ship fallback
    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        let uname = format!("helper_u{char_id}");
        snap.accounts.insert(
            uname.clone(),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: uname,
                password_token: String::new(),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock_with_computer_hash(
            char_id as u64,
            93_200_001,
            Some(char_id),
            None,
            true,
            100,
        )
        .expect("bring prime pilot online");
    let _ = ctx.drain_notifications_for_char(char_id);
    let ship_state = prime_ship_on_board(&ctx, &sess, char_id, ship_id);
    assert!(matches!(ship_state, PyRep::Tuple(t) if t.len() == 4));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs
        .iter()
        .any(|n| n.event == "ProcessGodmaPrimeLocation"));

    let fallback_state = prime_ship_on_board(&ctx, &sess, char_id, 999_111_222);
    assert!(matches!(fallback_state, PyRep::Tuple(t) if t.len() == 4));

    // online_module_active_effects true vs false
    assert_eq!(
        online_module_active_effects(mod_online, char_id, ship_id, 100, false),
        PyRep::Dict(Vec::new())
    );
    let PyRep::Dict(eff_on) =
        online_module_active_effects(mod_online, char_id, ship_id, 100, true)
    else {
        panic!("expected Dict");
    };
    assert_eq!(eff_on.len(), 1);
    assert_eq!(eff_on[0].0, PyRep::Integer(16));

    // build_specialized_module_result: unscripted HIC + custom timer, Burst with/without charge, other -> None
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let hic_t = ctx.sde.types[&SYN_HIC_CUSTOM_TIMER].clone();
    let hic_res = build_specialized_module_result(
        "Activate",
        &fit,
        &ctx,
        mod_online,
        &hic_t,
        Some(20_000.0),
        0,
        None,
    )
    .unwrap();
    assert_eq!(hic_res.dict_get("scripted").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(hic_res.dict_get("bubbleRadius").and_then(|v| v.as_f64()), Some(20_000.0));
    assert_eq!(hic_res.dict_get("disallowAssistance").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hic_res.dict_get("weaponTimerSeconds").and_then(|v| v.as_f64()), Some(45.0));

    let burst_t = ctx.sde.types[&SYN_COMMAND_BURST].clone();
    let burst_none = build_specialized_module_result(
        "Activate",
        &fit,
        &ctx,
        mod_online,
        &burst_t,
        None,
        0,
        None,
    )
    .unwrap();
    assert_eq!(burst_none.dict_get("chargeTypeID"), Some(&PyRep::None));
    let burst_some = build_specialized_module_result(
        "Activate",
        &fit,
        &ctx,
        mod_online,
        &burst_t,
        None,
        0,
        Some(42834),
    )
    .unwrap();
    assert_eq!(burst_some.dict_get("chargeTypeID"), Some(&PyRep::Integer(42834)));

    assert!(build_specialized_module_result(
        "Activate",
        &fit,
        &ctx,
        mod_online,
        &ctx.sde.types[&SYN_ONLINE_MOD],
        None,
        0,
        None,
    )
    .is_none());

    // handle_unload_ammo validation and edge branches
    assert_eq!(
        handle_unload_ammo(&ctx, &SessionState::new(1), &[]).unwrap_err().code,
        "Unauthorized"
    );
    let mut wrong_sess = sess.clone();
    wrong_sess.character_id = Some(other_char);
    assert_eq!(
        handle_unload_ammo(&ctx, &wrong_sess, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );
    // Empty/non-module/non-charge unload leaves non_charge_in_slot in MedSlot1
    handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::Tuple(vec![PyRep::Integer(mod_cargo), PyRep::Integer(mod_online), PyRep::Integer(999_999)]),
        ],
    )
    .unwrap();
    assert_eq!(
        ctx.db.get_item(non_charge_in_slot).unwrap().flag,
        InventoryFlag::MedSlot1
    );
    assert!(handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id)]).is_ok());
}

#[tokio::test]
async fn survey_scanner_ecm_burst_instant_effects_and_cycle_actions_cover_branches() {
    const SYN_SCANNER_FALLBACK: u32 = 997_201;
    const SYN_ECM_G201: u32 = 997_202;
    const SYN_BURST_G80: u32 = 997_203;
    const SYN_TRACTOR_G650: u32 = 997_204;
    const SYN_SALVAGER_ZERO_CHANCE: u32 = 997_205;

    let ctx = make_seeded_helper_ctx(|sde| {
        let mut scan_t = custom_inv_type(sde, SYN_SCANNER_FALLBACK, 49, 7, &[(54, 8_000.0)]);
        scan_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_SCANNER_FALLBACK, scan_t);

        let mut ecm_t = custom_inv_type(
            sde,
            SYN_ECM_G201,
            201,
            7,
            &[(238, 100.0), (239, 100.0), (240, 100.0), (241, 100.0), (54, 30_000.0), (2044, 20_000.0)],
        );
        ecm_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_ECM_G201, ecm_t);

        let mut burst_t = custom_inv_type(
            sde,
            SYN_BURST_G80,
            80,
            7,
            &[(238, 80.0), (239, 80.0), (240, 80.0), (241, 80.0), (197, 15_000.0)],
        );
        burst_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_BURST_G80, burst_t);

        let mut trac_t = custom_inv_type(sde, SYN_TRACTOR_G650, 650, 7, &[(54, 20_000.0)]);
        trac_t.effect_ids = vec![12, 16];
        sde.types.insert(SYN_TRACTOR_G650, trac_t);

        let mut salv_t = custom_inv_type(sde, SYN_SALVAGER_ZERO_CHANCE, 1122, 7, &[(1097, 0.0), (73, 5_000.0)]);
        salv_t.effect_ids = vec![12, 16];
        sde.types.insert(SYN_SALVAGER_ZERO_CHANCE, salv_t);
    });

    let char_id = 93_210_001i64;
    let legal_victim_char = 93_210_002i64;
    let neutral_victim_char = 93_210_003i64;
    let sys_id = 30_002_505u32; // lowsec
    let ship_id = 1_000_460_001i64;
    let legal_ship = 1_000_460_002i64;
    let neutral_ship = 1_000_460_003i64;
    let cloaked_ship = 1_000_460_004i64;
    let jetcan_id = 1_000_460_005i64;
    let wreck_id = 1_000_460_006i64;
    let mod_scan = 1_000_460_010i64;
    let mod_ecm = 1_000_460_011i64;
    let mod_burst = 1_000_460_012i64;
    let mod_salv = 1_000_460_013i64;

    seed_char_and_ship(&ctx, char_id, 98_000_001, sys_id, ship_id);
    seed_char_and_ship(&ctx, legal_victim_char, 98_000_002, sys_id, legal_ship);
    seed_char_and_ship(&ctx, neutral_victim_char, 98_000_003, sys_id, neutral_ship);
    ctx.db.with_mut_snapshot(|snap| {
        let uname = format!("ecm_victim_{legal_victim_char}");
        snap.accounts.insert(
            uname.clone(),
            eve_db::AccountRecord {
                user_id: legal_victim_char as u64,
                username: uname,
                password_token: String::new(),
                role: 1,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock_with_computer_hash(
            legal_victim_char as u64,
            93_210_002,
            Some(legal_victim_char),
            None,
            true,
            100,
        )
        .expect("bring legal victim online");

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, flag) in [
            (mod_scan, SYN_SCANNER_FALLBACK, InventoryFlag::MedSlot0),
            (mod_ecm, SYN_ECM_G201, InventoryFlag::MedSlot1),
            (mod_burst, SYN_BURST_G80, InventoryFlag::MedSlot2),
            (mod_salv, SYN_SALVAGER_ZERO_CHANCE, InventoryFlag::HiSlot0),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26557,
                owner_id: neutral_victim_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Asteroids: 1 zero quantity (skipped), 1 out of range (skipped), 1 unknown type in range (revealed)
        snap.asteroids.insert(
            78_000_001,
            eve_db::AsteroidRecord {
                asteroid_id: 78_000_001,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 100,
                position: (1_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
        snap.asteroids.insert(
            78_000_002,
            eve_db::AsteroidRecord {
                asteroid_id: 78_000_002,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 1230,
                quantity: 200,
                initial_quantity: 200,
                position: (50_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
        snap.asteroids.insert(
            78_000_003,
            eve_db::AsteroidRecord {
                asteroid_id: 78_000_003,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 999_888, // unknown type fallback name & vol
                quantity: 300,
                initial_quantity: 300,
                position: (2_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });

    spawn_helper_ball(&ctx, sys_id, ship_id, 587, 25, 6, char_id, glam::DVec3::new(1_000.0, 0.0, 0.0), false, false, 1_000.0).await;
    spawn_helper_ball(&ctx, sys_id, legal_ship, 587, 25, 6, legal_victim_char, glam::DVec3::new(1_500.0, 0.0, 0.0), false, false, 500.0).await;
    spawn_helper_ball(&ctx, sys_id, cloaked_ship, 587, 25, 6, 999_001, glam::DVec3::new(1_600.0, 0.0, 0.0), true, false, 500.0).await;
    spawn_helper_ball(&ctx, sys_id, jetcan_id, 23, 12, 2, neutral_victim_char, glam::DVec3::new(1_700.0, 0.0, 0.0), false, false, 0.0).await;
    // Also spawn an asteroid ball-free target ball in space only for ECM fallback path
    let space_only_ship = 1_000_460_099i64;
    spawn_helper_ball(&ctx, sys_id, space_only_ship, 587, 25, 6, 999_002, glam::DVec3::new(2_000.0, 0.0, 0.0), false, false, 100.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Survey scanner fallback to attr 54 = 8000m reveals only asteroid 78_000_003
    let scan_t = ctx.sde.types[&SYN_SCANNER_FALLBACK].clone();
    let scan_state = eve_dogma::DogmaItemState::from_sde(char_id, SYN_SCANNER_FALLBACK, &ctx.sde);
    let scan_res = handle_survey_scanner_activation(&ctx, &sys, sys_id, char_id, ship_id, mod_scan, &scan_t, &scan_state).await;
    assert_eq!(scan_res.dict_get("count").and_then(|v| v.as_i64()), Some(1));
    assert!((scan_res.dict_get("scanRange").and_then(|v| v.as_f64()).unwrap() - 8_000.0).abs() < 1e-6);

    // 2. Targeted ECM against ball not in snap.items uses ball fallback + seeded RNG -> jam lands
    let ecm_t = ctx.sde.types[&SYN_ECM_G201].clone();
    let ecm_state = eve_dogma::DogmaItemState::from_sde(char_id, SYN_ECM_G201, &ctx.sde);
    handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, mod_ecm, &ecm_t, &ecm_state, Some(space_only_ship), 0.0,
    )
    .await
    .expect("ecm vs ball target");
    assert!(sys.get_combat_state(space_only_ship).await.unwrap().unwrap().ecm_jammed_remaining_s > 0.0);

    // 3. Burst jammer: make legal_victim suspect, leave space_only_ship without char -> Burst succeeds even on Green!
    let now_u = ctx.now_unix_secs();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(legal_victim_char).suspect_flag_until_unix = now_u + 300;
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    let burst_t = ctx.sde.types[&SYN_BURST_G80].clone();
    let burst_state = eve_dogma::DogmaItemState::from_sde(char_id, SYN_BURST_G80, &ctx.sde);
    let _ = ctx.drain_notifications_for_char(legal_victim_char);
    handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, mod_burst, &burst_t, &burst_state, None, 15.0,
    )
    .await
    .expect("burst vs suspect + non-ship jetcan + cloaked ship succeeds on Green");
    assert!(ctx
        .drain_notifications_for_char(legal_victim_char)
        .iter()
        .any(|n| n.event == "OnTargetClear"));

    // Spawn neutral ship inside lowsec (0.3) -> Green safety blocks Burst Jammer with SafetyBlocked!
    spawn_helper_ball(&ctx, sys_id, neutral_ship, 587, 25, 6, neutral_victim_char, glam::DVec3::new(1_800.0, 0.0, 0.0), false, false, 500.0).await;
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, 0.3);
    });
    let err_green = handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, mod_burst, &burst_t, &burst_state, None, 15.0,
    )
    .await
    .unwrap_err();
    assert_eq!(err_green.code, "SafetyBlocked");

    // Yellow safety in lowsec (< 0.45) IS allowed for Burst Jammer!
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Yellow);
    });
    handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, mod_burst, &burst_t, &burst_state, None, 15.0,
    )
    .await
    .expect("yellow lowsec burst allowed");

    // 4. execute_pending_cycle_actions: SalvageEnd (keep_repeating & deactivate+refund), LanceBeamFired hit+miss,
    //    RepeatCycleStart error deactivation, SyncOverloadState (online + offline + char_id=None)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_salv);
        snap.module_active_charges.insert(mod_salv, 179);
    });
    {
        let mut st = sys.actor_state.write().unwrap();
        // Missing ship wreck + repeat != 0 -> keep_repeating=false -> deactivates + refunds 120 GJ cap
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::SalvageEnd {
            char_id,
            ship_id,
            module_id: mod_salv,
            module_type_id: SYN_SALVAGER_ZERO_CHANCE,
            wreck_id: 999_888_777, // missing wreck
            repeat: 1,
            effect_name: "defaultEffect".into(),
            next_cycle_cap_deducted: true,
            deducted_cap_gj: 120.0,
        });
        // Valid wreck + zero chance + repeat != 0 -> CycleRollFailed keeps repeating!
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::SalvageEnd {
            char_id,
            ship_id,
            module_id: mod_scan,
            module_type_id: SYN_SALVAGER_ZERO_CHANCE,
            wreck_id,
            repeat: 1,
            effect_name: "defaultEffect".into(),
            next_cycle_cap_deducted: false,
            deducted_cap_gj: 0.0,
        });
        // Repeat == 0 -> deactivates fitting module cleanly
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::SalvageEnd {
            char_id,
            ship_id,
            module_id: mod_burst,
            module_type_id: SYN_SALVAGER_ZERO_CHANCE,
            wreck_id,
            repeat: 0,
            effect_name: "defaultEffect".into(),
            next_cycle_cap_deducted: false,
            deducted_cap_gj: 0.0,
        });
        // LanceBeamFired with 0.0 miss + >0 hit
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::LanceBeamFired {
            attacker_char_id: char_id,
            attacker_ship_id: ship_id,
            attacker_ship_type_id: 587,
            module_id: mod_salv,
            module_type_id: 484,
            hits: vec![(legal_ship, 0.0), (neutral_ship, 150.0)],
        });
        // RepeatCycleStart: unknown type skipped + ECM with target_id=None fails -> deactivates repeating cycle
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::RepeatCycleStart {
            char_id,
            ship_id,
            module_id: mod_ecm,
            module_type_id: 999_999_111,
            target_id: None,
            duration_s: 10.0,
            is_overloaded: false,
            effect_name: "defaultEffect".into(),
        });
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::RepeatCycleStart {
            char_id,
            ship_id,
            module_id: mod_ecm,
            module_type_id: SYN_ECM_G201,
            target_id: None,
            duration_s: 10.0,
            is_overloaded: true,
            effect_name: "defaultEffect".into(),
        });
        // SyncOverloadState with char_id=None -> true
        st.pending_cycle_actions.push(crate::system_actor::PendingCycleAction::SyncOverloadState {
            char_id: None,
            ship_id,
            module_id: mod_scan,
            is_overloaded: true,
        });
    }
    let _ = sys.set_capacitor(ship_id, 500.0).await;
    execute_pending_cycle_actions(&ctx, sys_id).await;
    // Cap refunded by 120.0 -> 620.0
    let cap_after = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!((cap_after - 620.0).abs() < 1e-6);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_scan));
    let fit_after = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(fit_after.modules[&mod_scan].state, eve_dogma::ModuleState::Overloaded);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_salv));
        assert!(!snap.module_active_charges.contains_key(&mod_salv));
    });

    // 5. evaluate_module_cycle_aggression_or_assistance edge paths
    assert!(
        evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 55, None)
            .await
            .is_ok()
    );
    assert!(
        evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 46, Some(neutral_ship))
            .await
            .is_ok()
    );
    // Remote assistance group (41) in lowsec with legal target -> Ok(())
    assert!(
        evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 41, Some(legal_ship))
            .await
            .is_ok()
    );
    // Jetcan neutral aggression in lowsec does NOT trip sentry
    assert!(
        evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 55, Some(jetcan_id))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn try_handle_mercden_remote_shield_repair_covers_all_guards_and_repair() {
    const SYN_REP_G41: u32 = 997_301;
    let ctx = make_seeded_helper_ctx(|sde| {
        let mut rep_t = custom_inv_type(
            sde,
            SYN_REP_G41,
            41,
            7,
            &[(54, 10_000.0), (158, 2_000.0), (6, 50.0), (68, 1_500.0)],
        );
        rep_t.effect_ids = vec![13, 16];
        sde.types.insert(SYN_REP_G41, rep_t);
    });

    let char_id = 93_220_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 1_000_470_001i64;
    let rep_mod_id = 1_000_470_002i64;
    let den_id = 1_000_470_900i64;

    seed_char_and_ship(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rep_mod_id,
            InventoryItemRecord {
                item_id: rep_mod_id,
                type_id: SYN_REP_G41,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            den_id,
            eve_db::UpwellStructureRecord {
                structure_id: den_id,
                solar_system_id: sys_id,
                type_id: 85_230,
                owner_corp_id: 98_000_001,
                name: "Merc Den".into(),
                state: "online".into(),
                shield_hp: 500.0,
                armor_hp: 1_000.0,
                hull_hp: 1_000.0,
                position: [20_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            eve_db::durable_domain_types::PersistedMercenaryDen {
                den_id,
                owner_char_id: char_id,
                skyhook_id: 79_000_001,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                type_id: 85_230,
                enabled: false,
                cargo_extraction_enabled: true,
                development_level: 0,
                anarchy_level: 0,
                infomorphs: 0,
                simulated_at_unix: 1_800_000_000,
                next_activity_generation_unix: 1_800_000_000,
            },
        );
    });

    spawn_helper_ball(&ctx, sys_id, ship_id, 587, 25, 6, char_id, glam::DVec3::ZERO, false, false, 10.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let fm = fit.modules[&rep_mod_id].clone();
    let mod_t = ctx.sde.types[&SYN_REP_G41].clone();

    // 1. TargetNotLocked
    assert_eq!(
        try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id)
            .await
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );

    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id: den_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    // 2. Cloaked & Tethered guards
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_cloaked = true;
    assert_eq!(
        try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id)
            .await
            .unwrap_err()
            .code,
        "Cloaked"
    );
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_cloaked = false;
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_tethered = true;
    assert_eq!(
        try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id)
            .await
            .unwrap_err()
            .code,
        "Tethered"
    );
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship_id).unwrap().is_tethered = false;

    // 3. TargetOutOfRange (den at 20km > max_range 12km)
    assert_eq!(
        try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // Move den to 5km -> NotEnoughCapacitor (cur 10 < need 50)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&den_id).unwrap().position = [5_000.0, 0.0, 0.0];
    });
    assert_eq!(
        try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id)
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );

    // 4. Sufficient cap (120) -> deducts 50 (leaving 70) and repairs Merc Den shield:
    //    cycle 1 (500 + 1500 = 2000 / 5000 = 40% < 50%) -> denEnabled = false;
    //    cycle 2 (2000 + 1500 = 3500 / 5000 = 70% >= 50%) -> denEnabled = true.
    let _ = sys.set_capacitor(ship_id, 120.0).await;
    let res1 = try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Overload", den_id)
        .await
        .expect("repair 1 ok")
        .expect("some rep 1");
    assert_eq!(res1.dict_get("shieldPct").and_then(|v| v.as_i64()), Some(40));
    assert_eq!(res1.dict_get("denEnabled").and_then(|v| v.as_bool()), Some(false));
    assert!((sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 70.0).abs() < 1e-6);

    let res2 = try_handle_mercden_remote_shield_repair(&ctx, &sys, sys_id, char_id, ship_id, rep_mod_id, &fm, &mod_t, &fit, "Activate", den_id)
        .await
        .expect("repair 2 ok")
        .expect("some rep 2");
    assert_eq!(res2.dict_get("shieldPct").and_then(|v| v.as_i64()), Some(70));
    assert_eq!(res2.dict_get("denEnabled").and_then(|v| v.as_bool()), Some(true));
    assert!((sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 20.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helpers_reload_survey_dbuff_unload_dogma_and_weapon_damage_kill_mutants() {
    use crate::services::inventory_fitting::fitting_scanners_superweapons::{
        scanners_superweapons_state, ActiveDbuffEntry,
    };
    use crate::system_actor::PendingCycleAction;
    use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use eve_dogma::DogmaItemState;
    use eve_sde::{DbuffCollectionInfo, attr_ids};
    use glam::DVec3;

    let dbuff_197_id: u32 = 999_197;
    let scanner_tid: u32 = 999_444;
    let ctx = make_helper_ctx(|sde| {
        let mut sc = sde
            .types
            .values()
            .find(|t| t.group_id == 49)
            .expect("Survey Scanner in SDE")
            .clone();
        sc.type_id = scanner_tid;
        sc.default_attributes.insert(197, 10_000.0);
        sde.types.insert(scanner_tid, sc);
        sde.dbuff_collections.insert(
            dbuff_197_id,
            DbuffCollectionInfo {
                dbuff_id: dbuff_197_id,
                name: "TestSurveyRange".into(),
                operation: "PostPercent".into(),
                aggregate_mode: "Maximum".into(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: Vec::new(),
                location_modifiers: vec![197],
                location_group_modifiers: Vec::new(),
                location_required_skill_modifiers: Vec::new(),
                target_attribute_ids: vec![197],
            },
        );
    });

    let sys_id: u32 = 30_000_142;
    let station_id: i64 = 60_003_760;
    let char_id: i64 = 90_993_001;
    let victim_cid: i64 = 90_993_002;
    let ship_id: i64 = 1_993_001;
    let t3d_id: i64 = 1_993_002;
    let victim_ship_id: i64 = 1_993_003;
    let mod_id: i64 = 1_993_010;
    let charge_id: i64 = 1_993_011;
    let cargo_ammo_id: i64 = 1_993_012;
    let scanner_id: i64 = 1_993_013;

    ctx.db.provision_gm_account("cyc_m1", "pw", char_id, "CycM1");
    ctx.db.provision_gm_account("cyc_m2", "pw", victim_cid, "CycM2");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Confessor (34317) for evaluate_ship_dogma_attributes (stance + dynamic_attributes 3/18/265/266 + unchanged)
        snap.items.insert(
            t3d_id,
            InventoryItemRecord {
                item_id: t3d_id,
                type_id: 34_317,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([
                    (attr_ids::DAMAGE, 42.5),      // 3
                    (18_u16, 123.0),               // 18 capacitorCharge
                    (265_u16, 77.0),               // 265
                    (attr_ids::ARMOR_DAMAGE, 88.0),// 266
                    (9_u16, 0.0),                  // unmodified if zero? overwritten or equal
                ]),
                ..Default::default()
            },
        );
        // Fitted turret + charge in HiSlot0 on ship_id + existing stackable ammo in Cargo with version = 7
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 484, // Small Autocannon / turret
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 178, // Fusion S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_ammo_id,
            InventoryItemRecord {
                item_id: cargo_ammo_id,
                type_id: 178,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 7,
                ..Default::default()
            },
        );
        snap.items.insert(
            scanner_id,
            InventoryItemRecord {
                item_id: scanner_id,
                type_id: scanner_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            77_993_001,
            AsteroidRecord {
                asteroid_id: 77_993_001,
                solar_system_id: sys_id,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 200,
                initial_quantity: 200,
                radius: 50.0,
                position: (14_000.0, 0.0, 0.0), // outside base 10_000m, inside x1.5 = 15_000m
            },
        );
    });

    // 1. set_modules_reload_until & check_module_reloading exact < boundary
    let now = ctx.now_unix_secs();
    set_modules_reload_until(&ctx, &[mod_id], now + 10);
    assert!(check_module_reloading(&ctx, ship_id, mod_id).is_err());
    set_modules_reload_until(&ctx, &[mod_id], now);
    assert!(check_module_reloading(&ctx, ship_id, mod_id).is_ok());

    // 2. handle_survey_scanner_activation with active 197 dbuff (+50% range -> reveals 14km asteroid)
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let scanner_type = ctx.sde.types.get(&scanner_tid).unwrap().clone();
    let scanner_state = DogmaItemState::from_sde(char_id, scanner_tid, &ctx.sde);
    let scan_before = handle_survey_scanner_activation(
        &ctx, &sys, sys_id, char_id, ship_id, scanner_id, &scanner_type, &scanner_state,
    )
    .await;
    assert_eq!(scan_before.dict_get("count").and_then(|v| v.as_i64()), Some(0));

    {
        let mut st = scanners_superweapons_state().write().unwrap();
        st.ship_dbuffs.insert(
            (ctx.instance_id, ship_id),
            vec![ActiveDbuffEntry {
                dbuff_id: dbuff_197_id,
                value: 50.0,
                expires_at_unix: now + 600,
                source_module_id: 99_001,
            }],
        );
    }
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id));
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix());
    let scan_after = handle_survey_scanner_activation(
        &ctx, &sys, sys_id, char_id, ship_id, scanner_id, &scanner_type, &scanner_state,
    )
    .await;
    assert_eq!(scan_after.dict_get("count").and_then(|v| v.as_i64()), Some(1));
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnAsteroidRevealed")
    );

    // 3. handle_unload_ammo moves charge_id directly to Cargo and increments ci.version += 1
    let mut sess = SessionState::new(930);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let unload_res = handle_unload_ammo(
        &ctx,
        &sess,
        &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)],
    )
    .expect("handle_unload_ammo ok");
    assert_eq!(unload_res, PyRep::None);
    ctx.db.with_snapshot(|snap| {
        let unloaded = &snap.items[&charge_id];
        assert_eq!(unloaded.location_id, ship_id);
        assert_eq!(unloaded.flag, InventoryFlag::Cargo);
        assert_eq!(unloaded.quantity, 20);
        assert_eq!(unloaded.version, 2);
    });

    // 4. evaluate_ship_dogma_attributes on T3D Confessor: exact ship_type_id, Some stance, and 3/18/265/266 attrs
    let (ev_tid, ev_stance, ev_entries, ev_mods) =
        evaluate_ship_dogma_attributes(&ctx, char_id, t3d_id).expect("evaluate_ship_dogma_attributes ok");
    assert_eq!(ev_tid, 34_317);
    assert!(matches!(ev_stance, PyRep::Integer(s) if s > 1), "Confessor must return non-zero stance type ID");
    assert!(ev_mods.is_empty());
    let find_attr = |aid: u16| -> Option<f64> {
        ev_entries
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(i64::from(aid)))
            .and_then(|(_, v)| v.as_f64())
    };
    assert_eq!(find_attr(attr_ids::DAMAGE), Some(42.5));
    assert_eq!(find_attr(18), Some(123.0));
    assert_eq!(find_attr(265), Some(77.0));
    assert_eq!(find_attr(attr_ids::ARMOR_DAMAGE), Some(88.0));

    // 5. execute_pending_cycle_actions: empty early return + CycleWeaponDamage notification arms
    execute_pending_cycle_actions(&ctx, sys_id).await; // empty early return
    {
        let mut st = sys.actor_state.write().unwrap();
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(char_id),
            attacker_ship_id: ship_id,
            attacker_ship_type_id: 587,
            target_id: victim_ship_id,
            target_char_id: Some(victim_cid),
            module_type_id: 484,
            applied_dmg: 50.0,
            notified: true, // already notified -> skip
        });
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(char_id),
            attacker_ship_id: ship_id,
            attacker_ship_type_id: 587,
            target_id: victim_ship_id,
            target_char_id: Some(victim_cid),
            module_type_id: 484,
            applied_dmg: 0.0, // <= 0 -> skip
            notified: false,
        });
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(char_id),
            attacker_ship_id: ship_id,
            attacker_ship_type_id: 587,
            target_id: ship_id,
            target_char_id: Some(char_id), // self hit -> AttackHit only
            module_type_id: 484,
            applied_dmg: 25.0,
            notified: false,
        });
        st.pending_cycle_actions.push(PendingCycleAction::CycleWeaponDamage {
            attacker_char_id: Some(char_id),
            attacker_ship_id: ship_id,
            attacker_ship_type_id: 587,
            target_id: victim_ship_id,
            target_char_id: Some(victim_cid), // hits both
            module_type_id: 484,
            applied_dmg: 60.0,
            notified: false,
        });
    }
    let victim_uid = ctx.db.with_snapshot(|s| s.characters[&victim_cid].user_id);
    let _ = ctx.db.acquire_online_lock(victim_uid, 2, Some(victim_cid), false, ctx.now_unix());
    execute_pending_cycle_actions(&ctx, sys_id).await;
    let c1_notifs = ctx.drain_notifications_for_char(char_id);
    let c2_notifs = ctx.drain_notifications_for_char(victim_cid);
    assert!(c1_notifs.iter().any(|n| n.event == "OnDamageMessages"));
    assert!(c2_notifs.iter().any(|n| n.event == "OnDamageMessages"));
}


