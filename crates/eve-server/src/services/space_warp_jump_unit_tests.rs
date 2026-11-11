//! Unit and mutation-killing tests for `services::space_warp_jump`.

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_warp_jump_extra_unit_tests.rs"]
mod extra_unit_tests;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn spawn_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let handle = ctx.systems.get_or_spawn(sys_id);
    handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            armor_hp: 500.0,
            max_armor_hp: 500.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            capacitor: 250.0,
            max_capacitor: 250.0,
            ..Default::default()
        },
    );
}

#[test]
fn resolve_bound_or_session_sys_and_bookmark_expired_err_cover_branches() {
    let mut sess = SessionState::default();
    assert_eq!(
        resolve_bound_or_session_sys(None, &sess).unwrap_err().code,
        "BoundObjectRequired"
    );
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(resolve_bound_or_session_sys(None, &sess).unwrap(), 30_000_142);

    let wrong_bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: 90_000_001,
        service: "station".into(),
        object_id: 60_003_760,
        oid_str: "N=888444:1".into(),
        ref_id: 1,
    };
    let wrong_err = resolve_bound_or_session_sys(Some(&wrong_bound), &sess).unwrap_err();
    assert_eq!(wrong_err.code.to_ascii_lowercase(), "invalidboundservice");

    let ok_bound = BoundObjectEntry {
        service: "beyonce".into(),
        object_id: 30_000_142,
        ..wrong_bound
    };
    assert_eq!(
        resolve_bound_or_session_sys(Some(&ok_bound), &sess).unwrap(),
        30_000_142
    );
    let bm_err = bookmark_expired_err(42);
    assert_eq!(bm_err.code.to_ascii_lowercase(), "bookmarkexpired");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_stuff_validates_all_preconditions_and_initiates_warp() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Docked -> NotInSpace
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotInSpace"
    );

    // Undocked without ship -> NoActiveShip
    sess.station_id = None;
    sess.ship_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);

    // Invalid destination / target / own ship / non-finite range
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdWarpToStuff", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidWarpDestination"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(st_id),
                PyRep::Real(f64::NAN),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Spawn ship near station -> WarpDistanceTooShort
    let st_pos = ctx.sde.stations[&st_id].position;
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        sys_id,
        st_pos + DVec3::new(10_000.0, 0.0, 0.0),
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpDistanceTooShort"
    );

    // Move ship 10,000,000m away and test WarpScrambled, CloakedCannotWarp, NotEnoughCapacitor, then valid warp
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_set_ball_position(ship_id, st_pos + DVec3::new(10_000_000.0, 0.0, 0.0));
    sys_handle.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 1);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpScrambled"
    );

    let cloak_mod_id = 910_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11370,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 0;
        cs.is_cloaked = true;
        cs.is_covert_cloak = false;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CloakedCannotWarp"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.capacitor = 0.0;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughCapacitor"
    );

    sys_handle.try_update_combat_state(ship_id, |cs| cs.capacitor = 250.0);
    let warp_ok = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(warp_ok.as_i64().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stargate_jump_and_dock_enforce_range_timers_and_complete_transitions() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_002_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH)
        .cloned()
        .expect("Jita must have a stargate");

    let mut sess = SessionState {
        session_id: 50,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Spawn ship far from gate -> OutsideStargateJumpRange
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        sys_id,
        gate.position + DVec3::new(100_000.0, 0.0, 0.0),
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownStargate"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OutsideStargateJumpRange"
    );

    // Move within 500m of gate and jump
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_set_ball_position(ship_id, gate.position + DVec3::new(500.0, 0.0, 0.0));
    let jump_res = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        jump_res.dict_get("solarSystemID"),
        Some(&PyRep::Integer(gate.to_solar_system_id as i64))
    );
    assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));

    // Clear session-change timer, move back to Jita near station, and test CmdDock
    ctx.session_change_timers.lock().unwrap().clear();
    sess.solar_system_id = Some(sys_id);
    let st_pos = ctx.sde.stations[&st_id].position;
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        sys_id,
        st_pos + DVec3::new(500_000.0, 0.0, 0.0),
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdDock",
            &[PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OutsideDockingPerimeter"
    );

    sess.address = "tcp:127.0.0.1:44445".to_string();
    let (_, st_ball) = sys_handle.get_ball_snapshot(st_id).await.unwrap().unwrap();
    sys_handle.try_set_ball_position(ship_id, st_pos + DVec3::new(st_ball.radius + 10_000.0, 0.0, 0.0));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdDock",
            &[PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OutsideDockingPerimeter"
    );

    sys_handle.try_set_ball_position(ship_id, st_pos + DVec3::new(500.0, 0.0, 0.0));
    let dock_res = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdDock",
        &[PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(dock_res.dict_get("locationID"), Some(&PyRep::Integer(st_id)));
    assert_eq!(sess.station_id, Some(st_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmd_dock_warp_deceleration_1s_tolerance_and_mid_warp_rejection() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_099_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    let st_pos = ctx.sde.stations[&st_id].position;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, st_pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, st_ball) = sys_handle.get_ball_snapshot(st_id).await.unwrap().unwrap();
    let st_radius = st_ball.radius;
    let origin = st_pos + DVec3::new(100_000_000.0, 0.0, 0.0);
    let landing = st_pos + DVec3::new(st_radius, 0.0, 0.0);

    // Direct evaluate_dock_perimeter unit coverage across non-warp and warp boundaries
    let mut probe = Ball {
        id: ship_id,
        type_id: 606,
        position: st_pos + DVec3::new(st_radius + 5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    assert_eq!(
        evaluate_dock_perimeter(&probe, st_pos, st_radius).unwrap_err().code,
        "OutsideDockingPerimeter"
    );
    probe.position = st_pos + DVec3::new(st_radius + 2_000.0, 0.0, 0.0);
    assert_eq!(evaluate_dock_perimeter(&probe, st_pos, st_radius).unwrap(), false);

    // Warp to a destination outside docking perimeter must reject with ShipInWarp even if < 1.0s to that destination
    let far_dest = st_pos + DVec3::new(st_radius + 10_000.0, 0.0, 0.0);
    probe.position = far_dest + DVec3::new(500.0, 0.0, 0.0);
    probe.mode = MovementMode::Warp {
        origin,
        destination: far_dest,
        warp_speed_au_s: 15.0,
        total_distance: origin.distance(far_dest),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    assert_eq!(
        evaluate_dock_perimeter(&probe, st_pos, st_radius).unwrap_err().code,
        "ShipInWarp"
    );

    // Aligning phase outside 2,500m perimeter rejects with ShipInWarp
    probe.position = landing + DVec3::new(2_800.0, 0.0, 0.0);
    probe.mode = MovementMode::Warp {
        origin,
        destination: landing,
        warp_speed_au_s: 15.0,
        total_distance: origin.distance(landing),
        phase: eve_destiny::WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    assert_eq!(
        evaluate_dock_perimeter(&probe, st_pos, st_radius).unwrap_err().code,
        "ShipInWarp"
    );

    // Live CmdDock RPC: Decelerating with > 1.0s remaining (5,000m from landing at 15 AU/s) -> ShipInWarp
    let decel_mode = MovementMode::Warp {
        origin,
        destination: landing,
        warp_speed_au_s: 15.0,
        total_distance: origin.distance(landing),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 606,
            position: landing + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::new(-25_000.0, 0.0, 0.0),
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: decel_mode,
        },
        CombatState::default(),
    );
    ctx.active_warps.lock().unwrap().insert(ship_id, char_id);

    let mut sess = SessionState {
        session_id: 51,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        address: "tcp:127.0.0.1:44446".to_string(),
        ..Default::default()
    };
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdDock",
            &[PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipInWarp"
    );
    assert_eq!(sess.station_id, None);
    assert!(ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // Decelerating with <= 1.0s remaining (2,800m from landing at 15 AU/s, outside 2,500m static perimeter) -> Ok & warp stopped
    sys_handle.try_set_ball_position(ship_id, landing + DVec3::new(2_800.0, 0.0, 0.0));
    let dock_ok = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdDock",
        &[PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdDock within <= 1.0s warp deceleration must succeed");
    assert_eq!(dock_ok.dict_get("locationID"), Some(&PyRep::Integer(st_id)));
    assert_eq!(sess.station_id, Some(st_id));
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
}

#[tokio::test]
async fn e2_handoffs_b4_b8_c27_take_restore_absent_ball_and_capacitor_persistence() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let jita_sys = 30_000_142_u32;
    let perim_sys = 30_000_144_u32;
    let st_id = 60_003_760_i64;

    let st_pos = ctx
        .sde
        .stations
        .get(&st_id)
        .map(|s| s.position)
        .unwrap_or(DVec3::ZERO);
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        jita_sys,
        st_pos + DVec3::new(500.0, 0.0, 0.0),
    );
    ctx.db.with_mut_snapshot(|snap| {
        crate::sp6_lifecycle::enter_space_in_snap(snap, ship_id, jita_sys);
    });

    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let perim_handle = ctx.systems.get_or_spawn(perim_sys);

    // 1. Modify combat state & set partial capacitor + damage (#846, #889)
    let updated_cs = jita_handle
        .modify_combat_state(ship_id, |cs| {
            cs.shield_hp = 110.0;
            cs.armor_hp = 120.0;
            cs.structure_hp = 130.0;
            cs.capacitor = 92.5;
        })
        .await
        .unwrap()
        .expect("modify_combat_state on live ball must return Some");
    assert!((updated_cs.capacitor - 92.5).abs() < 1e-6);

    // 2. Dock via CmdDock -> takes ball atomically and records condition + partial capacitor (#842, #889, #917)
    let mut sess = SessionState {
        session_id: 917,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(jita_sys),
        station_id: None,
        ship_id: Some(ship_id),
        address: "tcp:127.0.0.1:45917".to_string(),
        ..Default::default()
    };
    let kw = HashMap::new();
    handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdDock",
        &[PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdDock should succeed");
    assert_eq!(sess.station_id, Some(st_id));
    assert!(jita_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());

    // 3. #846 (B8): Damage, heal, set_capacitor, and modify_combat_state on docked/absent ball
    // must return None / Err and NEVER fabricate CombatState::default() or clobber docked HP/capacitor!
    assert!(
        jita_handle
            .apply_damage(
                ship_id,
                eve_destiny::DamageVector {
                    em: 100.0,
                    thermal: 100.0,
                    kinetic: 100.0,
                    explosive: 100.0,
                },
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        jita_handle
            .apply_true_damage(ship_id, 250.0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(jita_handle.set_capacitor(ship_id, 1.0).await.is_err());
    assert!(jita_handle.heal_combat_state(ship_id).await.is_err());
    assert!(
        jita_handle
            .modify_combat_state(ship_id, |cs| cs.shield_hp = 1.0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(jita_handle.get_combat_state(ship_id).await.unwrap().is_none());

    // Step tick on Jita while ship is docked must not overwrite docked condition
    let _ = jita_handle.step_manual(1.0).await.unwrap();
    let cond = ctx
        .db
        .with_snapshot(|snap| crate::sp6_lifecycle::read_ship_condition_in_snap(snap, ship_id))
        .expect("persisted condition should exist");
    assert!((cond.shield_hp - 110.0).abs() < 1e-6);
    assert!((cond.armor_hp - 120.0).abs() < 1e-6);
    assert!((cond.hull_hp - 130.0).abs() < 1e-6);
    assert_eq!(cond.capacitor, Some(92.5));
    assert_eq!(cond.in_space_system, None);
    assert!(cond.space_entry_epoch >= 1);

    // 4. #889 (C27): Undock restores partial capacitor (92.5 GJ, not 250.0 GJ max) and bumps space_entry_epoch
    crate::services::station_structure::handle(
        &ctx,
        &mut sess,
        "stationSvc",
        "Undock",
        &[PyRep::Integer(ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Undock should succeed");
    assert_eq!(sess.station_id, None);
    let undocked_cs = jita_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("undocked ship must have live combat state");
    assert!(
        (undocked_cs.capacitor - 92.5).abs() < 1e-4,
        "expected partial capacitor 92.5 GJ after undock, got {}",
        undocked_cs.capacitor
    );
    assert!((undocked_cs.shield_hp - 110.0).abs() < 1e-4);
    assert!((undocked_cs.armor_hp - 120.0).abs() < 1e-4);
    assert!((undocked_cs.structure_hp - 130.0).abs() < 1e-4);

    let cond_after_undock = ctx
        .db
        .with_snapshot(|snap| crate::sp6_lifecycle::read_ship_condition_in_snap(snap, ship_id))
        .unwrap();
    assert_eq!(cond_after_undock.in_space_system, Some(jita_sys));
    assert!(cond_after_undock.space_entry_epoch > cond.space_entry_epoch);

    // 5. #842 (B4): take_ball + restore_ball rollback when DB transition fails during stargate jump
    ctx.session_change_timers.lock().unwrap().clear();
    let jita_to_perim_gate = ctx
        .sde
        .jumps
        .values()
        .find(|g| g.from_solar_system_id == jita_sys && g.to_solar_system_id == perim_sys)
        .cloned()
        .expect("Jita -> Perimeter stargate");
    jita_handle.try_set_ball_position(ship_id, jita_to_perim_gate.position + DVec3::new(500.0, 0.0, 0.0));
    let _ = jita_handle
        .modify_combat_state(ship_id, |cs| {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.gate_cloak_remaining_ticks = 0;
            cs.is_cloaked = false;
        })
        .await;

    // Force DB transition failure by marking character in another solar system concurrently right after take_ball:
    // First verify direct take_ball + restore_ball round-trip preserves ball and combat_state
    let taken = jita_handle
        .take_ball(ship_id)
        .await
        .expect("first take_ball must return Some");
    assert!(
        jita_handle.take_ball(ship_id).await.is_none(),
        "second take_ball on already-taken ball must return None (#842)"
    );
    jita_handle
        .restore_ball(taken)
        .await
        .expect("restore_ball must succeed");
    assert!(jita_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

    // Execute full CmdStargateJump and verify single-authority residency in destination system (#842, #917)
    handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &[
            PyRep::Integer(jita_to_perim_gate.stargate_id),
            PyRep::Integer(jita_to_perim_gate.destination_stargate_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdStargateJump Jita -> Perimeter should succeed");
    assert_eq!(sess.solar_system_id, Some(perim_sys));
    assert!(
        jita_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "departed system must not retain ship ball"
    );
    assert!(
        perim_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
        "destination system must own ship ball"
    );
    let perim_cs = perim_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("destination combat_state");
    assert!((perim_cs.capacitor - 92.5).abs() < 1e-4);

    // Stale tick flush from departed system (Jita) must never overwrite destination system residency/condition
    let _ = jita_handle.step_manual(1.0).await.unwrap();
    let cond_after_jump = ctx
        .db
        .with_snapshot(|snap| crate::sp6_lifecycle::read_ship_condition_in_snap(snap, ship_id))
        .unwrap();
    assert_eq!(cond_after_jump.in_space_system, Some(perim_sys));
    assert_eq!(cond_after_jump.capacitor, Some(92.5));
}
