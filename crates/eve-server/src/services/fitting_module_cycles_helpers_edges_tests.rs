//! Edge-case behavioral tests for `fitting_module_cycles_helpers`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use crate::system_actor::PendingCycleAction;
use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, TargetLock};
use eve_dogma::ShipFittingEngine;
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

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&484).expect("seed module type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_sde<F: FnOnce(&mut SdeRegistry)>(mutate: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate(&mut sde);
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, sys_id: u32, ship_id: i64, safety: SafetyLevel) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = 98_000_001;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.balance = eve_db::IskCents::from_isk(1_000_000);
        snap.characters.insert(char_id, ch);
    });
    with_mut_combat_runtime(ctx, |rt| {
        rt.safety_levels.insert(char_id, safety);
    });
    let _ = ctx.db.acquire_online_lock(
        char_id as u64,
        char_id as u64,
        Some(char_id),
        true,
        ctx.now_unix_secs(),
    );
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

async fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    is_cloaked: bool,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 1_500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.is_cloaked = is_cloaked;
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        security_status: 5.0,
        is_cloaked,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta).await.unwrap();
}

#[test]
fn snapshot_module_charge_fallback_weapon_bank_reload_and_specialized_script_edges() {
    let ctx = test_ctx_with_sde(|sde| {
        let hic = custom_inv_type(sde, 999_201, 899, 7, 5.0, &[(2037, 20_000.0)]);
        let burst = custom_inv_type(sde, 999_202, 1770, 7, 5.0, &[]);
        sde.types.insert(999_201, hic);
        sde.types.insert(999_202, burst);
    });
    let char_id = 93_100_001i64;
    let ship_id = 1_000_400_001i64;
    let mod1_id = 1_000_400_002i64;
    let mod2_id = 1_000_400_003i64;

    insert_item(&ctx, ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, mod1_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, mod2_id, 484, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_charges.insert(mod1_id, 179);
        snap.items
            .get_mut(&mod1_id)
            .unwrap()
            .dynamic_attributes
            .insert(9, 42.0);
        snap.weapon_banks.insert(
            ship_id,
            HashMap::from([
                (mod1_id, vec![mod1_id, mod2_id]),
                (mod2_id, vec![mod1_id, mod2_id]),
            ]),
        );
    });

    // Wrong ship returns None
    assert!(resolve_snapshot_fitted_module(&ctx, ship_id + 1, mod1_id).is_none());

    let fm = resolve_snapshot_fitted_module(&ctx, ship_id, mod1_id).unwrap();
    assert_eq!(fm.charge_type_id, Some(179));
    assert_eq!(fm.charge_quantity, 100);
    assert_eq!(fm.dynamic_attribute_overrides.get(&9).copied(), Some(42.0));

    // Reloading mod1 also blocks grouped sibling mod2
    set_modules_reload_until(&ctx, &[mod1_id], 1_800_000_020);
    let bank_err = check_module_reloading(&ctx, ship_id, mod2_id).unwrap_err();
    assert_eq!(bank_err.code, "ModuleIsReloading");

    // Specialized module edges: scripted HIC & command burst without charge
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let hic_type = ctx.sde.types.get(&999_201).unwrap();
    let hic_scripted = build_specialized_module_result(
        "Activate",
        &fit,
        &ctx,
        mod1_id,
        hic_type,
        None,
        37_294,
        None,
    )
    .expect("scripted hic");
    assert_eq!(hic_scripted.dict_get("scripted").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hic_scripted.dict_get("disallowAssistance").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(hic_scripted.dict_get("bubbleRadius"), Some(&PyRep::None));

    let burst_type = ctx.sde.types.get(&999_202).unwrap();
    let burst_no_charge = build_specialized_module_result(
        "Activate",
        &fit,
        &ctx,
        mod2_id,
        burst_type,
        None,
        0,
        None,
    )
    .expect("burst without charge");
    assert_eq!(burst_no_charge.dict_get("chargeTypeID"), Some(&PyRep::None));
}

#[test]
fn unload_ammo_ownership_flag_fallback_and_not_enough_cargo_space() {
    let ctx = test_ctx_with_sde(|sde| {
        let bulky_ammo = custom_inv_type(sde, 999_210, 83, 8, 50.0, &[]);
        let mut small_hull = sde.types.get(&587).unwrap().clone();
        small_hull.type_id = 999_211;
        small_hull.capacity = 40.0;
        small_hull.default_attributes.insert(eve_sde::attr_ids::CAPACITY, 40.0);
        sde.types.insert(999_210, bulky_ammo);
        sde.types.insert(999_211, small_hull);
    });
    let char_id = 93_100_010i64;
    let ship_id = 1_000_400_101i64;
    let mod_id = 1_000_400_102i64;
    let charge_id = 1_000_400_103i64;

    insert_item(&ctx, ship_id, 999_211, char_id, 60_003_760, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, mod_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, charge_id, 999_210, char_id, ship_id, InventoryFlag::HiSlot0, 2, false);

    let sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    };

    // Ship ownership violation
    let foreign_sess = SessionState {
        user_id: Some(99_999),
        character_id: Some(99_999),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let own_err = handle_unload_ammo(&ctx, &foreign_sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
        .unwrap_err();
    assert_eq!(own_err.code, "ShipOwnershipViolation");

    // Unload charges hits NotEnoughCargoSpace and leaves charge intact in HiSlot0
    let space_err = handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(mod_id)])
        .unwrap_err();
    assert_eq!(space_err.code, "NotEnoughCargoSpace");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&charge_id].flag, InventoryFlag::HiSlot0);
        assert_eq!(snap.items[&charge_id].quantity, 2);
    });

    // Flag fallback (MedSlot0 = 19) resolves module cleanly when only module occupies that slot
    let empty_mod_id = 1_000_400_104i64;
    insert_item(&ctx, empty_mod_id, 484, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    let ok_flag = handle_unload_ammo(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(19)])
        .unwrap();
    assert_eq!(ok_flag, PyRep::None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn burst_jammer_safety_and_pending_cycle_actions_salvage_and_overload() {
    let ctx = test_ctx_with_sde(|sde| {
        let burst = custom_inv_type(sde, 999_220, 80, 7, 5.0, &[(54, 6_000.0)]);
        sde.types.insert(999_220, burst);
    });
    let hi_sys = 30_000_142u32;
    let char_id = 93_100_020i64;
    let ship_id = 1_000_400_201i64;
    let mod_id = 1_000_400_202i64;
    let salvager_id = 1_000_400_203i64;
    let victim_char = 93_100_021i64;
    let victim_ship = 1_000_400_299i64;

    ensure_char(&ctx, char_id, hi_sys, ship_id, SafetyLevel::Green);
    ensure_char(&ctx, victim_char, hi_sys, victim_ship, SafetyLevel::Green);
    insert_item(&ctx, ship_id, 587, char_id, hi_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship, 587, victim_char, hi_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_id, 999_220, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, salvager_id, 484, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);

    spawn_ship(&ctx, hi_sys, ship_id, 587, char_id, DVec3::ZERO, false).await;
    spawn_ship(&ctx, hi_sys, victim_ship, 587, victim_char, DVec3::new(1_000.0, 0.0, 0.0), false).await;
    let sys_h = ctx.systems.get_or_spawn(hi_sys);
    let burst_type = ctx.sde.types.get(&999_220).unwrap().clone();
    let burst_state = DogmaItemState::from_sde(char_id, 999_220, &ctx.sde);

    // Highsec green safety blocks burst jammer
    let hi_green = handle_ecm_or_burst_jammer_activation(
        &ctx, &sys_h, hi_sys, char_id, ship_id, mod_id, &burst_type, &burst_state, None, 20.0,
    )
    .await
    .unwrap_err();
    assert_eq!(hi_green.code, "SafetyBlocked");

    // Highsec yellow safety ALSO blocks burst jammer
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Yellow);
    });
    let hi_yellow = handle_ecm_or_burst_jammer_activation(
        &ctx, &sys_h, hi_sys, char_id, ship_id, mod_id, &burst_type, &burst_state, None, 20.0,
    )
    .await
    .unwrap_err();
    assert_eq!(hi_yellow.code, "SafetyBlocked");

    // SyncOverloadState skips Offline module (not in module_online_states=true) and SalvageEnd deactivates salvager
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, false);
        snap.module_active_states.insert(salvager_id);
    });
    {
        let mut st = sys_h.actor_state.write().unwrap();
        st.pending_cycle_actions.push(PendingCycleAction::SyncOverloadState {
            char_id: Some(char_id),
            ship_id,
            module_id: mod_id,
            is_overloaded: true,
        });
        st.pending_cycle_actions.push(PendingCycleAction::SalvageEnd {
            char_id,
            ship_id,
            module_id: salvager_id,
            module_type_id: 484,
            wreck_id: 0,
            repeat: 1,
            effect_name: "salvage".into(),
            next_cycle_cap_deducted: false,
            deducted_cap_gj: 0.0,
        });
    }
    execute_pending_cycle_actions(&ctx, hi_sys).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_id));
        assert!(!snap.module_active_states.contains(&salvager_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mercden_remote_shield_repair_validation_edges() {
    let ctx = test_ctx_with_sde(|sde| {
        let rsb = custom_inv_type(sde, 999_230, 41, 7, 5.0, &[(54, 10_000.0), (6, 50.0)]);
        sde.types.insert(999_230, rsb);
    });
    let sys_id = 30_000_142u32;
    let char_id = 93_100_030i64;
    let ship_id = 1_000_400_301i64;
    let mod_id = 1_000_400_302i64;
    let den_id = 1_000_400_399i64;

    ensure_char(&ctx, char_id, sys_id, ship_id, SafetyLevel::Red);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_id, 999_230, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);

    spawn_ship(&ctx, sys_id, ship_id, 587, char_id, DVec3::ZERO, false).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            eve_db::types::UpwellStructureRecord {
                structure_id: den_id,
                type_id: 85_230,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Test Mercenary Den".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: den_id,
                type_id: 85_230,
                position: DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 4818,
                category_id: 65,
                owner_id: 98_000_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let rsb_type = ctx.sde.types.get(&999_230).unwrap();
    let fm = resolve_snapshot_fitted_module(&ctx, ship_id, mod_id).unwrap();
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);

    // 1. Target not locked -> TargetNotLocked
    let err_lock = try_handle_mercden_remote_shield_repair(
        &ctx, &sys_h, sys_id, char_id, ship_id, mod_id, &fm, rsb_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_lock.code, "TargetNotLocked");

    // 2. Locked target, ship cloaked -> Cloaked
    sys_h
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: den_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .unwrap();
    sys_h
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let err_cloak = try_handle_mercden_remote_shield_repair(
        &ctx, &sys_h, sys_id, char_id, ship_id, mod_id, &fm, rsb_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_cloak.code, "Cloaked");

    // 3. Uncloak ship -> TargetOutOfRange (50_000m > 10_000m)
    sys_h
        .set_combat_state(ship_id, CombatState::default())
        .await
        .unwrap();
    let err_range = try_handle_mercden_remote_shield_repair(
        &ctx, &sys_h, sys_id, char_id, ship_id, mod_id, &fm, rsb_type, &fit, "Activate", den_id,
    )
    .await
    .unwrap_err();
    assert_eq!(err_range.code, "TargetOutOfRange");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deactivate_repeating_cycle_after_action_superweapon_emits_real_effect_ids() {
    let ctx = test_ctx_with_sde(|_| {});
    let sys_id = 30_000_142u32;
    let char_id = 93_100_031i64;
    let ship_id = 1_000_400_311i64;
    let mod_amarr = 1_000_400_312i64;
    let mod_lance = 1_000_400_313i64;
    let mod_unknown = 1_000_400_314i64;

    ensure_char(&ctx, char_id, sys_id, ship_id, SafetyLevel::Red);
    let online_uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap
            .accounts
            .values()
            .next()
            .map(|a| a.user_id)
            .unwrap_or(1);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
        }
        uid
    });
    ctx.db
        .acquire_online_lock(online_uid, 9_000_031, Some(char_id), true, ctx.now_unix_secs())
        .expect("acquire online lock for cycle-stop superweapon test");
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, mod_amarr, 24550, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, mod_lance, 40631, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, mod_unknown, 999_999, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    deactivate_repeating_cycle_after_action(
        &ctx,
        &sys_h,
        Some(char_id),
        ship_id,
        mod_amarr,
        "superWeaponImmobilize",
    )
    .await;
    deactivate_repeating_cycle_after_action(
        &ctx,
        &sys_h,
        Some(char_id),
        ship_id,
        mod_lance,
        "superWeaponLanceBeam",
    )
    .await;
    deactivate_repeating_cycle_after_action(
        &ctx,
        &sys_h,
        Some(char_id),
        ship_id,
        mod_unknown,
        "superWeapon",
    )
    .await;

    let effects: Vec<(i64, i64)> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnGodmaShipEffect")
        .filter_map(|n| {
            let mid = n.args.first()?.as_i64()?;
            let eff = n.args.get(1)?.as_i64()?;
            Some((mid, eff))
        })
        .collect();
    assert_eq!(
        effects,
        vec![(mod_amarr, 4489), (mod_lance, 6472), (mod_unknown, 4489)],
        "superweapon cycle-stop OnGodmaShipEffect must emit real SDE effect IDs, never 0 or synthetic strings (#1019)"
    );
}
