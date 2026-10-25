//! Integration tests for dogmaIM targeting, attribute queries, batched onlining, module repair, and drone settings.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn login_select(ctx: &ServerContext, char_id: i64, sid: u64) -> SessionState {
    let (user, pass) = match char_id {
        C1 => ("demo", "demo-secret"),
        _ => ("other", "other-secret"),
    };
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    sess
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: owner,
                location_id: loc,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_req_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let pairs = [attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_3];
    let skills: Vec<u32> = pairs
        .iter()
        .filter_map(|s| ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(s)).map(|&v| v as u32))
        .filter(|&s| s > 0)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in skills {
            entry.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
}

async fn board_in_space(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3) {
    grant_req_skills(ctx, char_id, type_id);
    if ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)) {
        seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.jump_cooldown_until_unix = 0;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.invalidate_ship_fitting(ship_id);
    let t = &ctx.sde.types[&type_id];
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 1.0,
        mass: t.mass,
        radius: t.radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let (cs, meta) = (ctx.build_ship_combat_state(char_id, ship_id, type_id), ctx.build_ship_slim_meta(char_id, type_id));
    ctx.systems.get_or_spawn(sys_id).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ball");
}

#[tokio::test]
async fn test_dogma_im_targeting_and_queries_complete() {
    // parity: rpc:dogmaIM.AddTargetOBO
    // parity: rpc:dogmaIM.CancelAddTarget
    // parity: rpc:dogmaIM.RemoveTargetOBO
    // parity: rpc:dogmaIM.RemoveTargets
    // parity: rpc:dogmaIM.QueryAttributeValue
    // parity: rpc:dogmaIM.QueryAllAttributesForItem
    // parity: rpc:dogmaIM.FullyDescribeAttribute
    // parity: rpc:dogmaIM.ShipOnlineModules
    // parity: rpc:dogmaIM.InitiateModuleRepairMany
    // parity: rpc:dogmaIM.ChangeDroneSettings
    eve_testkit::attest(&[
        "rpc:dogmaIM.AddTargetOBO",
        "rpc:dogmaIM.CancelAddTarget",
        "rpc:dogmaIM.RemoveTargetOBO",
        "rpc:dogmaIM.RemoveTargets",
        "rpc:dogmaIM.QueryAttributeValue",
        "rpc:dogmaIM.QueryAllAttributesForItem",
        "rpc:dogmaIM.FullyDescribeAttribute",
        "rpc:dogmaIM.ShipOnlineModules",
        "rpc:dogmaIM.InitiateModuleRepairMany",
        "rpc:dogmaIM.ChangeDroneSettings",
    ]);

    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 9101).await;
    let mut s2 = login_select(&ctx, C2, 9102).await;

    let ship1 = 891_001;
    let ship2 = 891_002;
    board_in_space(&ctx, &mut s1, C1, ship1, 606, JITA, DVec3::ZERO).await;
    board_in_space(&ctx, &mut s2, C2, ship2, 606, JITA, DVec3::new(2_500.0, 0.0, 0.0)).await;

    // 1. AddTargetOBO: targets ship2 on behalf of ship1
    let add_obo_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(ship2), PyRep::Integer(ship1)],
    )
    .await;
    let PyRep::Tuple(lock_tuple) = add_obo_res else {
        panic!("AddTargetOBO must return 2-tuple, got {add_obo_res:?}");
    };
    assert_eq!(lock_tuple.len(), 2);
    assert_eq!(lock_tuple[0], PyRep::Bool(true));
    assert!(matches!(lock_tuple[1], PyRep::Real(_)));

    // 2. CancelAddTarget: cancels targeting ship2
    let cancel_res = rpc(&ctx, &mut s1, "dogmaIM", "CancelAddTarget", &[PyRep::Integer(ship2)]).await;
    assert_eq!(cancel_res, PyRep::None);

    // 3. RemoveTargetOBO: relocks then removes target
    rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(ship2), PyRep::Integer(ship1)],
    )
    .await;
    let rem_obo_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "RemoveTargetOBO",
        &[PyRep::Integer(ship2), PyRep::Integer(ship1)],
    )
    .await;
    assert_eq!(rem_obo_res, PyRep::None);

    // 4. RemoveTargets: removes multiple targets in a batch
    rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(ship2), PyRep::Integer(ship1)],
    )
    .await;
    let rem_many_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "RemoveTargets",
        &[PyRep::List(vec![PyRep::Integer(ship2)])],
    )
    .await;
    assert_eq!(rem_many_res, PyRep::None);

    // 5. QueryAttributeValue: query dogma attribute for ship
    let attr_val = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "QueryAttributeValue",
        &[PyRep::Integer(ship1), PyRep::Integer(37)], // maxVelocity
    )
    .await;
    let PyRep::Real(max_vel) = attr_val else {
        panic!("QueryAttributeValue must return Real, got {attr_val:?}");
    };
    assert!(max_vel > 0.0);

    // 6. QueryAllAttributesForItem: returns Dict mapping attribute IDs to values
    let all_attrs = rpc(&ctx, &mut s1, "dogmaIM", "QueryAllAttributesForItem", &[PyRep::Integer(ship1)]).await;
    let PyRep::Dict(attr_dict) = all_attrs else {
        panic!("QueryAllAttributesForItem must return Dict, got {all_attrs:?}");
    };
    assert!(!attr_dict.is_empty());
    assert!(attr_dict.iter().any(|(k, _)| k.as_i64() == Some(37)));

    // 7. FullyDescribeAttribute: returns 4-tuple of formatted description strings
    let full_desc = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "FullyDescribeAttribute",
        &[PyRep::Integer(ship1), PyRep::Integer(37), PyRep::Integer(0)],
    )
    .await;
    let PyRep::Tuple(desc_parts) = full_desc else {
        panic!("FullyDescribeAttribute must return 4-tuple, got {full_desc:?}");
    };
    assert_eq!(desc_parts.len(), 4);
    for part in desc_parts {
        assert!(matches!(part, PyRep::String(_)), "Each description element must be String");
    }

    // 8. ShipOnlineModules: batch onlines offline fitted modules
    let mod_id = 891_010;
    grant_req_skills(&ctx, C1, 17938);
    seed_item(&ctx, mod_id, 17938, C1, ship1, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mod_id, false);
    });
    ctx.invalidate_ship_fitting(ship1);

    let online_res = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[]).await;
    assert_eq!(online_res, PyRep::None);
    let is_online = ctx.db.with_snapshot(|s| s.module_online_states.get(&mod_id).copied().unwrap_or(false));
    assert!(is_online, "Module should be online after ShipOnlineModules");

    // 9. InitiateModuleRepairMany: batched repair of modules
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&C1) {
            ch.station_id = Some(60_003_760);
        }
        if let Some(it) = snap.items.get_mut(&mod_id) {
            it.dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
        }
    });
    s1.station_id = Some(60_003_760);
    let repair_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "InitiateModuleRepairMany",
        &[PyRep::List(vec![PyRep::Integer(mod_id)])],
    )
    .await;
    let PyRep::Tuple(repair_parts) = repair_res else {
        panic!("InitiateModuleRepairMany must return 3-tuple, got {repair_res:?}");
    };
    assert_eq!(repair_parts.len(), 3);
    let PyRep::List(repaired_list) = &repair_parts[0] else {
        panic!("Element 0 must be repaired list");
    };
    assert!(repaired_list.contains(&PyRep::Integer(mod_id)));

    // 10. ChangeDroneSettings: configure drone aggression and focus fire
    let drone_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::String("aggressive".into()), PyRep::Bool(true)),
            (PyRep::String("focusFire".into()), PyRep::Bool(false)),
        ])],
    )
    .await;
    assert_eq!(drone_res, PyRep::None);

    eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let settings = rt.drone_settings.get(&C1).expect("drone settings recorded");
        assert!(settings.aggressive);
        assert!(!settings.focus_fire);
    });
}
