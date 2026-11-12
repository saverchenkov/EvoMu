#![forbid(unsafe_code)]

use super::*;
use crate::system_actor::PendingCycleAction;
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_dogma::{DogmaItemState, ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(sde: &SdeRegistry, type_id: u32, group_id: u32, category_id: u32, attrs: &[(u16, f64)]) -> InvType {
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

fn test_ctx_with_custom_types<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn test_ctx() -> ServerContext {
    test_ctx_with_custom_types(|_| {})
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = 2;
        ch.fleet_id = None;
        snap.characters.entry(char_id).or_insert(ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn insert_item(
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
                ..Default::default()
            },
        );
    });
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ball_id: i64, type_id: u32, owner_id: i64, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, CombatState::default(), meta)
        .await
        .expect("spawn ball");
}

#[test]
fn resolve_snapshot_fitted_module_and_reload_windows() {
    let ctx = test_ctx();
    let char_id = 93_000_001;
    let ship_id = 1_000_300_001;
    let gun_id = 1_000_300_002;
    let charge_id = 1_000_300_003;
    let cargo_item_id = 1_000_300_004;

    insert_item(&ctx, gun_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, charge_id, 179, char_id, ship_id, InventoryFlag::HiSlot0, 40, false);
    insert_item(&ctx, cargo_item_id, 179, char_id, ship_id, InventoryFlag::Cargo, 200, false);

    // Non-fitted cargo item returns None
    assert!(resolve_snapshot_fitted_module(&ctx, ship_id, cargo_item_id).is_none());

    // Fitted module resolves with online state and loaded charge
    let fm = resolve_snapshot_fitted_module(&ctx, ship_id, gun_id).expect("fitted module");
    assert_eq!(fm.item_id, gun_id);
    assert_eq!(fm.state, ModuleState::Online);
    assert_eq!(fm.charge_type_id, Some(179));
    assert_eq!(fm.charge_quantity, 40);

    // Offline module resolves as ModuleState::Offline
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(gun_id, false);
    });
    let fm_off = resolve_snapshot_fitted_module(&ctx, ship_id, gun_id).unwrap();
    assert_eq!(fm_off.state, ModuleState::Offline);

    // Reload lock check blocks before reload_until_unix and clears after expiration
    set_modules_reload_until(&ctx, &[gun_id], 1_800_000_010);
    let err = check_module_reloading(&ctx, ship_id, gun_id).unwrap_err();
    assert_eq!(err.code, "ModuleIsReloading");

    set_modules_reload_until(&ctx, &[gun_id], 1_799_999_999);
    assert!(check_module_reloading(&ctx, ship_id, gun_id).is_ok());
}

#[test]
fn build_specialized_module_result_and_build_all_info() {
    let ctx = test_ctx();
    let char_id = 93_000_010;
    let sys_id = 30_000_142;
    let ship_id = 1_000_300_101;
    let mod_id = 1_000_300_102;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);

    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let mut mod_type = custom_inv_type(&ctx.sde, 12076, 899, 7, &[(2037, 60_000.0)]);

    // Overload branch
    let ov_rep = build_specialized_module_result("Overload", &fit, &ctx, mod_id, &mod_type, None, 0, None)
        .expect("overload result");
    assert_eq!(ov_rep.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));

    // HIC bubble branch
    let hic_bubble = build_specialized_module_result("Activate", &fit, &ctx, mod_id, &mod_type, Some(20_000.0), 0, None)
        .expect("hic bubble result");
    assert_eq!(hic_bubble.dict_get("disallowAssistance").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hic_bubble.dict_get("scripted").and_then(|v| v.as_bool()), Some(false));

    // Command burst branch
    mod_type.group_id = 1770;
    let burst_rep = build_specialized_module_result("Activate", &fit, &ctx, mod_id, &mod_type, None, 0, Some(42832))
        .expect("burst result");
    assert_eq!(burst_rep.dict_get("chargeTypeID").and_then(|v| v.as_i64()), Some(42832));

    // Non-specialized module returns None
    mod_type.group_id = 55;
    assert!(build_specialized_module_result("Activate", &fit, &ctx, mod_id, &mod_type, None, 0, None).is_none());

    // build_all_info emits ProcessGodmaPrimeLocation and populates charInfo & shipInfo
    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let info = build_all_info(
        &ctx,
        &sess,
        char_id,
        ship_id,
        587,
        PyRep::None,
        vec![(PyRep::Integer(37), PyRep::Real(320.0))],
        vec![(PyRep::Integer(mod_id), PyRep::Dict(Vec::new()))],
        &[],
    );
    assert_eq!(info.dict_get("shipID").and_then(|v| v.as_i64()), Some(ship_id));
    assert_ne!(info.dict_get("charInfo").cloned(), Some(PyRep::None));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "ProcessGodmaPrimeLocation"));
}

#[test]
fn unload_ammo_weapon_bank_and_cargo_limits() {
    let ctx = test_ctx();
    let char_id = 93_000_020;
    let other_char_id = 93_000_029;
    let sys_id = 30_000_142;
    let ship_id = 1_000_300_201;
    let gun1_id = 1_000_300_202;
    let gun2_id = 1_000_300_203;
    let charge1_id = 1_000_300_204;
    let charge2_id = 1_000_300_205;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, gun1_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, gun2_id, 484, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, charge1_id, 179, char_id, ship_id, InventoryFlag::HiSlot0, 20, false);
    insert_item(&ctx, charge2_id, 179, char_id, ship_id, InventoryFlag::HiSlot1, 20, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.weapon_banks.insert(ship_id, HashMap::from([(gun1_id, vec![gun1_id, gun2_id])]));
    });

    // Foreign character gets ShipOwnershipViolation
    let foreign_sess = SessionState {
        character_id: Some(other_char_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let err_owner = handle_unload_ammo(&ctx, &foreign_sess, &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id)])
        .unwrap_err();
    assert_eq!(err_owner.code, "Unauthorized");

    // Owner unloads linked weapon bank into Cargo
    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id)])
        .expect("unload weapon bank");
    ctx.db.with_snapshot(|snap| {
        let total_cargo_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 179)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(total_cargo_qty, 40);
    });
}

#[tokio::test]
async fn survey_scanner_ecm_burst_jammer_and_pending_cycle_actions() {
    let ctx = test_ctx_with_custom_types(|sde| {
        sde.types.insert(
            999_101,
            custom_inv_type(sde, 999_101, 80, 7, &[(238, 25.0), (239, 25.0), (240, 25.0), (241, 25.0), (197, 12_000.0)]),
        );
        sde.types.insert(
            999_102,
            custom_inv_type(sde, 999_102, 49, 7, &[(197, 25_000.0)]),
        );
        sde.types.insert(
            999_103,
            custom_inv_type(sde, 999_103, 201, 7, &[(238, 12.0), (54, 40_000.0), (2044, 20_000.0)]),
        );
    });

    let char_id = 93_000_030;
    let victim_char_id = 93_000_031;
    let sys_id = 30_002_505; // lowsec
    let ship_id = 1_000_300_301;
    let victim_ship_id = 1_000_300_302;
    let scanner_id = 1_000_300_303;
    let ecm_id = 1_000_300_304;
    let burst_id = 1_000_300_305;

    ensure_char(&ctx, char_id, 98_000_010, sys_id, ship_id);
    ensure_char(&ctx, victim_char_id, 98_000_011, sys_id, victim_ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, scanner_id, 999_102, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, ecm_id, 999_103, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    insert_item(&ctx, burst_id, 999_101, char_id, ship_id, InventoryFlag::MedSlot2, 1, true);

    spawn_ship_ball(&ctx, sys_id, ship_id, 587, char_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn_ship_ball(&ctx, sys_id, victim_ship_id, 587, victim_char_id, DVec3::new(11_000.0, 0.0, 0.0)).await;

    // Seed an asteroid near the scanner ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            77_000_001,
            AsteroidRecord {
                asteroid_id: 77_000_001,
                solar_system_id: sys_id,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 500,
                initial_quantity: 500,
                radius: 1_000.0,
                position: (12_000.0, 0.0, 0.0),
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let scanner_type = ctx.sde.types.get(&999_102).unwrap().clone();
    let scanner_state = DogmaItemState::from_sde(char_id, 999_102, &ctx.sde);

    // 1. Survey scanner reveals asteroid within 25km
    let scan_res = handle_survey_scanner_activation(
        &ctx,
        &sys,
        sys_id,
        char_id,
        ship_id,
        scanner_id,
        &scanner_type,
        &scanner_state,
    )
    .await;
    assert_eq!(scan_res.dict_get("count").and_then(|v| v.as_i64()), Some(1));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnAsteroidRevealed"));

    // 2. Targeted ECM requires locked target
    let ecm_type = ctx.sde.types.get(&999_103).unwrap().clone();
    let ecm_state = DogmaItemState::from_sde(char_id, 999_103, &ctx.sde);
    let ecm_err = handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, ecm_id, &ecm_type, &ecm_state, None, 20.0,
    )
    .await
    .unwrap_err();
    assert_eq!(ecm_err.code, "TargetRequired");

    // 3. Burst Jammer hits victim ship 1km away and applies combat aggression
    let burst_type = ctx.sde.types.get(&999_101).unwrap().clone();
    let burst_state = DogmaItemState::from_sde(char_id, 999_101, &ctx.sde);
    handle_ecm_or_burst_jammer_activation(
        &ctx, &sys, sys_id, char_id, ship_id, burst_id, &burst_type, &burst_state, None, 20.0,
    )
    .await
    .expect("burst jammer activation");

    // 4. execute_pending_cycle_actions: LanceBeamFired + InvalidateShipFitting
    {
        let mut st = sys.actor_state.write().unwrap();
        st.pending_cycle_actions.push(PendingCycleAction::LanceBeamFired {
            attacker_char_id: char_id,
            attacker_ship_id: ship_id,
            attacker_ship_type_id: 587,
            module_id: scanner_id,
            module_type_id: 484,
            hits: vec![(victim_ship_id, 250.0)],
        });
        st.pending_cycle_actions.push(PendingCycleAction::InvalidateShipFitting {
            char_id: Some(char_id),
            ship_id,
        });
    }
    execute_pending_cycle_actions(&ctx, sys_id).await;
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnDamageMessages"));
    assert!(ctx
        .drain_notifications_for_char(victim_char_id)
        .iter()
        .any(|n| n.event == "OnDamageMessage"));

    // 5. evaluate_module_cycle_aggression_or_assistance on weapon group 55
    evaluate_module_cycle_aggression_or_assistance(&ctx, &sys, sys_id, char_id, ship_id, 55, Some(victim_ship_id))
        .await
        .expect("pvp aggression recorded");
}

#[tokio::test]
async fn build_all_info_and_item_get_info_populate_effect_online_16_for_online_modules() {
    let ctx = test_ctx();
    let char_id = 93_000_050;
    let sys_id = 30_000_142;
    let ship_id = 1_000_300_501;
    let online_mod_id = 1_000_300_502;
    let offline_mod_id = 1_000_300_503;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, online_mod_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, offline_mod_id, 484, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(online_mod_id, true);
        snap.module_online_states.insert(offline_mod_id, false);
    });

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    let all_info = build_all_info(
        &ctx,
        &sess,
        char_id,
        ship_id,
        587,
        PyRep::None,
        vec![],
        vec![
            (PyRep::Integer(online_mod_id), PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))])),
            (PyRep::Integer(offline_mod_id), PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(0.0))])),
        ],
        &[],
    );
    let ship_info = all_info.dict_get("shipInfo").expect("shipInfo");
    let PyRep::Dict(ship_entries) = ship_info else { panic!("expected Dict shipInfo") };
    let online_entry = ship_entries.iter().find(|(k, _)| k.as_i64() == Some(online_mod_id)).unwrap().1.clone();
    let offline_entry = ship_entries.iter().find(|(k, _)| k.as_i64() == Some(offline_mod_id)).unwrap().1.clone();
    let PyRep::Dict(on_effs) = online_entry.dict_get("activeEffects").unwrap() else { panic!() };
    assert_eq!(on_effs.len(), 1);
    assert_eq!(on_effs[0].0, PyRep::Integer(16));
    let PyRep::List(eff_vals) = &on_effs[0].1 else { panic!() };
    assert_eq!(eff_vals.len(), 10);
    assert_eq!(eff_vals[0], PyRep::Integer(online_mod_id));
    assert_eq!(eff_vals[1], PyRep::Integer(char_id));
    assert_eq!(eff_vals[2], PyRep::Integer(ship_id));
    assert_eq!(eff_vals[6], PyRep::Integer(16));
    let PyRep::Dict(off_effs) = offline_entry.dict_get("activeEffects").unwrap() else { panic!() };
    assert!(off_effs.is_empty());

    let item_on = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(online_mod_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Dict(item_on_attrs) = item_on.dict_get("attributes").unwrap() else { panic!() };
    assert!(item_on_attrs.iter().any(|(k, v)| k.as_i64() == Some(2) && v.as_f64() == Some(1.0)));
    let PyRep::Dict(item_on_effs) = item_on.dict_get("activeEffects").unwrap() else { panic!() };
    assert!(item_on_effs.iter().any(|(k, _)| k.as_i64() == Some(16)));

    let item_off = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(offline_mod_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Dict(item_off_attrs) = item_off.dict_get("attributes").unwrap() else { panic!() };
    assert!(item_off_attrs.iter().any(|(k, v)| k.as_i64() == Some(2) && v.as_f64() == Some(0.0)));
    let PyRep::Dict(item_off_effs) = item_off.dict_get("activeEffects").unwrap() else { panic!() };
    assert!(item_off_effs.is_empty());
}

#[test]
fn unload_ammo_docked_station_hangar_destination_and_invalid_dest_946() {
    let ctx = test_ctx();
    let char_id = 93_000_060;
    let other_char_id = 93_000_061;
    let sys_id = 30_000_142;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_300_601;
    let gun_id = 1_000_300_602;
    let charge_id = 1_000_300_603;
    let filler_cargo_id = 1_000_300_604;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
    });
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, gun_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, charge_id, 179, char_id, ship_id, InventoryFlag::HiSlot0, 40, false);
    // Fill ship cargo beyond capacity so unloading to Cargo would fail, proving station hangar bypasses ship cargo cap
    insert_item(&ctx, filler_cargo_id, 179, char_id, ship_id, InventoryFlag::Cargo, 500_000, false);

    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Foreign destination owner rejected
    let err_owner = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(station_id),
                PyRep::Integer(other_char_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ]),
        ],
    )
    .unwrap_err();
    assert_eq!(err_owner.code, "ItemOwnershipViolation");

    // 2. Invalid destination flag rejected
    let err_flag = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(27),
            ]),
        ],
    )
    .unwrap_err();
    assert_eq!(err_flag.code, "InvalidInventoryFlag");

    // 3. Unreachable station rejected
    let err_dest = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(60_009_999),
                PyRep::Integer(char_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ]),
        ],
    )
    .unwrap_err();
    assert_eq!(err_dest.code, "InvalidDestination");

    // 3b. Active module rejected with ModuleIsActive, Hangar(4) on ship rejected, 2-tuple (ship_id, Cargo) supported
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(gun_id);
    });
    let err_active = handle_unload_ammo(
        &ctx,
        &sess,
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(gun_id)])],
    )
    .unwrap_err();
    assert_eq!(err_active.code, "ModuleIsActive");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&gun_id);
    });

    let err_ship_hangar = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ]),
        ],
    )
    .unwrap_err();
    assert_eq!(err_ship_hangar.code, "InvalidInventoryFlag");

    // 4. Unloading to ship cargo when cargo is full or ship capacity is unknown/zero fails closed with NotEnoughCargoSpace
    let err_cargo = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ]),
        ],
    )
    .unwrap_err();
    assert_eq!(err_cargo.code, "NotEnoughCargoSpace");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 999_999_999; // unknown ship type -> eval_cargo_cap is None -> fail closed
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let err_unknown_cap = handle_unload_ammo(
        &ctx,
        &sess,
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(gun_id)])],
    )
    .unwrap_err();
    assert_eq!(err_unknown_cap.code, "NotEnoughCargoSpace");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 587;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 4b. Unloading into a packaged container (!singleton) or another owned ship in station hangar is rejected
    let packaged_cont_id = 1_000_300_605;
    let other_ship_id = 1_000_300_606;
    insert_item(&ctx, packaged_cont_id, 3297, char_id, station_id, InventoryFlag::Hangar, 1, false);
    insert_item(&ctx, other_ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let err_pkg_cont = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Integer(packaged_cont_id),
        ],
    )
    .unwrap_err();
    assert_eq!(err_pkg_cont.code, "ContainerNotAssembled");

    let err_other_ship = handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Integer(other_ship_id),
        ],
    )
    .unwrap_err();
    assert_eq!(err_other_ship.code, "InvalidDestination");

    // 4c. Malformed args[2] (<= 0 or non-integer), CorpSAG flag (116), or Hangar flag (4) on container fail closed
    assert_eq!(
        handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(gun_id)]), PyRep::Integer(0)]).unwrap_err().code,
        "InvalidDestination"
    );
    assert_eq!(
        handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(gun_id)]), PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(116)])]).unwrap_err().code,
        "InvalidInventoryFlag"
    );
    let assembled_cont_id = 1_000_300_607;
    insert_item(&ctx, assembled_cont_id, 3297, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(gun_id)]), PyRep::Tuple(vec![PyRep::Integer(assembled_cont_id), PyRep::Integer(InventoryFlag::Hangar as i64)])]).unwrap_err().code,
        "InvalidInventoryFlag"
    );

    // 5. Unloading to docked station hangar (station_id, char_id, 4) succeeds and moves charge to station hangar
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_unload_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun_id)]),
            PyRep::Tuple(vec![
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ]),
        ],
    )
    .expect("unload to docked station hangar");

    let moved_charge = ctx.db.get_item(charge_id).expect("charge still exists");
    assert_eq!(moved_charge.location_id, station_id);
    assert_eq!(moved_charge.flag, InventoryFlag::Hangar);
    assert_eq!(moved_charge.quantity, 40);
    assert!(!moved_charge.singleton);

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnItemChange"
            && format!("{:?}", n.args).contains(&charge_id.to_string())
            && format!("{:?}", n.args).contains(&station_id.to_string())),
        "expected OnItemChange for charge moving from ship to station hangar, got {notifs:?}"
    );
}

#[test]
fn get_all_info_and_prime_ship_include_loaded_charges_941_948() {
    let ctx = test_ctx();
    let char_id = 93_000_070;
    let sys_id = 30_000_142;
    let ship_id = 1_000_300_701;
    let launcher_id = 1_000_300_702;
    let charge_id = 1_000_300_703;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, launcher_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, charge_id, 179, char_id, ship_id, InventoryFlag::HiSlot0, 40, false);

    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    let (ship_tid, active_stance, entries, module_entries) =
        evaluate_ship_dogma_attributes(&ctx, char_id, ship_id).expect("eval ship dogma");
    assert!(
        module_entries.iter().any(|(k, _)| k.as_i64() == Some(charge_id)),
        "module_entries must include loaded charge {charge_id}: {module_entries:?}"
    );

    let _ = ctx.drain_notifications_for_char(char_id);
    let all_info = build_all_info(
        &ctx,
        &sess,
        char_id,
        ship_id,
        ship_tid,
        active_stance,
        entries,
        module_entries,
        &[],
    );
    let PyRep::Dict(ship_entries) = all_info.dict_get("shipInfo").expect("shipInfo") else {
        panic!("expected shipInfo Dict");
    };
    let charge_entry = ship_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(charge_id))
        .map(|(_, v)| v)
        .expect("shipInfo must contain loaded charge entry (#941/#948)");

    // Verify invItem preserves stacksize=40 (singleton=false -> quantity cell is 40, not -1)
    let inv_item_str = format!("{:?}", charge_entry.dict_get("invItem"));
    assert!(
        inv_item_str.contains("Integer(40)"),
        "loaded charge invItem DBRow must encode stacksize 40 (singleton=false), got {inv_item_str}"
    );

    // Verify attributes dict includes attributeQuantity (80) = 40.0
    let PyRep::Dict(c_attrs) = charge_entry.dict_get("attributes").expect("charge attributes") else {
        panic!("expected charge attributes Dict");
    };
    assert!(
        c_attrs.iter().any(|(k, v)| k.as_i64() == Some(80) && v.as_f64() == Some(40.0)),
        "charge attributes must include QUANTITY (80) = 40.0: {c_attrs:?}"
    );

    // Verify ProcessGodmaPrimeLocation notification on prime_ship_on_board (undock #948) also includes charge_id
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = prime_ship_on_board(&ctx, &sess, char_id, ship_id);
    let prime_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        prime_notifs.iter().any(|n| n.event == "ProcessGodmaPrimeLocation"
            && format!("{:?}", n.args).contains(&charge_id.to_string())),
        "prime_ship_on_board must include loaded charge in ProcessGodmaPrimeLocation (#948)"
    );
}
