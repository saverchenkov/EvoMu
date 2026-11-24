//! Edge-case unit tests for `fitting_module_cycles_guards`:
//! `is_warp_safe_cloak_module`, `is_ship_actively_cloaked` (combat state + DB fallback),
//! `enforce_space_module_cloak_and_warp_guard`, `enforce_space_action_cloak_and_warp_guard`,
//! and `fallback_fitted_weapon_module`.

use super::*;
use crate::services::combat_crimewatch::SafetyLevel;
use crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    ctx
}

fn inv_type(type_id: u32, group_id: u32, name: &str, effect_ids: Vec<u16>) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 7,
        name: name.into(),
        mass: 0.0,
        volume: 5.0,
        capacity: 0.0,
        radius: 0.0,
        base_price: 0.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids,
        default_attributes: HashMap::new(),
    }
}

fn ship_ball(ship_id: i64, mode: MovementMode) -> Ball {
    Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    }
}

#[test]
fn is_warp_safe_cloak_module_covers_all_predicates() {
    let ctx = make_ctx();

    // Wrong group_id -> false even if Covert Ops or effect 980
    assert!(!is_warp_safe_cloak_module(
        &ctx,
        &inv_type(11578, 53, "Covert Ops Laser", vec![980])
    ));
    // Group 330 with type_id 11578 -> true
    assert!(is_warp_safe_cloak_module(
        &ctx,
        &inv_type(11578, 330, "Generic Cloak", vec![])
    ));
    // Group 330 with effect 980 -> true
    assert!(is_warp_safe_cloak_module(
        &ctx,
        &inv_type(999_001, 330, "Generic Cloak", vec![980])
    ));
    // Group 330 with "Covert Ops" in name -> true
    assert!(is_warp_safe_cloak_module(
        &ctx,
        &inv_type(999_002, 330, "Covert Ops Cloaking Device II", vec![])
    ));
    // Group 330 with an SDE effect that has is_warp_safe == true (if any exist)
    if let Some(ws_eid) = ctx
        .sde
        .effects
        .values()
        .find(|e| e.is_warp_safe && e.effect_id != 980)
        .map(|e| e.effect_id)
    {
        assert!(is_warp_safe_cloak_module(
            &ctx,
            &inv_type(999_003, 330, "Synth Cloak", vec![ws_eid])
        ));
    }
    // Plain non-covops cloak -> false
    assert!(!is_warp_safe_cloak_module(
        &ctx,
        &inv_type(11577, 330, "Prototype Cloaking Device I", vec![])
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn is_ship_actively_cloaked_and_guards_cover_warp_gate_cloak_and_active_cloak() {
    let ctx = make_ctx();
    let sys_id = 30_000_142;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 90_000_001;
    let ship_id = 1_050_000_001;

    // 1. Ship in WarpPhase::Cruising
    let warp_mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e9, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e9,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    sys.spawn_ball_with_combat(ship_ball(ship_id, warp_mode), CombatState::default())
        .await
        .unwrap();

    let weapon = inv_type(484, 53, "Light Electron Blaster II", vec![]);
    let nullifier = inv_type(58958, 4117, "Interdiction Nullifier I", vec![]);
    let covops_cloak = inv_type(11578, 330, "Covert Ops Cloaking Device II", vec![980]);
    let probe_launcher = inv_type(17901, 481, "Core Probe Launcher I", vec![]);
    let proto_cloak = inv_type(11577, 330, "Prototype Cloaking Device I", vec![]);

    // Regular weapon blocked in warp; nullifier & covops cloak allowed
    assert_eq!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &weapon)
            .await
            .unwrap_err()
            .code,
        "CantActivateInWarp"
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &nullifier)
            .await
            .is_ok()
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &covops_cloak)
            .await
            .is_ok()
    );
    // Space action blocked in warp
    let act_err =
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "launch drones")
            .await
            .unwrap_err();
    assert_eq!(act_err.code, "CantActivateInWarp");
    assert!(act_err.message.contains("launch drones"));

    // 2. WarpPhase::Aligning allows weapon & space action
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e9, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e9,
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
        };
    }
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &weapon)
            .await
            .is_ok()
    );
    assert!(
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "engage")
            .await
            .is_ok()
    );

    // 3. Gate cloak (gate_cloak_remaining_ticks > 0) is NOT actively cloaked and breaks on guard
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Stop;
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 30;
        cs.cloak_velocity_multiplier = None;
    }
    assert!(!is_ship_actively_cloaked(&ctx, &sys, ship_id).await);
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &weapon)
            .await
            .is_ok()
    );
    assert_eq!(
        sys.get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .gate_cloak_remaining_ticks,
        0
    );

    // Re-apply gate cloak and verify action guard also breaks it
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 15;
    }
    assert!(
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "fire")
            .await
            .is_ok()
    );
    assert_eq!(
        sys.get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .gate_cloak_remaining_ticks,
        0
    );

    // 4. Non-gate active cloak (is_cloaked && gate_cloak_remaining_ticks == 0)
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    }
    assert!(is_ship_actively_cloaked(&ctx, &sys, ship_id).await);
    assert_eq!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &weapon)
            .await
            .unwrap_err()
            .code,
        "CannotActivateCloaked"
    );
    assert_eq!(
        enforce_space_action_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, "engage")
            .await
            .unwrap_err()
            .code,
        "CannotActivateCloaked"
    );
    // Cloak, Scan Probe Launcher, and Nullifier are permitted while cloaked
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &proto_cloak)
            .await
            .is_ok()
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &probe_launcher)
            .await
            .is_ok()
    );
    assert!(
        enforce_space_module_cloak_and_warp_guard(&ctx, &sys, char_id, ship_id, &nullifier)
            .await
            .is_ok()
    );

    // 5. cloak_velocity_multiplier alone also triggers is_ship_actively_cloaked
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = false;
        cs.cloak_velocity_multiplier = Some(0.25);
    }
    assert!(is_ship_actively_cloaked(&ctx, &sys, ship_id).await);

    // 6. DB module_active_states fallback when combat state has no cloak
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = false;
        cs.cloak_velocity_multiplier = None;
    }
    assert!(!is_ship_actively_cloaked(&ctx, &sys, ship_id).await);
    let cloak_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 330)
        .map(|t| t.type_id)
        .unwrap_or(11578);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: cloak_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(777_001);
    });
    assert!(is_ship_actively_cloaked(&ctx, &sys, ship_id).await);
}

#[test]
fn fallback_fitted_weapon_module_covers_missing_unloaded_and_charged_cases() {
    let ctx = make_ctx();
    let ship_id = 1_050_000_002;
    let mod_id = 888_001;
    let charge_id = 888_002;

    assert!(fallback_fitted_weapon_module(&ctx, ship_id, mod_id).is_none());

    ctx.db.with_mut_snapshot(|snap| {
        // Item on another ship -> None
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 484,
                owner_id: 90_000_001,
                location_id: ship_id + 99,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert!(fallback_fitted_weapon_module(&ctx, ship_id, mod_id).is_none());

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().location_id = ship_id;
        // Zero-quantity charge in same slot should be ignored
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 232,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 0,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let no_charge = fallback_fitted_weapon_module(&ctx, ship_id, mod_id).unwrap();
    assert_eq!(no_charge.item_id, mod_id);
    assert_eq!(no_charge.charge_type_id, None);

    // Positive-quantity charge in same slot gets attached
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&charge_id).unwrap().quantity = 40;
    });
    let with_charge = fallback_fitted_weapon_module(&ctx, ship_id, mod_id).unwrap();
    assert_eq!(with_charge.charge_type_id, Some(232));
}

#[tokio::test]
async fn commit_module_cycle_atomic_with_replace_replaces_active_cycle_p2_r8_03() {
    let ctx = make_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 990_811_001_i64;
    let mod_id = 990_811_002_i64;
    let sys = ctx.systems.get_or_spawn(sys_id);

    fmc_spawn_target_ball(&ctx, sys_id, ship_id, 587, 25, 6, char_id, DVec3::ZERO, 1_000.0, 1_000.0).await;

    let first_cycle = crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: 21_096,
        owner_char_id: Some(char_id),
        effect_name: "effects.First".to_string(),
        repeat: 0,
        duration_s: 20.0,
        cap_need_per_cycle: 100.0,
        immobilize_self: true,
        started_at_unix: ctx.now_unix(),
        ..Default::default()
    };
    let out1 = commit_module_cycle_atomic_with_notifications(
        &ctx,
        &sys,
        char_id,
        ship_id,
        first_cycle.clone(),
        false,
        false,
        true,
        crate::system_actor::ProtectionBreakReason::ModuleActivation,
    )
    .await
    .expect("initial cycle activation must succeed");
    assert!((out1.capacitor_after - 900.0).abs() < 1e-6);
    assert!(sys.has_module_cycle(ship_id, mod_id).await);

    // Non-replacing call (`replace_existing_cycle == false`) must fail with ModuleAlreadyActive.
    let err = commit_module_cycle_atomic_with_notifications(
        &ctx,
        &sys,
        char_id,
        ship_id,
        first_cycle,
        false,
        false,
        true,
        crate::system_actor::ProtectionBreakReason::ModuleActivation,
    )
    .await
    .expect_err("duplicate cycle without replacement must fail");
    assert!(err.contains("ModuleAlreadyActive"));

    // Replacing call (`replace_existing_cycle == true`) swaps the active cycle atomically and charges cap once.
    let replacement = crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: 21_096,
        owner_char_id: Some(char_id),
        effect_name: "effects.Replaced".to_string(),
        repeat: 0,
        duration_s: 4.0,
        cap_need_per_cycle: 50.0,
        immobilize_self: false,
        started_at_unix: ctx.now_unix(),
        ..Default::default()
    };
    let out2 = commit_module_cycle_atomic_with_replace_notifications(
        &ctx,
        &sys,
        char_id,
        ship_id,
        replacement,
        false,
        false,
        true,
        true,
        crate::system_actor::ProtectionBreakReason::ModuleActivation,
    )
    .await
    .expect("replace_existing_cycle=true must replace active cycle");
    assert!((out2.capacitor_after - 850.0).abs() < 1e-6);
    assert!(sys.has_module_cycle(ship_id, mod_id).await);

    let remaining = sys
        .remove_module_cycle(ship_id, mod_id)
        .await
        .expect("remove cycle")
        .expect("replaced cycle must be active");
    assert!(
        remaining <= 4.1,
        "replaced cycle must carry new 4.0s duration instead of old 20.0s duration (got {remaining})"
    );
    assert_eq!(
        sys.remove_module_cycle(ship_id, mod_id).await.unwrap(),
        None,
        "exactly one cycle must have existed after replacement"
    );
}

#[allow(clippy::too_many_arguments)]
fn fmc_custom_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut t = sde.types.get(&587).expect("rifter").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.effect_ids = effects.to_vec();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn fmc_ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("FMC-Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = 1;
        ch.fleet_id = None;
        snap.characters.insert(char_id, ch);
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

#[allow(clippy::too_many_arguments)]
fn fmc_insert_item(
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

#[allow(clippy::too_many_arguments)]
async fn fmc_spawn_target_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
    shield: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(1_000.0);
    cs.shield_hp = shield;
    cs.max_shield_hp = shield;
    cs.armor_hp = 5_000.0;
    cs.max_armor_hp = 5_000.0;
    cs.structure_hp = 5_000.0;
    cs.max_structure_hp = 5_000.0;
    cs.shield_resonance = [1.0; 4];
    let meta = eve_destiny::SlimItemMeta {
        group_id,
        category_id,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ball");
}

const FMC_CHAR_ID: i64 = 97_100_001;
const FMC_TARGET_CHAR: i64 = 97_100_002;
const FMC_CORP_ID: i64 = 98_000_001;
const FMC_SYS_ID: u32 = 30_000_142;
const FMC_SHIP_ID: i64 = 1_000_800_001;
const FMC_TARGET_SHIP: i64 = 1_000_800_002;
const FMC_TARGET_CONT: i64 = 1_000_800_003;
const FMC_TARGET_DEPOT: i64 = 1_000_800_004;
const FMC_MOON_TARGET: i64 = 1_000_800_005;
const FMC_STRUCT_ID: i64 = 1_000_800_090;

const FMC_MOD_CAP: i64 = 1_000_800_010;
const FMC_CAP_CHARGE: i64 = 1_000_800_011;
const FMC_MOD_TUR1: i64 = 1_000_800_012;
const FMC_AMMO1: i64 = 1_000_800_013;
const FMC_MOD_TUR2: i64 = 1_000_800_014;
const FMC_AMMO2: i64 = 1_000_800_015;
const FMC_MOD_MISSILE: i64 = 1_000_800_016;
const FMC_MISSILE_CHARGE: i64 = 1_000_800_017;
const FMC_MOD_VORTON: i64 = 1_000_800_018;
const FMC_VORTON_AMMO: i64 = 1_000_800_019;
const FMC_MOD_SURVEY_ATTR: i64 = 1_000_800_020;
const FMC_MOD_PLAIN: i64 = 1_000_800_021;
const FMC_MOD_PROBE_LNCH: i64 = 1_000_800_022;
const FMC_PROBE_CHARGE: i64 = 1_000_800_023;
const FMC_MOD_CRYSTAL: i64 = 1_000_800_024;
const FMC_CRYSTAL_CHARGE: i64 = 1_000_800_025;
const FMC_STRUCT_MOD: i64 = 1_000_800_091;

const T_HULL: u32 = 998_100;
const T_CAP_BOOSTER: u32 = 998_101;
const T_CAP_CHARGE: u32 = 998_102;
const T_TURRET: u32 = 998_103;
const T_AMMO: u32 = 998_104;
const T_MISSILE_LNCH: u32 = 998_105;
const T_MISSILE: u32 = 998_106;
const T_VORTON: u32 = 998_107;
const T_VORTON_AMMO: u32 = 998_108;
const T_SURVEY_BY_ATTR: u32 = 998_109;
const T_PLAIN_NO_ATTR197: u32 = 998_110;
const T_CONTAINER_340: u32 = 998_111;
const T_DEPOT_1328: u32 = 998_112;
const T_SURVEY_PROBE_LNCH: u32 = 998_113;
const T_SURVEY_PROBE: u32 = 998_114;
const T_CRYSTAL: u32 = 998_115;

#[inline(never)]
fn fmc_custom_sde() -> Arc<SdeRegistry> {
    static CUSTOM_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(CUSTOM_SDE.get_or_init(|| {
        let mut sde = (*shared_sde()).clone();
        let custom_types: &[(u32, u32, u32, f64, f64, &str, &[(u16, f64)], &[u16])] = &[
            (
                T_HULL,
                25,
                6,
                50_000.0,
                500.0,
                "Multi-Fit Test Hull",
                &[
                    (attr_ids::HI_SLOTS, 8.0),
                    (attr_ids::MED_SLOTS, 6.0),
                    (attr_ids::LOW_SLOTS, 4.0),
                    (attr_ids::TURRET_SLOTS_LEFT, 8.0),
                    (attr_ids::LAUNCHER_SLOTS_LEFT, 8.0),
                    (attr_ids::CPU_OUTPUT, 50_000.0),
                    (attr_ids::POWER_OUTPUT, 500_000.0),
                    (attr_ids::CAPACITOR_CAPACITY, 5_000.0),
                    (attr_ids::MAX_LOCKED_TARGETS, 8.0),
                    (attr_ids::MAX_TARGET_RANGE, 150_000.0),
                    (attr_ids::SCAN_RESOLUTION, 1_000.0),
                    (attr_ids::SIGNATURE_RADIUS, 40.0),
                ],
                &[],
            ),
            (
                T_CAP_BOOSTER,
                76,
                7,
                5.0,
                50.0,
                "Small Capacitor Booster II",
                &[(attr_ids::SPEED, 2_000.0)],
                &[13, 16],
            ),
            (T_CAP_CHARGE, 87, 8, 5.0, 0.0, "Cap Booster 400", &[(67, 400.0)], &[]),
            (
                T_TURRET,
                53,
                7,
                5.0,
                20.0,
                "Dual Light Pulse Laser II",
                &[
                    (attr_ids::SPEED, 2_000.0),
                    (attr_ids::DAMAGE_MULTIPLIER, 2.0),
                    (attr_ids::MAX_RANGE, 30_000.0),
                    (attr_ids::FALLOFF, 10_000.0),
                    (attr_ids::TRACKING_SPEED, 1.0),
                    (6, 1.0),
                ],
                &[12, 16],
            ),
            (
                T_AMMO,
                85,
                8,
                0.1,
                0.0,
                "Synthetic Hybrid Charge",
                &[
                    (attr_ids::EM_DAMAGE, 10.0),
                    (attr_ids::THERMAL_DAMAGE, 20.0),
                    (attr_ids::KINETIC_DAMAGE, 30.0),
                    (attr_ids::EXPLOSIVE_DAMAGE, 40.0),
                ],
                &[],
            ),
            (
                T_CRYSTAL,
                86,
                8,
                0.1,
                0.0,
                "Volatile Crystal",
                &[(attr_ids::EM_DAMAGE, 50.0), (784, 0.25), (783, 100.0)],
                &[],
            ),
            (
                T_MISSILE_LNCH,
                507,
                7,
                5.0,
                20.0,
                "Light Missile Launcher II",
                &[(attr_ids::SPEED, 2_000.0), (6, 0.0)],
                &[12, 16],
            ),
            (
                T_MISSILE,
                384,
                8,
                0.1,
                0.0,
                "Mjolnir Light Missile",
                &[
                    (attr_ids::EM_DAMAGE, 60.0),
                    (attr_ids::THERMAL_DAMAGE, 20.0),
                    (attr_ids::KINETIC_DAMAGE, 10.0),
                    (attr_ids::EXPLOSIVE_DAMAGE, 10.0),
                    (attr_ids::MAX_VELOCITY, 4_000.0),
                    (2811, 5_000.0),
                    (653, 50.0),
                    (654, 100.0),
                ],
                &[],
            ),
            (
                T_VORTON,
                4060,
                7,
                5.0,
                20.0,
                "Small Vorton Projector II",
                &[
                    (attr_ids::SPEED, 2_000.0),
                    (attr_ids::DAMAGE_MULTIPLIER, 2.0),
                    (attr_ids::MAX_RANGE, 35_000.0),
                    (attr_ids::FALLOFF, 5_000.0),
                    (6, 1.0),
                    (604, 85.0),
                ],
                &[12, 16],
            ),
            (
                T_VORTON_AMMO,
                85,
                8,
                0.1,
                0.0,
                "ElectroPunch Ultra S",
                &[(attr_ids::EM_DAMAGE, 50.0), (attr_ids::KINETIC_DAMAGE, 50.0)],
                &[],
            ),
            (
                T_SURVEY_BY_ATTR,
                999,
                7,
                5.0,
                0.0,
                "Synthetic Cargo Scanner with Survey Attr",
                &[(attr_ids::DURATION, 3_000.0), (197, 25_000.0)],
                &[13],
            ),
            (
                T_PLAIN_NO_ATTR197,
                999,
                7,
                5.0,
                0.0,
                "Plain Group 48 Module",
                &[(attr_ids::DURATION, 3_000.0)],
                &[13],
            ),
            (T_CONTAINER_340, 340, 2, 100.0, 0.0, "Secure Container", &[], &[]),
            (T_DEPOT_1328, 1328, 22, 100.0, 0.0, "Mobile Depot", &[], &[]),
            (
                T_SURVEY_PROBE_LNCH,
                1226,
                7,
                5.0,
                2.0,
                "Survey Probe Launcher I",
                &[(attr_ids::DURATION, 10_000.0)],
                &[12],
            ),
            (
                T_SURVEY_PROBE,
                492,
                8,
                0.2,
                0.0,
                "Quest Survey Probe I",
                &[(2697, 100_000_000.0)],
                &[],
            ),
        ];
        for &(type_id, group_id, category_id, volume, capacity, name, attrs, effects) in custom_types
        {
            let custom = fmc_custom_type(
                &sde,
                type_id,
                group_id,
                category_id,
                volume,
                capacity,
                name,
                attrs,
                effects,
            );
            sde.types.insert(type_id, custom);
        }
        Arc::new(sde)
    }))
}

#[inline(never)]
fn fmc_build_seeded_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(fmc_custom_sde(), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_100_000)));

    fmc_ensure_char(&ctx, FMC_CHAR_ID, FMC_CORP_ID, FMC_SYS_ID, FMC_SHIP_ID);
    fmc_ensure_char(&ctx, FMC_TARGET_CHAR, 98_000_002, FMC_SYS_ID, FMC_TARGET_SHIP);

    let items: &[(i64, u32, i64, i64, InventoryFlag, i32, bool)] = &[
        (FMC_SHIP_ID, T_HULL, FMC_CHAR_ID, FMC_SYS_ID as i64, InventoryFlag::None, 1, true),
        (FMC_TARGET_SHIP, 587, FMC_TARGET_CHAR, FMC_SYS_ID as i64, InventoryFlag::None, 1, true),
        (FMC_MOD_CAP, T_CAP_BOOSTER, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::MedSlot0, 1, true),
        (FMC_MOD_TUR1, T_TURRET, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot0, 1, true),
        (FMC_AMMO1, T_AMMO, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot0, 20, false),
        (FMC_MOD_TUR2, T_TURRET, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot1, 1, true),
        (FMC_AMMO2, T_AMMO, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot1, 20, false),
        (FMC_MOD_MISSILE, T_MISSILE_LNCH, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot2, 1, true),
        (FMC_MISSILE_CHARGE, T_MISSILE, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot2, 20, false),
        (FMC_MOD_VORTON, T_VORTON, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot3, 1, true),
        (FMC_VORTON_AMMO, T_VORTON_AMMO, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot3, 20, false),
        (FMC_MOD_PROBE_LNCH, T_SURVEY_PROBE_LNCH, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot4, 1, true),
        (FMC_PROBE_CHARGE, T_SURVEY_PROBE, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot4, 5, false),
        (FMC_MOD_CRYSTAL, T_TURRET, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot5, 1, true),
        (FMC_CRYSTAL_CHARGE, T_CRYSTAL, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::HiSlot5, 1, true),
        (FMC_MOD_SURVEY_ATTR, T_SURVEY_BY_ATTR, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::MedSlot1, 1, true),
        (FMC_MOD_PLAIN, T_PLAIN_NO_ATTR197, FMC_CHAR_ID, FMC_SHIP_ID, InventoryFlag::MedSlot2, 1, true),
        (FMC_STRUCT_MOD, T_TURRET, FMC_CHAR_ID, FMC_STRUCT_ID, InventoryFlag::HiSlot0, 1, true),
    ];
    for &(item_id, type_id, owner_id, location_id, flag, quantity, singleton) in items {
        fmc_insert_item(&ctx, item_id, type_id, owner_id, location_id, flag, quantity, singleton);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            FMC_STRUCT_ID,
            UpwellStructureRecord {
                structure_id: FMC_STRUCT_ID,
                type_id: 35832,
                owner_corp_id: FMC_CORP_ID,
                solar_system_id: FMC_SYS_ID,
                name: "Struct".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    crate::services::combat_crimewatch::set_char_safety_level(&ctx, FMC_CHAR_ID, SafetyLevel::Red);

    ctx
}

struct FmcModuleCyclesHarness {
    ctx: ServerContext,
    sys: crate::system_actor::SystemHandle,
    sess: SessionState,
    kw: HashMap<String, PyRep>,
}

impl FmcModuleCyclesHarness {
    #[inline(never)]
    async fn setup() -> Self {
        // Box setup and step sub-futures so debug builds keep each test coroutine well below 2 MiB (#1376).
        Box::pin(async {
            let ctx = fmc_build_seeded_ctx();
            let balls = [
                (FMC_SHIP_ID, T_HULL, 25, 6, FMC_CHAR_ID, DVec3::ZERO, 100.0, 5_000.0),
                (
                    FMC_TARGET_SHIP,
                    587,
                    25,
                    6,
                    FMC_TARGET_CHAR,
                    DVec3::new(15_000.0, 0.0, 0.0),
                    1_000.0,
                    10_000.0,
                ),
                (
                    FMC_TARGET_CONT,
                    T_CONTAINER_340,
                    340,
                    2,
                    FMC_TARGET_CHAR,
                    DVec3::new(12_000.0, 0.0, 0.0),
                    0.0,
                    5_000.0,
                ),
                (
                    FMC_TARGET_DEPOT,
                    T_DEPOT_1328,
                    1328,
                    22,
                    FMC_TARGET_CHAR,
                    DVec3::new(13_000.0, 0.0, 0.0),
                    0.0,
                    5_000.0,
                ),
                (
                    FMC_MOON_TARGET,
                    587,
                    25,
                    6,
                    0,
                    DVec3::new(300_000.0, 0.0, 0.0),
                    0.0,
                    5_000.0,
                ),
            ];
            for (ball_id, type_id, group_id, category_id, owner_id, pos, cap, shield) in balls {
                fmc_spawn_target_ball(
                    &ctx,
                    FMC_SYS_ID,
                    ball_id,
                    type_id,
                    group_id,
                    category_id,
                    owner_id,
                    pos,
                    cap,
                    shield,
                )
                .await;
            }

            let sys = ctx.systems.get_or_spawn(FMC_SYS_ID);
            for tid in [FMC_TARGET_SHIP, FMC_TARGET_CONT, FMC_TARGET_DEPOT] {
                sys.add_target_lock(
                    FMC_SHIP_ID,
                    TargetLock {
                        target_id: tid,
                        lock_time_ms: 100.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: Some(100_000.0),
                    },
                )
                .await
                .unwrap();
            }

            let sess = SessionState {
                user_id: Some(FMC_CHAR_ID as u64),
                character_id: Some(FMC_CHAR_ID),
                corporation_id: Some(FMC_CORP_ID),
                solar_system_id: Some(FMC_SYS_ID),
                ship_id: Some(FMC_SHIP_ID),
                ..Default::default()
            };
            Self {
                ctx,
                sys,
                sess,
                kw: HashMap::new(),
            }
        })
        .await
    }

    #[inline(never)]
    async fn call_cycles(
        &mut self,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        handle_module_cycles(&self.ctx, &mut self.sess, None, service, method, args, &self.kw).await
    }

    #[inline(never)]
    async fn step_secs(&self, ticks: usize) {
        Box::pin(async move {
            for _ in 0..ticks {
                let _ = self.sys.step_manual(1.0).await;
            }
        })
        .await;
    }

    #[inline(never)]
    async fn combat_state(&self, ball_id: i64) -> CombatState {
        Box::pin(async move { self.sys.get_combat_state(ball_id).await.unwrap().unwrap() }).await
    }
}

#[tokio::test]
async fn handle_module_cycles_dispatch_boundaries_and_structure_overload_guard() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 1. Dispatch boundary (`L23-24`) and structure overload (`L60`)
    assert!(h
        .call_cycles("nonexistent_svc", "Activate", &[PyRep::Integer(FMC_MOD_CAP)])
        .await
        .is_none());
    assert!(h
        .call_cycles("dogmaIM", "unhandled_op", &[PyRep::Integer(FMC_MOD_CAP)])
        .await
        .is_none());
    h.sess.ship_id = Some(FMC_STRUCT_ID);
    assert_eq!(
        h.call_cycles("dogmaIM", "Overload", &[PyRep::Integer(FMC_STRUCT_MOD)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotOverloadStructureModule"
    );
    h.sess.ship_id = Some(FMC_SHIP_ID);
}

#[tokio::test]
async fn handle_module_cycles_cap_booster_missing_charge_and_injection() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 2. Cap Booster (`group 76`, `L957`): missing charge error, then charge attr 67 = 400.0 injection
    assert_eq!(
        h.call_cycles("dogmaIM", "Activate", &[PyRep::Integer(FMC_MOD_CAP)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoChargesLoaded"
    );
    fmc_insert_item(
        &h.ctx,
        FMC_CAP_CHARGE,
        T_CAP_CHARGE,
        FMC_CHAR_ID,
        FMC_SHIP_ID,
        InventoryFlag::MedSlot0,
        3,
        false,
    );
    let _ = h
        .call_cycles("dogmaIM", "Activate", &[PyRep::Integer(FMC_MOD_CAP)])
        .await
        .unwrap()
        .expect("cap booster activate");
    h.step_secs(3).await;
    let cap_after = h.combat_state(FMC_SHIP_ID).await.capacitor;
    assert!(
        cap_after >= 490.0,
        "expected Cap Booster 400 injection from 100.0 cap, got {cap_after}"
    );
    let _ = h
        .call_cycles("dogmaIM", "Deactivate", &[PyRep::Integer(FMC_MOD_CAP)])
        .await;
}

#[tokio::test]
async fn handle_module_cycles_survey_probe_launcher_while_following() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 3. Survey Probe Launcher in `MovementMode::Follow` (`L571, L577`)
    {
        let mut bp = h.sys.ballpark.write().unwrap();
        bp.balls.get_mut(&FMC_SHIP_ID).unwrap().mode = MovementMode::Follow {
            target_id: FMC_MOON_TARGET,
            range: 1_000.0,
        };
    }
    let probe_rep = h
        .call_cycles("dogmaIM", "Activate", &[PyRep::Integer(FMC_MOD_PROBE_LNCH)])
        .await
        .unwrap()
        .expect("survey probe launch while following");
    assert_eq!(
        probe_rep.dict_get("launcherID").and_then(|v| v.as_i64()),
        Some(FMC_MOD_PROBE_LNCH)
    );
    h.sys.ballpark.write().unwrap().balls.get_mut(&FMC_SHIP_ID).unwrap().mode = MovementMode::Stop;
}

#[tokio::test]
async fn handle_module_cycles_survey_scanner_attr197_vs_plain_module() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 4. Survey scanner branch via attr 197 on group 48 (`L1654`: `group_id == 49 || attr 197 > 0`)
    let surv_rep = h
        .call_cycles("dogmaIM", "Activate", &[PyRep::Integer(FMC_MOD_SURVEY_ATTR)])
        .await
        .unwrap()
        .expect("attr 197 survey scanner");
    assert!(surv_rep.dict_get("asteroids").is_some());
    let plain_rep = h
        .call_cycles("dogmaIM", "Activate", &[PyRep::Integer(FMC_MOD_PLAIN)])
        .await
        .unwrap()
        .expect("plain group 48 without attr 197");
    assert!(plain_rep.dict_get("asteroids").is_none());
}

#[tokio::test]
async fn handle_module_cycles_vorton_projector_on_container_and_deployable() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 5. Vorton (`group 4060`, `L246`) on container (340) and deployable (1328)
    for tgt in [FMC_TARGET_CONT, FMC_TARGET_DEPOT] {
        let v_rep = h
            .call_cycles(
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(FMC_MOD_VORTON), PyRep::Integer(tgt)],
            )
            .await
            .unwrap()
            .expect("vorton special target");
        assert_eq!(v_rep.dict_get("targetID"), Some(&PyRep::Integer(tgt)));
        assert!(v_rep.dict_get("appliedDamage").is_some());
        h.step_secs(3).await;
        let _ = h
            .call_cycles("dogmaIM", "Deactivate", &[PyRep::Integer(FMC_MOD_VORTON)])
            .await;
    }
}

#[tokio::test]
async fn handle_module_cycles_linked_turret_bank_deals_damage() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 6a. Linked turret bank (2 guns)
    h.ctx.db.with_mut_snapshot(|snap| {
        snap.weapon_banks.insert(
            FMC_SHIP_ID,
            HashMap::from([(FMC_MOD_TUR1, vec![FMC_MOD_TUR1, FMC_MOD_TUR2])]),
        );
    });
    let shield_before = h.combat_state(FMC_TARGET_SHIP).await.shield_hp;
    let tur_rep = h
        .call_cycles(
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(FMC_MOD_TUR1), PyRep::Integer(FMC_TARGET_SHIP)],
        )
        .await
        .unwrap()
        .expect("linked turret bank");
    assert_eq!(tur_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(2.0));
    h.step_secs(3).await;
    let shield_after_tur = h.combat_state(FMC_TARGET_SHIP).await.shield_hp;
    assert!(
        shield_after_tur < shield_before,
        "expected turret cycle to deal damage (before={shield_before}, after={shield_after_tur})"
    );
    let _ = h
        .call_cycles("dogmaIM", "Deactivate", &[PyRep::Integer(FMC_MOD_TUR1)])
        .await;
}

#[tokio::test]
async fn handle_module_cycles_volatile_crystal_turret_cycle() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 6b. Volatile crystal gun
    let _ = h
        .call_cycles(
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(FMC_MOD_CRYSTAL), PyRep::Integer(FMC_TARGET_SHIP)],
        )
        .await
        .unwrap()
        .expect("crystal turret");
    h.step_secs(3).await;
    let _ = h
        .call_cycles("dogmaIM", "Deactivate", &[PyRep::Integer(FMC_MOD_CRYSTAL)])
        .await;
}

#[tokio::test]
async fn handle_module_cycles_missile_launcher_cycle() {
    let mut h = FmcModuleCyclesHarness::setup().await;

    // 6c. Missile launcher (group 507)
    let shield_before_mis = h.combat_state(FMC_TARGET_SHIP).await.shield_hp;
    let mis_rep = h
        .call_cycles(
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(FMC_MOD_MISSILE), PyRep::Integer(FMC_TARGET_SHIP)],
        )
        .await
        .unwrap()
        .expect("missile launcher");
    assert_eq!(mis_rep.dict_get("duration").and_then(|v| v.as_f64()), Some(2.0));
    h.step_secs(4).await;
    let shield_after_mis = h.combat_state(FMC_TARGET_SHIP).await.shield_hp;
    assert!(
        shield_after_mis <= shield_before_mis,
        "expected missile cycle completion without error"
    );
}

