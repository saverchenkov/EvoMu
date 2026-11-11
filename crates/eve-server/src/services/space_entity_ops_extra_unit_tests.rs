#![allow(unused_imports)]
//! Additional unit and mutation-killing tests for `space_entity_ops`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime, ContainerLifetimeEntry, DroneSpaceState,
};
use crate::services::space_entity_ops::handle;
use eve_db::types::{FleetMemberRecord, FleetRecord, FleetRole};
use eve_db::{
    corp_roles, BookmarkRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_beyonce_navigation_and_transfers_unit_tests.rs"]
mod space_beyonce_navigation_and_transfers_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(601);
    s.user_id = Some(9);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = role;
    s
}

pub(super) async fn call_ops(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, None, svc, method, args, &kw).await.unwrap()
}

pub(super) fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entity_unanchor_behavior_debug_extra_fleet_members_and_shipping_unit_branches() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_GM_DEFAULT);
    let kw = HashMap::new();

    {
        let reg = Arc::make_mut(&mut ctx.sde);
        let mut t: InvType = reg.types.values().next().cloned().unwrap();
        t.type_id = 33_474;
        t.group_id = 1246;
        t.category_id = 22;
        reg.types.insert(33_474, t);
    }

    // 1. CmdUnanchor with 2-arg ([ids], target_id): Upwell structures reject with CannotUnanchorStructure;
    // corp-owned deployables/starbases succeed via entity.CmdUnanchor
    let struct_id = 1_000_000_800_001_i64;
    let dep_id = 1_000_000_800_002_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(make_ball(ship_id, 587, DVec3::ZERO)).await;
    let _ = sys_handle
        .spawn_ball(make_ball(struct_id, 35832, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    let _ = sys_handle
        .spawn_ball(make_ball(dep_id, 33_474, DVec3::new(1_500.0, 0.0, 0.0)))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                type_id: 35832,
                name: "Corp Struct".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            dep_id,
            InventoryItemRecord {
                item_id: dep_id,
                type_id: 33_474,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_ops(
            &ctx,
            &mut sess,
            "entity",
            "CmdUnanchor",
            &[PyRep::List(vec![]), PyRep::Integer(struct_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotUnanchorStructure"
    );
    let res = call_ops(
        &ctx,
        &mut sess,
        "entity",
        "CmdUnanchor",
        &[PyRep::List(vec![]), PyRep::Integer(dep_id)],
    )
    .await
    .unwrap();
    assert!(matches!(res, PyRep::KeyVal(_)));

    // 2. EnableBehaviorDebugging / DisableBehaviorDebugging via kwargs + bound_entry fallback
    let mut b_kw = HashMap::new();
    b_kw.insert("entityID".into(), PyRep::Integer(555_001));
    assert!(handle(&ctx, &mut sess, None, "entity", "EnableBehaviorDebugging", &[], &b_kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle(&ctx, &mut sess, None, "entity", "DisableBehaviorDebugging", &[], &b_kw)
        .await
        .unwrap()
        .is_ok());
    let mut no_sys_sess = sess.clone();
    no_sys_sess.solar_system_id = None;
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 9,
        character_id: char_id,
        service: "entity".into(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    assert!(handle(&ctx, &mut no_sys_sess, Some(&bound), "entity", "GetBehaviorTreeManager", &[], &kw)
        .await
        .unwrap()
        .is_ok());

    // 3. GetExtraFleetMembers with Guard role
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            801,
            FleetRecord {
                fleet_id: 801,
                boss_id: char_id,
                members: vec![char_id, other_char],
                free_move: true,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            777_001,
            DroneSpaceState {
                ball_id: 777_001,
                drone_item_id: 777_001,
                type_id: 2456,
                group_id: 100,
                owner_char_id: other_char,
                original_owner_char_id: other_char,
                parent_ship_id: 200,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: Some(char_id),
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 400.0,
                optimal_range: 2000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    let extra = call_ops(&ctx, &mut sess, "entity", "GetExtraFleetMembers", &[]).await.unwrap();
    assert!(matches!(extra, PyRep::List(ref v) if v.len() == 1));

    // 4. OpenShippingUnitStorage errors & personal deployable restriction against fleetmate
    assert_eq!(
        call_ops(&ctx, &mut sess, "entity", "OpenShippingUnitStorage", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_ops(&ctx, &mut sess, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(999_888)])
            .await
            .unwrap_err()
            .code,
        "UnitNotInSpace"
    );

    let dep_id = 777_002_i64;
    let _ = sys_handle
        .spawn_ball(make_ball(dep_id, 33_474, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dep_id,
            InventoryItemRecord {
                item_id: dep_id,
                type_id: 33_474,
                owner_id: other_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut non_gm = sess.clone();
    non_gm.role = service_roles::ROLE_PLAYER_DEFAULT;
    assert_eq!(
        call_ops(&ctx, &mut non_gm, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(dep_id)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
}
