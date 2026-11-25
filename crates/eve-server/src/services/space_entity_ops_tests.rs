//! Unit tests for `space_entity_ops` (`entity` & `beyonce` grid, drone, fleet, bookmark & asset ops).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::DroneSpaceState;
use crate::services::drone_test_helpers;
use eve_db::types::{FleetMemberRecord, FleetRecord, FleetRole};
use eve_db::{
    InventoryFlag, InventoryItemRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_entity_fleet_and_bookmarks_tests.rs"]
mod space_entity_fleet_and_bookmarks_tests;
#[cfg(test)]
#[path = "space_entity_ops_edges_tests.rs"]
mod space_entity_ops_edges_tests;

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

pub(super) fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

pub(super) fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>, ship_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(501);
    s.user_id = Some(9);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = role;
    s
}

pub(super) fn kv_field<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        _ => None,
    }
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

pub(super) fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1,
        ..Default::default()
    }
}

pub(super) fn make_fleet(fleet_id: i64, boss_id: i64, members: &[(i64, FleetRole)]) -> FleetRecord {
    FleetRecord {
        fleet_id,
        boss_id,
        members: members.iter().map(|&(cid, _)| cid).collect(),
        free_move: true,
        member_details: members
            .iter()
            .map(|&(cid, role)| {
                (
                    cid,
                    FleetMemberRecord {
                        character_id: cid,
                        role,
                        wing_id: None,
                        squad_id: None,
                        takes_fleet_warp: true,
                        accepts_regroup: true,
                        accepts_conduit_jumps: true,
                        job: 0,
                        role_name: String::new(),
                        join_time_unix: 0,
                    },
                )
            })
            .collect(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmd_unanchor_validates_auth_range_single_use_and_mutates_drone_and_deployable() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let (ship_id, drone_id, dep_id, single_use_id, far_id, enemy_id) = (
        1_010_000_001,
        1_010_000_010,
        1_010_000_011,
        1_010_000_012,
        1_010_000_013,
        1_010_000_014,
    );

    clone_type_with(&mut ctx.sde, 34, 990_001, 100, 18, 5.0, 0.0, &[]);
    clone_type_with(&mut ctx.sde, 34, 990_002, 1250, 22, 50.0, 500.0, &[]);
    clone_type_with(&mut ctx.sde, 34, 990_003, 1249, 22, 50.0, 0.0, &[]);

    let mut unauth = SessionState::new(1);
    assert!(handle(&ctx, &mut unauth, None, "charMgr", "GetPublicInfo", &[], &HashMap::new()).await.is_none());
    assert!(handle(&ctx, &mut unauth, None, "beyonce", "CmdStop", &[], &HashMap::new()).await.is_none());
    assert_eq!(handle(&ctx, &mut unauth, None, "entity", "CmdUnanchor", &[], &HashMap::new()).await.unwrap().unwrap_err().code, "Unauthorized");

    let mut no_sys = make_session(char_id, corp_id, None, Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(handle(&ctx, &mut no_sys, None, "entity", "CmdUnanchor", &[PyRep::Integer(drone_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "NoSolarSystem");

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(handle(&ctx, &mut sess, None, "entity", "CmdUnanchor", &[], &HashMap::new()).await.unwrap().unwrap_err().code, "TargetRequired");
    assert_eq!(handle(&ctx, &mut sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(drone_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "TargetNotInSpace");

    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 606, DVec3::ZERO),
        (drone_id, 990_001, DVec3::new(1_200.0, 0.0, 0.0)),
        (dep_id, 990_002, DVec3::new(4_999.0, 0.0, 0.0)),
        (single_use_id, 990_003, DVec3::new(800.0, 0.0, 0.0)),
        (far_id, 990_001, DVec3::new(5_001.0, 0.0, 0.0)),
        (enemy_id, 990_001, DVec3::new(600.0, 0.0, 0.0)),
    ] {
        let _ = handle_sys.spawn_ball(make_ball(id, tid, pos)).await;
    }

    assert_eq!(handle(&ctx, &mut sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(far_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "TargetOutOfRange");
    assert_eq!(handle(&ctx, &mut sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(enemy_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "AccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(single_use_id, make_item(single_use_id, 990_003, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(dep_id, make_item(dep_id, 990_002, corp_id, sys_id as i64, InventoryFlag::None, 1));
        crate::services::mobile_deployables::write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                dep_id,
                crate::services::mobile_deployables::MobileDeployableRecord {
                    item_id: dep_id,
                    owner_id: corp_id,
                    solar_system_id: sys_id,
                    type_id: 990_002,
                    group_id: 1250,
                    position: [4_999.0, 0.0, 0.0],
                    anchor_start_unix: 1_000,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: 0,
                    reinforced_until_unix: None,
                    last_interaction_unix: 0,
                    last_ping_unix: None,
                    deployed_fleet_id: None,
                },
            );
        });
    });
    assert_eq!(handle(&ctx, &mut sess, None, "entity", "CmdUnanchor", &[PyRep::Integer(single_use_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "CannotUnanchor");

    drone_test_helpers::seed_active_drone(
        &ctx,
        DroneSpaceState {
            ball_id: drone_id,
            drone_item_id: drone_id,
            type_id: 990_001,
            group_id: 100,
            owner_char_id: char_id,
            original_owner_char_id: char_id,
            parent_ship_id: ship_id,
            solar_system_id: sys_id,
            target_id: Some(enemy_id),
            activity_state: 1,
            assist_char_id: Some(char_id),
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
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        },
    );

    let kwargs = HashMap::from([("targetID".to_string(), PyRep::Integer(drone_id))]);
    let drone_res = handle(&ctx, &mut sess, None, "entity", "CmdUnanchor", &[], &kwargs).await.unwrap().unwrap();
    assert_eq!(kv_field(&drone_res, "targetID").and_then(|v| v.as_i64()), Some(drone_id));
    assert_eq!(kv_field(&drone_res, "unanchored").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv_field(&drone_res, "isDrone").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(drone_test_helpers::active_drone(&ctx, drone_id).and_then(|d| d.target_id), None);

    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 9,
        character_id: char_id,
        service: "entity".into(),
        object_id: dep_id,
        oid_str: format!("N=1:{dep_id}"),
        ref_id: 1,
    };
    let dep_bound_ok = super::handle_cmd_unanchor(&ctx, &sess, Some(&bound), &[], &HashMap::new()).await.unwrap();
    assert_eq!(kv_field(&dep_bound_ok, "isDrone").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(kv_field(&dep_bound_ok, "typeID").and_then(|v| v.as_i64()), Some(990_002));
}

#[tokio::test]
async fn beyonce_siphon_and_container_expiry_enforce_type_cloak_and_grid_range() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let foreign_id = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let invalid_target = "InvalidContainerTarget";
    let blocked_cloaked = "ActionBlockedWhileCloaked";
    let ship_id = 98_400_100_i64;
    let foreign_ship_id = 98_400_101_i64;
    let siphon_id = 98_400_200_i64;
    let far_can_id = 98_400_201_i64;
    let near_can_id = 98_400_202_i64;
    let can_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 12)
        .map(|t| t.type_id)
        .unwrap_or(23);
    let siphon_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1247)
        .map(|t| t.type_id)
        .unwrap_or(33_477);
    let mtu_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1250)
        .map(|t| t.type_id)
        .unwrap_or(33_475);
    let wreck_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 186)
        .map(|t| t.type_id)
        .unwrap_or(26_512);
    let far_siphon_id = 98_400_203_i64;
    let unspawned_siphon_id = 98_400_204_i64;
    let unspawned_can_id = 98_400_205_i64;
    let cargo_siphon_id = 98_400_206_i64;
    let cargo_can_id = 98_400_207_i64;
    let mtu_id = 98_400_208_i64;
    let ore_id = 98_400_209_i64;
    let wreck_id = 98_400_210_i64;
    let unspawned_ship_id = 98_400_211_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, owner, loc) in [
            (ship_id, 587, char_id, sys_id as i64),
            (foreign_ship_id, 587, foreign_id, sys_id as i64),
            (siphon_id, siphon_tid, foreign_id, sys_id as i64),
            (far_can_id, can_tid, foreign_id, sys_id as i64),
            (near_can_id, can_tid, foreign_id, sys_id as i64),
            (far_siphon_id, siphon_tid, foreign_id, sys_id as i64),
            (cargo_siphon_id, siphon_tid, char_id, ship_id),
            (cargo_can_id, can_tid, char_id, ship_id),
            (mtu_id, mtu_tid, foreign_id, sys_id as i64),
            (ore_id, 34, foreign_id, sys_id as i64),
            (wreck_id, wreck_tid, foreign_id, sys_id as i64),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag: if loc == ship_id { InventoryFlag::Cargo } else { InventoryFlag::None },
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos) in [
        (ship_id, 587, DVec3::ZERO),
        (siphon_id, siphon_tid, DVec3::new(10_000.0, 0.0, 0.0)),
        (near_can_id, can_tid, DVec3::new(15_000.0, 0.0, 0.0)),
        (far_can_id, can_tid, DVec3::new(300_000.0, 0.0, 0.0)),
        (far_siphon_id, siphon_tid, DVec3::new(300_000.0, 0.0, 0.0)),
        (mtu_id, mtu_tid, DVec3::new(12_000.0, 0.0, 0.0)),
        (ore_id, 34, DVec3::new(13_000.0, 0.0, 0.0)),
        (wreck_id, wreck_tid, DVec3::new(14_000.0, 0.0, 0.0)),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1e6,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
                eve_destiny::SlimItemMeta::default(),
            )
            .await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, owner) in [
            (unspawned_ship_id, 587, char_id),
            (unspawned_siphon_id, siphon_tid, foreign_id),
            (unspawned_can_id, can_tid, foreign_id),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);
    let kw = HashMap::new();

    // Non-GM querying a ship must be rejected.
    for method in ["GetCapacityOfSiphon", "GetContainerExpiryDate"] {
        assert_eq!(
            handle(&ctx, &mut sess, None, "beyonce", method, &[PyRep::Integer(foreign_ship_id)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            invalid_target
        );
    }

    // Out-of-grid (>250km) siphon & container must both be rejected.
    for (method, tid) in [
        ("GetCapacityOfSiphon", far_siphon_id),
        ("GetContainerExpiryDate", far_can_id),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, None, "beyonce", method, &[PyRep::Integer(tid)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "OutOfRange"
        );
    }

    // Wrong SDE groups (MTU 1250 on siphon endpoint; non-container ore 34 on either) must reject.
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(mtu_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        invalid_target
    );
    for method in ["GetCapacityOfSiphon", "GetContainerExpiryDate"] {
        assert_eq!(
            handle(&ctx, &mut sess, None, "beyonce", method, &[PyRep::Integer(ore_id)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            invalid_target
        );
    }

    // Target inside cargo or not spawned as a live ball in system must reject.
    for (method, tid, code) in [
        ("GetCapacityOfSiphon", cargo_siphon_id, "ItemNotInSpace"),
        ("GetContainerExpiryDate", cargo_can_id, "ItemNotInSpace"),
        ("GetCapacityOfSiphon", unspawned_siphon_id, "TargetNotInSpace"),
        ("GetContainerExpiryDate", unspawned_can_id, "TargetNotInSpace"),
    ] {
        let err = handle(&ctx, &mut sess, None, "beyonce", method, &[PyRep::Integer(tid)], &kw)
            .await
            .unwrap()
            .expect_err(&format!("expected {code} for {method} {tid}"));
        assert_eq!(err.code, code);
    }

    // Docked callers (station or Upwell structure) and missing/unspawned active ship must reject.
    let mut station_docked = sess.clone();
    station_docked.station_id = Some(60_003_760);
    let mut structure_docked = sess.clone();
    structure_docked.structure_id = Some(1_040_000_050);
    let mut no_ship_sess = sess.clone();
    no_ship_sess.ship_id = None;
    let mut unspawned_ship_sess = sess.clone();
    unspawned_ship_sess.ship_id = Some(unspawned_ship_id);
    for (bad_sess, code) in [
        (&mut station_docked, "NotInSpace"),
        (&mut structure_docked, "NotInSpace"),
        (&mut no_ship_sess, "NoActiveShip"),
        (&mut unspawned_ship_sess, "ShipNotInSpace"),
    ] {
        for (method, tid) in [
            ("GetCapacityOfSiphon", siphon_id),
            ("GetContainerExpiryDate", near_can_id),
        ] {
            assert_eq!(
                handle(&ctx, bad_sess, None, "beyonce", method, &[PyRep::Integer(tid)], &kw)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                code
            );
        }
    }

    // Cloaked caller must be rejected even for in-grid siphon/container.
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    for (method, tid) in [
        ("GetCapacityOfSiphon", siphon_id),
        ("GetContainerExpiryDate", near_can_id),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, None, "beyonce", method, &[PyRep::Integer(tid)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            blocked_cloaked
        );
    }

    // Uncloaked in-grid siphon, container, and wreck (group 186) succeed.
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);
    assert!(
        handle(&ctx, &mut sess, None, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_id)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(&ctx, &mut sess, None, "beyonce", "GetContainerExpiryDate", &[PyRep::Integer(near_can_id)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(&ctx, &mut sess, None, "beyonce", "GetContainerExpiryDate", &[PyRep::Integer(wreck_id)], &kw)
            .await
            .unwrap()
            .is_ok()
    );

    // BF-03: GetCapacityOfSiphon / GetContainerExpiryDate stay RpcSceneClass::Any in dispatch_rpc so
    // docked GMs can inspect them while docked non-GM pilots still reject with NotInSpace.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    let mut docked_player = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);
    docked_player.user_id = Some(1);
    docked_player.station_id = Some(60_003_760);
    docked_player.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    let err_not_in_space = "NotInSpace";
    for (method, tid) in [
        ("GetCapacityOfSiphon", siphon_id),
        ("GetContainerExpiryDate", near_can_id),
    ] {
        let err = ctx
            .dispatch_rpc(&mut docked_player, "beyonce", None, method, &[PyRep::Integer(tid)], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, err_not_in_space, "docked non-GM {method}");
    }

    let mut docked_gm = docked_player.clone();
    docked_gm.role = eve_machonet::service_roles::ROLE_GMH;
    for (method, tid) in [
        ("GetCapacityOfSiphon", siphon_id),
        ("GetContainerExpiryDate", near_can_id),
    ] {
        assert!(
            ctx.dispatch_rpc(&mut docked_gm, "beyonce", None, method, &[PyRep::Integer(tid)], &kw, None)
                .await
                .is_ok(),
            "docked GM {method} must succeed through dispatch_rpc"
        );
    }
}
