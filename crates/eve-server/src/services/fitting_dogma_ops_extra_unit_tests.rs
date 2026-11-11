//! Deep branch-coverage and mutation-killing unit tests for `fitting_dogma_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState};
use crate::services::inventory_fitting::fitting_dogma_ops::handle_dogma_ops;
use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

async fn call_dogma(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_dogma_ops(ctx, session, None, "dogmaIM", method, args, kwargs).await
}

fn setup_dogma_ctx() -> (ServerContext, SessionState, i64, i64, i64, u32) {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_300_001_i64;
    let other_char_id = 96_300_002_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_300_100_i64;
    let target_ship_id = 96_300_200_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "u96301".into(),
            AccountRecord {
                user_id: 96_301,
                username: "u96301".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for (cid, uid, sid) in [(char_id, 96_301, ship_id), (other_char_id, 96_302, target_ship_id)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
        }
        for (sid, oid) in [(ship_id, char_id), (target_ship_id, other_char_id)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: oid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for &sk in &[3429_u32, 3430, 28164, 28879, 28880] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
            );
        }
    });

    let mut session = SessionState::default();
    session.user_id = Some(96_301);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    (ctx, session, char_id, ship_id, target_ship_id, sys_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn targeting_error_branches_drone_source_auth_and_remove_targets_variants() {
    let (ctx, mut session, _char_id, ship_id, target_ship_id, sys_id) = setup_dogma_ctx();
    let kw = HashMap::new();

    assert!(handle_dogma_ops(&ctx, &mut session, None, "invbroker", "AddTargetOBO", &[], &kw).await.is_none());
    assert!(call_dogma(&ctx, &mut session, "NoSuchMethod", &[], &kw).await.is_none());

    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(0)], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(999_888), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "ShipOwnershipViolation");

    session.solar_system_id = None;
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "NoSolarSystem");
    session.solar_system_id = Some(sys_id);

    // Missing source ball in space -> TargetingFailed
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let make_ball = |id: i64, pos: DVec3| Ball {
        id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball_with_meta(make_ball(ship_id, DVec3::ZERO), CombatState::default(), SlimItemMeta::default()).await;

    // Missing target ball in space -> TargetingFailed
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");

    // Target ball tethered -> TargetInvulnerable
    let _ = sys.spawn_ball_with_meta(
        make_ball(target_ship_id, DVec3::new(2_000.0, 0.0, 0.0)),
        CombatState { is_tethered: true, ..Default::default() },
        SlimItemMeta::default(),
    ).await;
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetInvulnerable");

    // Source ECM jammed -> ShipJammed
    let _ = sys.set_combat_state(target_ship_id, CombatState::default()).await;
    let _ = sys.set_combat_state(ship_id, CombatState { ecm_jammed_remaining_s: 5.0, ..Default::default() }).await;
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "ShipJammed");

    // Source cloaked -> TargetingFailed
    let _ = sys.set_combat_state(ship_id, CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 0, ..Default::default() }).await;
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");

    // Target beyond maxTargetRange -> TargetingFailed
    let _ = sys.set_combat_state(ship_id, CombatState::default()).await;
    let _ = sys.set_ball_position(target_ship_id, DVec3::new(500_000.0, 0.0, 0.0)).await;
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");

    // RemoveTargets with List and single Integer
    let _ = sys.set_ball_position(target_ship_id, DVec3::new(2_000.0, 0.0, 0.0)).await;
    for arg in [
        PyRep::List(vec![PyRep::Integer(target_ship_id)]),
        PyRep::Integer(target_ship_id),
    ] {
        let _ = sys.add_target_lock(ship_id, TargetLock {
            target_id: target_ship_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
        }).await;
        call_dogma(&ctx, &mut session, "RemoveTargets", &[arg], &kw).await.unwrap().unwrap();
        assert!(sys.get_target_locks(ship_id).await.unwrap().is_empty());
    }

    assert_eq!(call_dogma(&ctx, &mut session, "CancelAddTarget", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[PyRep::Integer(0)], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[PyRep::Integer(999_888), PyRep::Integer(target_ship_id)], &kw).await.unwrap().unwrap_err().code, "ShipOwnershipViolation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attribute_access_control_online_capacitor_repair_and_drone_settings_branches() {
    let (ctx, mut session, char_id, ship_id, target_ship_id, sys_id) = setup_dogma_ctx();
    let kw = HashMap::new();
    let container_id = 96_300_500_i64;
    let nested_item_id = 96_300_501_i64;
    let drone_ball_id = 96_300_600_i64;
    let mod_undamaged = 96_300_701_i64;
    let mod_destroyed = 96_300_702_i64;
    let mod_ehe = 96_300_703_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id, type_id: 23, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
            },
        );
        snap.items.insert(
            nested_item_id,
            InventoryItemRecord {
                item_id: nested_item_id, type_id: 484, owner_id: 999_999, location_id: container_id,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true,
                dynamic_attributes: HashMap::from([(999, 42.5)]), ..Default::default()
            },
        );
        snap.transient_item_attributes.entry(ship_id).or_default().insert(1175, 77.5);
        for (id, tid, flag, dmg) in [
            (mod_undamaged, 484_u32, InventoryFlag::HiSlot0, 0.0),
            (mod_destroyed, 484_u32, InventoryFlag::HiSlot1, 40.0),
            (mod_ehe, 40714_u32, InventoryFlag::LoSlot0, 10.0),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id, type_id: tid, owner_id: char_id, location_id: ship_id, flag,
                    quantity: 1, singleton: true,
                    dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, dmg), (attr_ids::HP_STRUCT, 40.0)]),
                    ..Default::default()
                },
            );
        }
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball_id,
            DroneSpaceState {
                ball_id: drone_ball_id,
                drone_item_id: drone_ball_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: Some(target_ship_id),
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1_000.0,
                optimal_range: 5_000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    assert_eq!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(0), PyRep::Integer(3)], &kw).await.unwrap().unwrap_err().code, "AccessDenied");
    assert_eq!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(target_ship_id), PyRep::Integer(3)], &kw).await.unwrap().unwrap_err().code, "AccessDenied");
    assert_eq!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(nested_item_id), PyRep::Integer(999)], &kw).await.unwrap().unwrap(), PyRep::Real(42.5));
    assert_eq!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(ship_id), PyRep::Integer(1175)], &kw).await.unwrap().unwrap(), PyRep::Real(77.5));
    assert!(call_dogma(&ctx, &mut session, "QueryAllAttributesForItem", &[PyRep::Integer(nested_item_id)], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "FullyDescribeAttribute", &[PyRep::Integer(drone_ball_id), PyRep::Integer(65_000)], &kw).await.unwrap().is_ok());

    session.role = eve_machonet::service_roles::ROLE_GMS;
    assert!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(target_ship_id), PyRep::Integer(9)], &kw).await.unwrap().is_ok());
    session.role = 0;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        CombatState { capacitor: 50.0, max_capacitor: 100.0, ..Default::default() },
        SlimItemMeta::default(),
    ).await;
    assert_eq!(call_dogma(&ctx, &mut session, "ShipOnlineModules", &[], &kw).await.unwrap().unwrap_err().code, "NotEnoughCapacitorToOnline");

    session.station_id = Some(60_003_760);
    assert_eq!(call_dogma(&ctx, &mut session, "InitiateModuleRepairMany", &[], &kw).await.unwrap().unwrap_err().code, "CannotRepairNaniteWhileDocked");
    session.station_id = None;

    let rep_tuple = call_dogma(
        &ctx, &mut session, "InitiateModuleRepairMany",
        &[PyRep::Tuple(vec![PyRep::Integer(mod_undamaged), PyRep::Integer(mod_destroyed), PyRep::Integer(mod_ehe)])],
        &kw,
    ).await.unwrap().unwrap();
    if let PyRep::Tuple(parts) = rep_tuple {
        assert_eq!(parts[0], PyRep::List(vec![]));
        assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(mod_destroyed), PyRep::Integer(mod_ehe)]));
        assert_eq!(parts[2], PyRep::List(vec![PyRep::Integer(mod_undamaged)]));
    } else {
        panic!("expected tuple");
    }

    assert_eq!(call_dogma(&ctx, &mut session, "ChangeDroneSettings", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_dogma(&ctx, &mut session, "ChangeDroneSettings", &[PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(0)])], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    call_dogma(&ctx, &mut session, "ChangeDroneSettings", &[PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0)])], &kw).await.unwrap().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.combat_crimewatch_state.drone_settings.get(&char_id).copied()), Some((true, false)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogma_module_queries_nanite_repair_success_and_drone_kwargs() {
    let (ctx, mut session, char_id, _other_id, _prev_ship_id, sys_id) = setup_dogma_ctx();
    let kw = HashMap::new();
    let ship_id = 96_305_001_i64;
    let mod_id = 96_305_002_i64;
    let hangar_item_id = 96_305_003_i64;
    let paste_id = 96_305_004_i64;

    // Pick a real module type from SDE (e.g., 16278 or first category_id == 7 with default_attributes)
    let mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.published && !t.default_attributes.is_empty() && !t.effect_ids.contains(&6484))
        .map(|t| t.type_id)
        .unwrap_or(527);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut ship = InventoryItemRecord {
            item_id: ship_id,
            type_id: 587,
            owner_id: char_id,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        ship.dynamic_attributes.insert(1175, 42.5);
        snap.items.insert(ship_id, ship);

        let mut fitted = InventoryItemRecord {
            item_id: mod_id,
            type_id: mod_tid,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        fitted.dynamic_attributes.insert(attr_ids::DAMAGE, 8.0);
        fitted.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
        snap.items.insert(mod_id, fitted);

        snap.items.insert(
            hangar_item_id,
            InventoryItemRecord {
                item_id: hangar_item_id,
                type_id: mod_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);
    session.station_id = None;
    session.structure_id = None;

    // QueryAttributeValue on ship heat (1175), fitted module DAMAGE, HP_STRUCT, and SDE default attr
    let heat_val = call_dogma(
        &ctx,
        &mut session,
        "QueryAttributeValue",
        &[PyRep::Integer(ship_id), PyRep::Integer(1175)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(heat_val, PyRep::Real(42.5));

    let dmg_val = call_dogma(
        &ctx,
        &mut session,
        "QueryAttributeValue",
        &[PyRep::Integer(mod_id), PyRep::Integer(i64::from(attr_ids::DAMAGE))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(dmg_val, PyRep::Real(8.0));

    let hp_val = call_dogma(
        &ctx,
        &mut session,
        "QueryAttributeValue",
        &[PyRep::Integer(mod_id), PyRep::Integer(i64::from(attr_ids::HP_STRUCT))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(hp_val, PyRep::Real(40.0));

    // QueryAllAttributesForItem on fitted module and non-fitted hangar item
    let mod_attrs = call_dogma(
        &ctx,
        &mut session,
        "QueryAllAttributesForItem",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(mod_attrs, PyRep::Dict(d) if !d.is_empty()));

    let hangar_attrs = call_dogma(
        &ctx,
        &mut session,
        "QueryAllAttributesForItem",
        &[PyRep::Integer(hangar_item_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(hangar_attrs, PyRep::Dict(d) if !d.is_empty()));

    // Spawn ball and run InitiateModuleRepairMany with scalar mod_id (happy path + duplicate active repair)
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 100.0,
                max_capacitor: 100.0,
                ..Default::default()
            },
            SlimItemMeta::default(),
        )
        .await;

    let rep_ok = call_dogma(
        &ctx,
        &mut session,
        "InitiateModuleRepairMany",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::Tuple(parts) = rep_ok {
        assert_eq!(parts[0], PyRep::List(vec![PyRep::Integer(mod_id)]));
        assert_eq!(parts[1], PyRep::List(vec![]));
        assert_eq!(parts[2], PyRep::List(vec![]));
    } else {
        panic!("expected tuple");
    }
    assert!(sys.has_active_nanite_repair_sync(ship_id, mod_id));

    // Re-calling while repair is already active puts mod_id into failed
    let rep_dup = call_dogma(
        &ctx,
        &mut session,
        "InitiateModuleRepairMany",
        &[PyRep::List(vec![PyRep::Integer(mod_id)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::Tuple(parts) = rep_dup {
        assert_eq!(parts[1], PyRep::List(vec![PyRep::Integer(mod_id)]));
    } else {
        panic!("expected tuple");
    }

    // ChangeDroneSettings via kwargs and invalid focusFire type
    let mut drone_kw = HashMap::new();
    drone_kw.insert("aggressive".to_string(), PyRep::Bool(false));
    drone_kw.insert("focusFire".to_string(), PyRep::String("bad".into()));
    assert_eq!(
        call_dogma(&ctx, &mut session, "ChangeDroneSettings", &[], &drone_kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    drone_kw.insert("focusFire".to_string(), PyRep::Bool(true));
    call_dogma(&ctx, &mut session, "ChangeDroneSettings", &[], &drone_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.combat_crimewatch_state.drone_settings.get(&char_id).copied()),
        Some((false, true))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogma_target_limits_cancel_remove_overheat_repair_and_attr_branches() {
    let (ctx, mut session, char_id, other_id, _prev_ship_id, sys_id) = setup_dogma_ctx();
    let kw = HashMap::new();
    let ship_id = 96_306_001_i64;
    let mod_id = 96_306_002_i64;
    let mod_ehe = 96_306_003_i64;
    let hangar_item = 96_306_004_i64;
    let cont_id = 96_306_005_i64;
    let inside_item = 96_306_006_i64;
    let t_ship1 = 96_306_101_i64;
    let t_ship2 = 96_306_102_i64;
    let mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.published && !t.default_attributes.is_empty() && !t.effect_ids.contains(&6484))
        .map(|t| t.type_id)
        .unwrap_or(527);
    let ehe_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.effect_ids.contains(&6484))
        .map(|t| t.type_id)
        .unwrap_or(40714);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::MAX_LOCKED_TARGETS, 1.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 12.0), (1175, 55.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_ehe,
            InventoryItemRecord {
                item_id: mod_ehe,
                type_id: ehe_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 23,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            inside_item,
            InventoryItemRecord {
                item_id: inside_item,
                type_id: 34,
                owner_id: other_id,
                location_id: cont_id,
                flag: InventoryFlag::None,
                quantity: 10,
                dynamic_attributes: HashMap::from([(999, 8.5)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            hangar_item,
            InventoryItemRecord {
                item_id: hangar_item,
                type_id: mod_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for tid_item in [t_ship1, t_ship2] {
            snap.items.insert(
                tid_item,
                InventoryItemRecord {
                    item_id: tid_item,
                    type_id: 587,
                    owner_id: other_id,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    session.ship_id = Some(ship_id);
    session.station_id = None;
    session.structure_id = None;

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, x) in [(ship_id, 0.0), (t_ship1, 5_000.0), (t_ship2, 6_000.0)] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: 587,
                    position: DVec3::new(x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1e6,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState { capacitor: 1_000.0, max_capacitor: 1_000.0, ..Default::default() },
                SlimItemMeta::default(),
            )
            .await;
    }

    // AddTargetOBO hits target owned by other_id, then hitting max_locked_targets fails
    assert!(
        call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ship1)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ship2)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetingFailed"
    );

    // CancelAddTarget missing char, NoActiveShip, InvalidArgument, valid
    let mut no_char = SessionState::new(1);
    assert_eq!(call_dogma(&ctx, &mut no_char, "CancelAddTarget", &[], &kw).await.unwrap().unwrap_err().code, "Unauthorized");
    assert_eq!(call_dogma(&ctx, &mut session, "CancelAddTarget", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert!(call_dogma(&ctx, &mut session, "CancelAddTarget", &[PyRep::Integer(t_ship1)], &kw).await.unwrap().is_ok());

    // RemoveTargetOBO + RemoveTargets variants (list, single id, clear-all)
    assert_eq!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[PyRep::Integer(t_ship2), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "ShipOwnershipViolation");
    assert!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ship1)], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "RemoveTargets", &[PyRep::List(vec![PyRep::Integer(t_ship1)])], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "RemoveTargets", &[PyRep::Integer(t_ship1)], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "RemoveTargets", &[], &kw).await.unwrap().is_ok());

    // ShipOnlineModules error paths (NoActiveShip, station without fitting service)
    let no_fit_station = ctx
        .sde
        .stations
        .iter()
        .find(|(&id, _)| !crate::services::station_structure::npc_station_offers_service(&ctx.sde, id, 17))
        .map(|(&id, _)| id);
    if let Some(st_id) = no_fit_station {
        session.station_id = Some(st_id);
        assert_eq!(call_dogma(&ctx, &mut session, "ShipOnlineModules", &[], &kw).await.unwrap().unwrap_err().code, "StationServiceUnavailable");
        session.station_id = None;
    }
    session.ship_id = None;
    assert_eq!(call_dogma(&ctx, &mut session, "ShipOnlineModules", &[], &kw).await.unwrap().unwrap_err().code, "NoActiveShip");
    assert_eq!(call_dogma(&ctx, &mut session, "CancelAddTarget", &[PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "NoActiveShip");
    assert_eq!(call_dogma(&ctx, &mut session, "RemoveTargets", &[], &kw).await.unwrap().unwrap_err().code, "NoActiveShip");
    assert_eq!(call_dogma(&ctx, &mut session, "InitiateModuleRepairMany", &[], &kw).await.unwrap().unwrap_err().code, "NoActiveShip");
    session.ship_id = Some(ship_id);

    // QueryAttributeValue / QueryAllAttributesForItem across module, ship, cargo item, and container-owned item
    assert_eq!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(mod_id), PyRep::Integer(i64::from(attr_ids::DAMAGE))], &kw).await.unwrap().unwrap(), PyRep::Real(12.0));
    assert!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(mod_id), PyRep::Integer(i64::from(attr_ids::HP_STRUCT))], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(mod_id), PyRep::Integer(50)], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(hangar_item), PyRep::Integer(50)], &kw).await.unwrap().is_ok());
    assert_eq!(call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(inside_item), PyRep::Integer(999)], &kw).await.unwrap().unwrap(), PyRep::Real(8.5));
    assert!(call_dogma(&ctx, &mut session, "QueryAllAttributesForItem", &[PyRep::Integer(mod_id)], &kw).await.unwrap().is_ok());
    assert!(call_dogma(&ctx, &mut session, "QueryAllAttributesForItem", &[PyRep::Integer(hangar_item)], &kw).await.unwrap().is_ok());

    // Overheating blocks InitiateModuleRepairMany
    let _ = sys
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: mod_id,
            duration_s: 5.0,
            repeat: -1,
            is_overloaded: true,
            ..crate::system_actor::ActiveModuleCycle::default()
        })
        .await;
    assert_eq!(
        call_dogma(&ctx, &mut session, "InitiateModuleRepairMany", &[PyRep::Integer(mod_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotRepairWhileOverheating"
    );

    // AddTargetOBO authorization, self-target, missing sys/ball, online module attr & drone OBO branches
    assert_eq!(call_dogma(&ctx, &mut no_char, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "Unauthorized");
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(0), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(ship_id)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(t_ship2), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "ShipOwnershipViolation");
    session.solar_system_id = None;
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap(), PyRep::None);
    session.solar_system_id = Some(sys_id);
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(999_777)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");

    let drone_id = 96_306_999_i64;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(drone_id, DroneSpaceState {
            ball_id: drone_id,
            drone_item_id: drone_id,
            type_id: 2454,
            group_id: 100,
            owner_char_id: char_id,
            original_owner_char_id: char_id,
            parent_ship_id: ship_id,
            solar_system_id: sys_id,
            target_id: Some(t_ship1),
            activity_state: 1,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 1_000.0,
            optimal_range: 5_000.0,
            damage_vector: Default::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 1.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        });
    });
    assert_eq!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(drone_id), PyRep::Integer(t_ship1)], &kw).await.unwrap().unwrap_err().code, "TargetingFailed");
    assert!(call_dogma(&ctx, &mut session, "RemoveTargetOBO", &[PyRep::Integer(drone_id), PyRep::Integer(t_ship1)], &kw).await.unwrap().is_ok());

    // Online mod_id & query module attributes through fit.modules branch
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });
    let _ = call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(mod_id), PyRep::Integer(i64::from(attr_ids::HP_STRUCT))], &kw).await;
    let _ = call_dogma(&ctx, &mut session, "QueryAttributeValue", &[PyRep::Integer(mod_id), PyRep::Integer(50)], &kw).await;
    let _ = call_dogma(&ctx, &mut session, "QueryAllAttributesForItem", &[PyRep::Integer(mod_id)], &kw).await;
}

#[test]
fn resolve_parent_ship_and_evaluate_drone_or_probe_state_with_overrides_pin_all_branches() {
    let (ctx, _session, char_id, ship_id, target_ship_id, sys_id) = setup_dogma_ctx();
    let other_ship_mod = 96_309_001_i64;
    let probe_id = 96_309_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            other_ship_mod,
            InventoryItemRecord {
                item_id: other_ship_mod,
                type_id: 484,
                owner_id: 96_300_002,
                location_id: target_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // 1. Item itself is a ship (category 6) -> returns (item.owner_id, item.item_id)
    let (_, p_ship) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_id, target_ship_id);
    assert_eq!(p_ship, Some((96_300_002, target_ship_id)));

    // 2. Item located inside another ship (loc_cat == 6) when active_ship_id != target_ship_id
    let (_, p_loc_ship) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_id, other_ship_mod);
    assert_eq!(p_loc_ship, Some((96_300_002, target_ship_id)));

    // 3. Active scanner probe lookup when item is not in db.items
    ctx.active_scanner_probes.write().unwrap().insert(
        probe_id,
        crate::router::ActiveScannerProbe {
            probe_id,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 1,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1e11,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 1,
            state: 1,
            warp_end_ft: None,
        },
    );
    let (_, p_probe) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, 0, probe_id);
    assert_eq!(p_probe, Some((char_id, ship_id)));

    // 4. Fallback when unknown item: active_ship_id > 0 -> Some((char_id, active_ship_id)), active_ship_id == 0 -> None
    assert_eq!(
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_id, 999_999_888).1,
        Some((char_id, ship_id))
    );
    assert_eq!(
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, 0, 999_999_888).1,
        None
    );

    // 5. evaluate_drone_or_probe_state_with_overrides: dynamic_attributes filter (3,18,265,266 excluded) + triage (27951/4294) zeroes attr 64
    let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let drone_id = 96_309_010_i64;
    fit.add_drone(drone_id, 2454, &ctx.sde);
    let item_rec = InventoryItemRecord {
        item_id: drone_id,
        type_id: 2454,
        owner_id: char_id,
        location_id: ship_id,
        flag: InventoryFlag::DroneBay,
        quantity: 1,
        singleton: true,
        dynamic_attributes: HashMap::from([
            (3, 999.0),
            (18, 999.0),
            (265, 999.0),
            (266, 999.0),
            (64, 2.5),
            (500, 77.0),
        ]),
        ..Default::default()
    };
    let st_no_triage = super::super::evaluate_drone_or_probe_state_with_overrides(
        &ctx,
        &fit,
        drone_id,
        Some(&item_rec),
    )
    .expect("drone state");
    assert_eq!(st_no_triage.base_attributes.get(&500), Some(&77.0));
    assert_eq!(st_no_triage.base_attributes.get(&64), Some(&2.5));
    assert_ne!(st_no_triage.base_attributes.get(&3), Some(&999.0));

    // Fit an active Triage Module I (27951) and verify attr 64 is zeroed
    fit.modules.insert(
        96_309_011,
        eve_dogma::FittedModule {
            item_id: 96_309_011,
            type_id: 27951,
            flag_id: 27,
            state: eve_dogma::ModuleState::Active,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    let st_with_triage = super::super::evaluate_drone_or_probe_state_with_overrides(
        &ctx,
        &fit,
        drone_id,
        Some(&item_rec),
    )
    .expect("drone state with triage");
    assert_eq!(st_with_triage.base_attributes.get(&64), Some(&0.0));
}

#[test]
fn resolve_parent_ship_full_tuple_and_triage_overloaded_modifier_retention() {
    let (ctx, _session, char_id, ship_id, target_ship_id, sys_id) = setup_dogma_ctx();
    let ship_ghost_id = 96_310_100_i64;
    let fitted_on_ghost_id = 96_310_101_i64;
    let cont_in_ship_id = 96_310_102_i64;
    let nested_item_id = 96_310_103_i64;
    let hangar_item_id = 96_310_104_i64;
    let zero_loc_item_id = 96_310_105_i64;
    let drone_ball_id = 96_310_111_i64;
    let drone_item_id = 96_310_112_i64;
    let other_char_id = 96_300_002_i64;
    let drone_owner_id = 96_310_999_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Point character's active_ship_id to ship_ghost_id which is NOT in snap.items (exercises L943)
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_ghost_id;
        snap.characters.get_mut(&other_char_id).unwrap().active_ship_id = 0;

        snap.items.insert(
            fitted_on_ghost_id,
            InventoryItemRecord {
                item_id: fitted_on_ghost_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_ghost_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            zero_loc_item_id,
            InventoryItemRecord {
                item_id: zero_loc_item_id,
                type_id: 484,
                owner_id: other_char_id,
                location_id: 0,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Nested item: nested_item -> cont_in_ship -> target_ship_id (target_ship_id is in snap.items)
        snap.items.insert(
            cont_in_ship_id,
            InventoryItemRecord {
                item_id: cont_in_ship_id,
                type_id: 23,
                owner_id: other_char_id,
                location_id: target_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            nested_item_id,
            InventoryItemRecord {
                item_id: nested_item_id,
                type_id: 34,
                owner_id: other_char_id,
                location_id: cont_in_ship_id,
                flag: InventoryFlag::None,
                quantity: 5,
                ..Default::default()
            },
        );
        snap.items.insert(
            hangar_item_id,
            InventoryItemRecord {
                item_id: hangar_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                ..Default::default()
            },
        );
    });

    // Drone absent from snap.items with distinct ball_id vs drone_item_id
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball_id,
            DroneSpaceState {
                ball_id: drone_ball_id,
                drone_item_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: drone_owner_id,
                original_owner_char_id: drone_owner_id,
                parent_ship_id: target_ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 400.0,
                optimal_range: 5_000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    // Assert BOTH tuple items across every branch (kills all 20 FnValue mutants + BinOp)
    let (item_ship, db_ship) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_id, ship_id);
    assert_eq!(item_ship.as_ref().map(|i| i.item_id), Some(ship_id));
    assert_eq!(db_ship, Some((char_id, ship_id)));

    let (item_ghost, db_ghost) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_ghost_id, fitted_on_ghost_id);
    assert_eq!(item_ghost.as_ref().map(|i| i.item_id), Some(fitted_on_ghost_id));
    assert_eq!(db_ghost, Some((char_id, ship_ghost_id)));

    let (item_zero, db_zero) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, other_char_id, 0, zero_loc_item_id);
    assert_eq!(item_zero.as_ref().map(|i| i.item_id), Some(zero_loc_item_id));
    assert_eq!(db_zero, None);

    let (item_nested, db_nested) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, other_char_id, 0, nested_item_id);
    assert_eq!(item_nested.as_ref().map(|i| i.item_id), Some(nested_item_id));
    assert_eq!(db_nested, Some((other_char_id, target_ship_id)));

    let (item_hangar, db_hangar) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_id, hangar_item_id);
    assert_eq!(item_hangar.as_ref().map(|i| i.item_id), Some(hangar_item_id));
    assert_eq!(db_hangar, None);

    let (item_dball, db_dball) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, drone_owner_id, 0, drone_ball_id);
    assert!(item_dball.is_none());
    assert_eq!(db_dball, Some((drone_owner_id, target_ship_id)));

    let (item_ditem, db_ditem) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, drone_owner_id, 0, drone_item_id);
    assert!(item_ditem.is_none());
    assert_eq!(db_ditem, Some((drone_owner_id, target_ship_id)));

    let (item_miss, db_miss) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, ship_id, 999_888_777);
    assert!(item_miss.is_none());
    assert_eq!(db_miss, Some((char_id, ship_id)));

    let (item_miss_zero, db_miss_zero) =
        super::super::resolve_parent_ship_for_dogma_item(&ctx, char_id, 0, 999_888_777);
    assert!(item_miss_zero.is_none());
    assert!(db_miss_zero.is_none());

    // evaluate_drone_or_probe_state_with_overrides: triage 4294 Overloaded vs Online, retains != 64 modifier
    let mut fit = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("ship fit");
    fit.modules.insert(
        96_310_201,
        eve_dogma::FittedModule {
            item_id: 96_310_201,
            type_id: 4294,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: eve_dogma::ModuleState::Overloaded,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    let mut rec_with_64 = InventoryItemRecord {
        item_id: 96_310_202,
        type_id: 2454,
        owner_id: char_id,
        location_id: ship_id,
        flag: InventoryFlag::DroneBay,
        quantity: 1,
        singleton: true,
        dynamic_attributes: HashMap::from([(64, 150.0)]),
        ..Default::default()
    };
    let mut base_sde = (*ctx.sde).clone();
    // Apply two OwnerRequiredSkillModifier effects (target attr 64 and attr 50) onto Hobgoblin I (2454)
    // via a trained synthetic skill so evaluate_drone_or_probe_state_with_overrides retains non-64 while stripping 64 under overloaded triage.
    let synth_skill_tid = 96_310_998_u32;
    let synth_effect_id = (60_000..=65_530)
        .find(|id| !base_sde.effects.contains_key(id))
        .expect("free synthetic effect id");
    if let Some(t) = base_sde.types.get_mut(&2454) {
        t.default_attributes.insert(64, 150.0);
        t.default_attributes.insert(1285, f64::from(synth_skill_tid));
    }
    if let Some(skill_tmpl) = base_sde.types.get(&3429).cloned() {
        let mut st = skill_tmpl;
        st.type_id = synth_skill_tid;
        st.category_id = 16;
        st.effect_ids = vec![synth_effect_id];
        st.default_attributes.clear();
        st.default_attributes.insert(280, 5.0);
        base_sde.types.insert(synth_skill_tid, st);
    }
    base_sde.effects.insert(
        synth_effect_id,
        eve_sde::DogmaEffectType {
            effect_id: synth_effect_id,
            name: "testDroneAttrModifiers".to_string(),
            effect_category: 0,
            modifiers: vec![
                eve_sde::EffectModifierSpec {
                    domain: "charID".to_string(),
                    func: "OwnerRequiredSkillModifier".to_string(),
                    modified_attribute_id: 64,
                    modifying_attribute_id: 280,
                    operator: 6,
                    group_id: None,
                    skill_type_id: Some(synth_skill_tid),
                    effect_id: None,
                },
                eve_sde::EffectModifierSpec {
                    domain: "charID".to_string(),
                    func: "OwnerRequiredSkillModifier".to_string(),
                    modified_attribute_id: 50,
                    modifying_attribute_id: 280,
                    operator: 6,
                    group_id: None,
                    skill_type_id: Some(synth_skill_tid),
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    let sde_arc = Arc::new(base_sde);
    let ctx_custom = ServerContext::with_sde_and_repository(sde_arc, Arc::clone(&ctx.db));
    let mut probe_fit = fit.clone();
    probe_fit.character_skills.clear();
    probe_fit.character_skills.insert(synth_skill_tid, 5);
    probe_fit.add_drone(96_310_202, 2454, &ctx_custom.sde);
    let st_overloaded = super::super::evaluate_drone_or_probe_state_with_overrides(
        &ctx_custom,
        &probe_fit,
        96_310_202,
        Some(&rec_with_64),
    )
    .expect("overloaded triage drone");
    assert_eq!(st_overloaded.base_attributes.get(&64), Some(&0.0));
    // Triage II (4294) active effect adds 4 non-64 drone penalty modifiers + synth skill adds attr 50 -> 5 non-64 retained, all attr 64 stripped
    assert_eq!(st_overloaded.modifiers.len(), 5);
    assert!(!st_overloaded.modifiers.iter().any(|m| m.target_attribute_id == 64));
    assert!(st_overloaded.modifiers.iter().any(|m| m.target_attribute_id == 50));

    // Offline/Online (not Active/Overloaded) triage does NOT zero attr 64
    probe_fit.modules.get_mut(&96_310_201).unwrap().state = eve_dogma::ModuleState::Online;
    probe_fit.modules.get_mut(&96_310_201).unwrap().type_id = 27951;
    let st_online = super::super::evaluate_drone_or_probe_state_with_overrides(
        &ctx_custom,
        &probe_fit,
        96_310_202,
        Some(&rec_with_64),
    )
    .expect("online triage drone");
    assert_eq!(st_online.base_attributes.get(&64), Some(&150.0));
    assert_eq!(st_online.modifiers.len(), 2);

    // Overloaded triage when drone type does NOT contain attr 64 -> must NOT insert key 64
    rec_with_64.dynamic_attributes.clear();
    let mut sde_no64 = (*ctx.sde).clone();
    if let Some(t) = sde_no64.types.get_mut(&2454) {
        t.default_attributes.remove(&64);
    }
    let ctx_no64 = ServerContext::with_sde_and_repository(Arc::new(sde_no64), Arc::clone(&ctx.db));
    probe_fit.modules.get_mut(&96_310_201).unwrap().state = eve_dogma::ModuleState::Overloaded;
    let st_no64 = super::super::evaluate_drone_or_probe_state_with_overrides(
        &ctx_no64,
        &probe_fit,
        96_310_202,
        Some(&rec_with_64),
    )
    .expect("no64 drone");
    assert!(!st_no64.base_attributes.contains_key(&64));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogma_ops_lock_pruning_ship_online_drone_settings_and_attr_evaluation_tree() {
    // Build a deterministic custom SDE with ship modifiers, turret, launcher, charge, and drone attrs
    let mut sde = (*shared_sde()).clone();
    let ship_tid = 587u32;
    let mod_online_tid = 96_311_101_u32;
    let mod_offline_tid = 96_311_102_u32;
    let launcher_tid = 96_311_103_u32;
    let charge_tid = 96_311_104_u32;
    let ship_boost_eff = 59_901_u16;

    sde.effects.insert(
        ship_boost_eff,
        eve_sde::DogmaEffectType {
            effect_id: ship_boost_eff,
            name: "shipBoostMaxTargetRange".into(),
            effect_category: 0, // passive when fitted/online
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_ids::MAX_TARGET_RANGE,
                modifying_attribute_id: 20, // Speed Factor / flat value from module attr 20
                operator: 7, // PostAssign
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    if let Some(ship_t) = sde.types.get_mut(&ship_tid) {
        ship_t.default_attributes.insert(attr_ids::MAX_TARGET_RANGE, 40_000.0);
        ship_t.default_attributes.insert(attr_ids::MAX_LOCKED_TARGETS, 3.0);
        ship_t.default_attributes.insert(attr_ids::CPU_OUTPUT, 5_000.0);
        ship_t.default_attributes.insert(attr_ids::POWER_OUTPUT, 5_000.0);
        ship_t.default_attributes.insert(attr_ids::HI_SLOTS, 4.0);
        ship_t.default_attributes.insert(attr_ids::LOW_SLOTS, 4.0);
    }
    let mut mod_t = sde.types.get(&527).unwrap().clone();
    mod_t.type_id = mod_online_tid;
    mod_t.category_id = 7;
    mod_t.published = true;
    mod_t.default_attributes = HashMap::from([
        (2, 0.0),
        (20, 50_000.0),
        (54, 10_000.0),
        (158, 5_000.0),
        (160, 0.5),
        (attr_ids::CPU, 10.0),
        (attr_ids::POWER, 10.0),
        (attr_ids::HP_STRUCT, 45.0),
    ]);
    mod_t.effect_ids = vec![12, 16, ship_boost_eff];
    sde.types.insert(mod_online_tid, mod_t.clone());

    let mut offline_t = mod_t.clone();
    offline_t.type_id = mod_offline_tid;
    offline_t.effect_ids = vec![11, 16];
    sde.types.insert(mod_offline_tid, offline_t);

    let mut launch_t = mod_t.clone();
    launch_t.type_id = launcher_tid;
    launch_t.default_attributes = HashMap::from([
        (attr_ids::CPU, 5.0),
        (attr_ids::POWER, 5.0),
        (attr_ids::CAPACITY, 100.0),
        (attr_ids::CHARGE_SIZE, 1.0),
    ]);
    launch_t.effect_ids = vec![12, 16];
    sde.types.insert(launcher_tid, launch_t);

    let mut chg_t = sde.types.get(&34).unwrap().clone();
    chg_t.type_id = charge_tid;
    chg_t.category_id = 8;
    chg_t.group_id = 384;
    chg_t.volume = 0.01;
    chg_t.default_attributes = HashMap::from([
        (attr_ids::EM_DAMAGE, 123.0),
        (attr_ids::CHARGE_SIZE, 1.0),
    ]);
    sde.types.insert(charge_tid, chg_t);

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_311_001_i64;
    let other_char_id = 96_311_002_i64;
    let ship_id = 96_311_100_i64;
    let other_ship_id = 96_311_101_i64;
    let sys_id = 30_000_142_u32;
    let mod_online_id = 96_311_201_i64;
    let mod_offline_id = 96_311_202_i64;
    let mod_to_online_id = 96_311_203_i64;
    let mod_other_ship_id = 96_311_204_i64;
    let cargo_mod_id = 96_311_205_i64;
    let launcher_id = 96_311_206_i64;
    let loaded_charge_id = 96_311_207_i64;
    let cargo_charge_id = 96_311_208_i64;
    let hangar_item_id = 96_311_209_i64;
    let t_cloaked = 96_311_301_i64;
    let t_warp_inv = 96_311_302_i64;
    let t_warping = 96_311_303_i64;
    let t_far = 96_311_304_i64;
    let t_edge = 96_311_305_i64;
    let t_ok1 = 96_311_306_i64;
    let t_ok2 = 96_311_307_i64;
    let t_ok3 = 96_311_308_i64;
    let probe_mine = 96_311_401_i64;
    let probe_other = 96_311_402_i64;
    let drone_ball_id = 96_311_501_i64;
    let drone_item_id = 96_311_502_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, sid) in &[(char_id, ship_id), (other_char_id, other_ship_id)] {
            snap.accounts.insert(
                format!("u{cid}"),
                AccountRecord {
                    user_id: cid as u64,
                    username: format!("u{cid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            snap.characters.insert(cid, eve_db::CharacterRecord {
                activity_status: None,
                character_id: cid,
                user_id: cid as u64,
                name: format!("C{cid}"),
                corporation_id: 1_000_009,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: sid,
                balance: eve_db::IskCents(0),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 11,
                neural: eve_db::NeuralAttributes {
                    charisma: 20,
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    bonus_remaps: 2,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 0,
            });
            snap.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: ship_tid, owner_id: cid, location_id: i64::from(sys_id),
                flag: InventoryFlag::None, quantity: 1, singleton: true,
                dynamic_attributes: if sid == ship_id {
                    HashMap::from([
                        (attr_ids::MAX_TARGET_RANGE, 1.0), // overridden by dogma ship modifier!
                        (265, 77.5),
                        (1175, 44.5),
                        (555, 88.5), // non-modifier dynamic attr preserved by !fit.ship.modifiers.iter().any
                    ])
                } else {
                    HashMap::new()
                },
                ..Default::default()
            });
        }
        snap.transient_item_attributes.entry(ship_id).or_default().insert(777, 99.5);
        snap.skills.entry(char_id).or_default().insert(
            3429,
            eve_db::TrainedSkillRecord {
                type_id: 3429,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(mod_online_id, InventoryItemRecord {
            item_id: mod_online_id, type_id: mod_online_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true,
            dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 7.25), (attr_ids::HP_STRUCT, 45.0)]),
            ..Default::default()
        });
        snap.module_online_states.insert(mod_online_id, true);
        snap.items.insert(mod_offline_id, InventoryItemRecord {
            item_id: mod_offline_id, type_id: mod_offline_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true,
            dynamic_attributes: HashMap::from([(556, 33.5)]),
            ..Default::default()
        });
        snap.module_online_states.insert(mod_offline_id, false);
        snap.items.insert(mod_to_online_id, InventoryItemRecord {
            item_id: mod_to_online_id, type_id: mod_offline_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::LoSlot1, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(mod_to_online_id, false);
        snap.items.insert(mod_other_ship_id, InventoryItemRecord {
            item_id: mod_other_ship_id, type_id: mod_offline_tid, owner_id: char_id, location_id: other_ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(cargo_mod_id, InventoryItemRecord {
            item_id: cargo_mod_id, type_id: mod_offline_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(launcher_id, InventoryItemRecord {
            item_id: launcher_id, type_id: launcher_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(loaded_charge_id, InventoryItemRecord {
            item_id: loaded_charge_id, type_id: charge_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot1, quantity: 20, singleton: false, ..Default::default()
        });
        snap.items.insert(cargo_charge_id, InventoryItemRecord {
            item_id: cargo_charge_id, type_id: charge_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
        });
        snap.items.insert(hangar_item_id, InventoryItemRecord {
            item_id: hangar_item_id, type_id: charge_tid, owner_id: char_id, location_id: 60_003_760,
            flag: InventoryFlag::Hangar, quantity: 5, singleton: false, ..Default::default()
        });
    });

    // Projected remote tracking computer onto ship_id -> boosts online module 54/158/160 but NOT offline module!
    let dbk = crate::services::tactical_modules::db_ptr_key(&ctx.db);
    ctx.tactical_state.write().unwrap().remote_assistance.insert(
        (dbk, ship_id),
        vec![crate::services::tactical_modules::ProjectedRemoteAssistance {
            source_ship_id: other_ship_id,
            module_id: 96_311_800,
            module_type_id: 1964,
            max_target_range_bonus_pct: 0.0,
            scan_resolution_bonus_pct: 0.0,
            sensor_strength_bonus_pct: 0.0,
            optimal_range_bonus_pct: 25.0,
            falloff_bonus_pct: 50.0,
            tracking_speed_bonus_pct: 20.0,
        }],
    );

    // 1. Verify get_item_attribute_value & get_all_item_attributes across ship, online/offline module, charges, drone, fallback
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, ship_id, 777), 99.5);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, ship_id, 1175), 44.5);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, ship_id, 265), 77.5);
    let eval_range = super::super::get_item_attribute_value(&ctx, char_id, ship_id, ship_id, attr_ids::MAX_TARGET_RANGE);
    assert_eq!(eval_range, 50_000.0);
    let ship_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, ship_id);
    assert_eq!(ship_all.get(&attr_ids::MAX_TARGET_RANGE), Some(&50_000.0));
    assert_eq!(ship_all.get(&555), Some(&88.5));
    assert_eq!(ship_all.get(&777), Some(&99.5));

    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, mod_online_id, 2), 1.0);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, mod_online_id, attr_ids::DAMAGE), 7.25);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, mod_online_id, attr_ids::HP_STRUCT), 45.0);
    assert!((super::super::get_item_attribute_value(&ctx, char_id, ship_id, mod_online_id, 54) - 12_500.0).abs() < 1e-6);
    let on_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, mod_online_id);
    assert_eq!(on_all.get(&2), Some(&1.0));
    assert!((on_all[&54] - 12_500.0).abs() < 1e-6);

    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, mod_offline_id, 2), 0.0);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, mod_offline_id, 54), 10_000.0);
    let off_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, mod_offline_id);
    assert_eq!(off_all.get(&2), Some(&0.0));
    assert_eq!(off_all.get(&54), Some(&10_000.0));
    assert_eq!(off_all.get(&556), Some(&33.5));

    // Loaded charge vs cargo charge + fallback L1287
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, loaded_charge_id, attr_ids::EM_DAMAGE), 123.0);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, loaded_charge_id, attr_ids::DAMAGE), 0.0);
    let chg_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, loaded_charge_id);
    assert_eq!(chg_all.get(&attr_ids::EM_DAMAGE), Some(&123.0));
    assert_eq!(chg_all.get(&attr_ids::DAMAGE), Some(&0.0));
    let cargo_chg_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, cargo_charge_id);
    assert_eq!(cargo_chg_all.get(&attr_ids::EM_DAMAGE), Some(&123.0));
    let hangar_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, hangar_item_id);
    assert_eq!(hangar_all.get(&attr_ids::DAMAGE), Some(&0.0));
    // Fallback when active_ship_id == item_id without dogma eval must NOT inject DAMAGE=0.0 (L1287)
    let fallback_ship_all = super::super::get_all_item_attributes(&ctx, char_id, other_ship_id + 999, hangar_item_id);
    assert_eq!(fallback_ship_all.get(&attr_ids::DAMAGE), Some(&0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&other_ship_id).unwrap().type_id = charge_tid; // non-ship type -> get_or_rebuild_ship_fitting errs
    });
    let non_dogma_ship_all = super::super::get_all_item_attributes(&ctx, other_char_id, other_ship_id, other_ship_id);
    assert!(!non_dogma_ship_all.contains_key(&attr_ids::DAMAGE));

    // Active drone live attributes {3, 18, 265, 266} + access checks (probe + drone)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball_id,
            DroneSpaceState {
                ball_id: drone_ball_id,
                drone_item_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: other_char_id,
                original_owner_char_id: other_char_id,
                parent_ship_id: other_ship_id,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 400.0,
                optimal_range: 5_000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.transient_item_attributes.entry(drone_ball_id).or_default().insert(266, 65.0);
        snap.items.insert(
            drone_item_id,
            InventoryItemRecord {
                item_id: drone_item_id,
                type_id: 2454,
                owner_id: other_char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(265, 42.0), (18, 0.25)]),
                ..Default::default()
            },
        );
    });
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, drone_item_id, 265), 42.0);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, drone_item_id, 18), 0.25);
    assert_eq!(super::super::get_item_attribute_value(&ctx, char_id, ship_id, drone_ball_id, 266), 65.0);
    let drone_all = super::super::get_all_item_attributes(&ctx, char_id, ship_id, drone_item_id);
    assert_eq!(drone_all.get(&attr_ids::DAMAGE), Some(&0.0));

    ctx.active_scanner_probes.write().unwrap().insert(
        probe_mine,
        eve_db::PersistedScannerProbe {
            probe_id: probe_mine,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: launcher_id,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: 100,
            state: 1,
            warp_end_ft: None,
        },
    );
    ctx.active_scanner_probes.write().unwrap().insert(
        probe_other,
        eve_db::PersistedScannerProbe {
            probe_id: probe_other,
            probe_type_id: 30_013,
            owner_char_id: other_char_id,
            ship_id: other_ship_id,
            launcher_module_id: launcher_id,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 4,
            scan_range_m: 1_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: 100,
            state: 1,
            warp_end_ft: None,
        },
    );
    let mut session = SessionState::default();
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    assert!(super::super::check_dogma_item_access(&ctx, &session, char_id, probe_mine).is_ok());
    assert_eq!(super::super::check_dogma_item_access(&ctx, &session, char_id, probe_other).unwrap_err().code, "AccessDenied");
    assert_eq!(super::super::check_dogma_item_access(&ctx, &session, char_id, drone_ball_id).unwrap_err().code, "AccessDenied");

    // 2. AddTargetOBO lock cleanup geometry/flags, duplicate lock, and max_locked boundary
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mk_ball = |id: i64, pos: DVec3, mode: MovementMode| Ball {
        id, type_id: ship_tid, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode,
    };
    let _ = sys.spawn_ball_with_meta(mk_ball(ship_id, DVec3::new(50_000.0, 0.0, 0.0), MovementMode::Stop), CombatState::default(), SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_cloaked, DVec3::new(51_000.0, 0.0, 0.0), MovementMode::Stop), CombatState { is_cloaked: true, ..Default::default() }, SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_warp_inv, DVec3::new(52_000.0, 0.0, 0.0), MovementMode::Stop), CombatState { warp_exit_invulnerable_remaining_ticks: 4, is_cloaked: false, ..Default::default() }, SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(
        mk_ball(
            t_warping,
            DVec3::new(53_000.0, 0.0, 0.0),
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1e9, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1e9,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        ),
        CombatState::default(),
        SlimItemMeta::default(),
    ).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_far, DVec3::new(60_001.0, 0.0, 0.0), MovementMode::Stop), CombatState::default(), SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_edge, DVec3::new(60_000.0, 0.0, 0.0), MovementMode::Stop), CombatState::default(), SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_ok1, DVec3::new(51_500.0, 0.0, 0.0), MovementMode::Stop), CombatState::default(), SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_ok2, DVec3::new(52_500.0, 0.0, 0.0), MovementMode::Stop), CombatState::default(), SlimItemMeta::default()).await;
    let _ = sys.spawn_ball_with_meta(mk_ball(t_ok3, DVec3::new(53_500.0, 0.0, 0.0), MovementMode::Stop), CombatState::default(), SlimItemMeta::default()).await;

    for &(tid, locked) in &[
        (t_cloaked, true),
        (t_warp_inv, true),
        (t_warping, true),
        (t_far, true),
        (t_edge, true),
    ] {
        let _ = sys.add_target_lock(ship_id, TargetLock {
            target_id: tid, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked, max_range_m: Some(10_000.0),
        }).await;
    }

    let kw = HashMap::new();
    let add1 = call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ok1)], &kw)
        .await.unwrap().unwrap();
    assert!(matches!(add1, PyRep::Tuple(ref v) if v[0] == PyRep::Integer(1)));
    let locks_after = sys.get_target_locks(ship_id).await.unwrap();
    let lock_ids: Vec<i64> = locks_after.iter().map(|l| l.target_id).collect();
    assert_eq!(locks_after.len(), 2);
    assert!(lock_ids.contains(&t_edge) && lock_ids.contains(&t_ok1));

    // Duplicate on pending lock (locked: false) -> (1, [])
    let dup_pend = call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ok1)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(dup_pend, PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())]));

    // Duplicate on already locked (locked: true) -> (0, [t_edge])
    let dup_locked = call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_edge)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(dup_locked, PyRep::Tuple(vec![PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(t_edge)])]));

    // Fill 3rd lock -> succeeds (len 2 < max_locked 3); 4th lock -> fails (len 3 >= max_locked 3)
    assert!(call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ok2)], &kw).await.unwrap().is_ok());
    assert_eq!(
        call_dogma(&ctx, &mut session, "AddTargetOBO", &[PyRep::Integer(ship_id), PyRep::Integer(t_ok3)], &kw).await.unwrap().unwrap_err().code,
        "TargetingFailed"
    );

    // 3. ShipOnlineModules ownership, slot range, already-online re-call
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().owner_id = other_char_id;
    });
    assert_eq!(
        call_dogma(&ctx, &mut session, "ShipOnlineModules", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_to_online_id)])], &kw).await.unwrap().unwrap_err().code,
        "ShipOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().owner_id = char_id;
        snap.items.get_mut(&other_ship_id).unwrap().type_id = 587;
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = other_ship_id;
    });
    assert_eq!(
        call_dogma(&ctx, &mut session, "ShipOnlineModules", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_to_online_id)])], &kw).await.unwrap().unwrap_err().code,
        "ShipOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    call_dogma(
        &ctx,
        &mut session,
        "ShipOnlineModules",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![
                PyRep::Integer(mod_to_online_id),
                PyRep::Integer(cargo_mod_id),
                PyRep::Integer(mod_other_ship_id),
            ]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&mod_to_online_id), Some(&true));
        assert_ne!(snap.module_online_states.get(&cargo_mod_id), Some(&true));
        assert_ne!(snap.module_online_states.get(&mod_other_ship_id), Some(&true));
    });
    assert!(call_dogma(
        &ctx,
        &mut session,
        "ShipOnlineModules",
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_to_online_id)])],
        &kw,
    ).await.unwrap().is_ok());

    // 4. ChangeDroneSettings Real(0.0 / 1.0) vs boundary 1e-9 / -1.0 / 2.0 / 1.0+1e-9
    call_dogma(
        &ctx,
        &mut session,
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::Integer(1275), PyRep::Real(0.0)),
            (PyRep::Integer(1297), PyRep::Real(1.0)),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    call_dogma(
        &ctx,
        &mut session,
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::Integer(1275), PyRep::Real(1.0)),
            (PyRep::Integer(1297), PyRep::Real(0.0)),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    for bad in [-1.0_f64, 2.0, 0.5, 1e-9, -1e-9, 1.0 + 1e-9, 1.0 - 2e-9] {
        assert_eq!(
            call_dogma(
                &ctx,
                &mut session,
                "ChangeDroneSettings",
                &[PyRep::Dict(vec![(PyRep::Integer(1275), PyRep::Real(bad))])],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument",
            "expected InvalidArgument for ChangeDroneSettings Real({bad})"
        );
    }
}

