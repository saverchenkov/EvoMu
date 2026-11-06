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

#[tokio::test]
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

#[tokio::test]
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
