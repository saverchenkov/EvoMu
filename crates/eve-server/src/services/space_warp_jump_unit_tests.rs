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

    // Spawn ship far from gate -> NotCloseEnoughToJump
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
        "NotCloseEnoughToJump"
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
    // Landing point inside the 2,500m docking perimeter so a ship 300m short of landing is outside
    // the static perimeter (`> st_radius + 2,500m`) while still within <= 1.0s under capped `k_d = 2.0`.
    let landing = st_pos + DVec3::new(st_radius + 2_300.0, 0.0, 0.0);

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

    // Live CmdDock RPC: Decelerating with > 4.0s remaining (1,000,000m from landing at 15 AU/s) -> ShipInWarp
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
            position: landing + DVec3::new(1_000_000.0, 0.0, 0.0),
            velocity: DVec3::new(-2_000_000.0, 0.0, 0.0),
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

    // #1001: Autopilot RealDock -> CmdDock while client exited DSTBALL_WARP ~2.8s ahead of 5.0 AU/s server decel
    let mut venture_probe = probe.clone();
    venture_probe.position = landing + DVec3::new(6_069.0, 0.0, 0.0);
    venture_probe.velocity = DVec3::new(-10_116.53, 0.0, 0.0);
    venture_probe.max_velocity = 350.0;
    venture_probe.mode = MovementMode::Warp {
        origin,
        destination: landing,
        warp_speed_au_s: 5.0,
        total_distance: origin.distance(landing),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    assert_eq!(
        evaluate_dock_perimeter(&venture_probe, st_pos, st_radius).unwrap(),
        true
    );

    // Decelerating with <= 4.0s remaining (300m from landing at capped k_d = 2.0, outside 2,500m static perimeter) -> Ok & warp stopped
    sys_handle.try_set_ball_position(ship_id, landing + DVec3::new(300.0, 0.0, 0.0));
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
    assert!((cond.shield_hp - updated_cs.max_shield_hp).abs() < 1e-6);
    assert!((cond.armor_hp - 120.0).abs() < 1e-6);
    assert!((cond.hull_hp - 130.0).abs() < 1e-6);
    assert_eq!(cond.capacitor, None);
    assert_eq!(cond.in_space_system, None);
    assert!(cond.space_entry_epoch >= 1);

    // 4. #1082 / #889 (C27): Undock restores shield & capacitor to full while preserving armor/hull damage and bumping space_entry_epoch
    let char_uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))
        .unwrap_or(1);
    sess.user_id = Some(char_uid);
    let _ = ctx.db.acquire_online_lock(
        char_uid,
        sess.session_id,
        Some(char_id),
        true,
        ctx.now_unix_secs(),
    );
    let _ = ctx.drain_notifications_for_char(char_id);
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
        (undocked_cs.capacitor - undocked_cs.max_capacitor).abs() < 1e-4,
        "expected full capacitor {} GJ after undock, got {}",
        undocked_cs.max_capacitor,
        undocked_cs.capacitor
    );
    let undock_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        undock_notifs.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(entries)) if entries.iter().any(|e| {
                    matches!(e, PyRep::Tuple(t)
                        if t.len() >= 7
                        && t[2].as_i64() == Some(ship_id)
                        && t[3].as_i64() == Some(18)
                        && t[5].as_f64().is_some_and(|v| (v - undocked_cs.max_capacitor).abs() < 1e-4))
                }))
        }),
        "Undock must emit OnModuleAttributeChanges (attr 18 CHARGE) for HUD capacitor (#1002)"
    );
    assert!((undocked_cs.shield_hp - undocked_cs.max_shield_hp).abs() < 1e-4);
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
            cs.capacitor = 92.5;
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

#[tokio::test]
async fn cmd_warp_to_stuff_preserves_custom_ball_kinematics_on_normal_warp() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_099_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let st_pos = ctx.sde.stations[&st_id].position;
    let kw = HashMap::new();

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
                type_id: 606,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 606,
            position: st_pos + DVec3::new(10_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 420.0,
            inertia: 1.75,
            mass: 2_345_678.0,
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
            capacitor: 600.0,
            max_capacitor: 600.0,
            base_inertia: Some(9.9),
            base_mass: Some(9_999_999.0),
            ..Default::default()
        },
    );

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };

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
    .expect("CmdWarpToStuff should initiate warp");

    let (_, ball_after) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball still in space");
    assert!(matches!(ball_after.mode, MovementMode::Warp { .. }));
    assert!(
        (ball_after.inertia - 1.75).abs() < 1e-6,
        "CmdWarpToStuff must not clobber live ball inertia ({})",
        ball_after.inertia
    );
    assert!(
        (ball_after.mass - 2_345_678.0).abs() < 1e-3,
        "CmdWarpToStuff must not clobber live ball mass ({})",
        ball_after.mass
    );
    let cs_after = sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("ship combat_state");
    assert!(
        cs_after.capacitor < 600.0,
        "CmdWarpToStuff must deduct warp capacitor via set_capacitor"
    );
}

#[tokio::test]
async fn agency_dock_21au_warp_after_advance_clock_keeps_sync_and_docks() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_131_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let st_pos = ctx.sde.stations[&st_id].position;
    let ship_type_id = if ctx.sde.types.contains_key(&32880) {
        32880
    } else {
        606
    };
    let kw = HashMap::new();

    // Start 21.116 AU from station (matching #1031 Agency Dock Venture warp)
    let start_pos = st_pos + DVec3::new(21.116 * 149_597_870_700.0, 0.0, 0.0);
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
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: start_pos,
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
            capacitor: 100_000.0,
            max_capacitor: 100_000.0,
            ..Default::default()
        },
    );

    let mut sess = SessionState {
        session_id: 1031,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        role: eve_machonet::service_roles::ROLE_GML,
        address: "tcp:127.0.0.1:44446".to_string(),
        ..Default::default()
    };

    // Pre-warp AdvanceClock (+120s) without DoSimClockRebase must not skew machoNet.GetTime
    ctx.dispatch_rpc(
        &mut sess,
        "timeControl",
        None,
        "AdvanceClock",
        &[PyRep::Integer(120)],
        &kw,
        None,
    )
    .await
    .expect("AdvanceClock +120s");

    let server_get_time = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .expect("machoNet.GetTime")
        .as_i64()
        .expect("GetTime integer");
    let wall_ticks = crate::router::context::host_wall_blueos_filetime_ticks();
    let diff_secs = (wall_ticks - server_get_time) as f64 / 10_000_000.0;
    // Mirrors CCP connectionService.ClockSyncDaemon rate clamp (up to 1.95x on skewed GetTime)
    let client_clock_rate = if diff_secs.abs() < 5.0 {
        1.0
    } else {
        let drift = (diff_secs.abs() / 60.0).clamp(0.02, 0.95);
        if diff_secs < 0.0 {
            1.0 + drift
        } else {
            (1.0 - drift).max(0.05)
        }
    };
    assert!(
        (client_clock_rate - 1.0).abs() < 1e-9,
        "ClockSyncDaemon must stay at 1.0x after AdvanceClock (diff_secs={diff_secs:.3})"
    );

    // Agency Dock -> autopilot.__NavigateSystemTo -> CmdWarpToStuff("item", station_id)
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
    .expect("CmdWarpToStuff to station should initiate warp");

    let (_, warped_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball in space");
    assert!(matches!(warped_ball.mode, MovementMode::Warp { .. }));

    let mut client_bp = eve_destiny::Ballpark::new(sys_id);
    assert!(client_bp.add_ball(warped_ball));

    let mut client_finished_warp = false;
    for _step in 0..180 {
        client_bp.step(client_clock_rate);
        let _ = sys_handle.step_manual(1.0).await.unwrap();
        if !matches!(
            client_bp.balls.get(&ship_id).map(|b| &b.mode),
            Some(MovementMode::Warp { .. })
        ) {
            client_finished_warp = true;
            break;
        }
    }
    assert!(
        client_finished_warp,
        "client Destiny ball should exit 21 AU warp within 180s"
    );

    // Client autopilot immediately invokes CmdDock upon client warp completion
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
    .expect("CmdDock after client warp exit must succeed instead of raising ShipInWarp (#1031)");
    assert_eq!(
        dock_res.dict_get("locationID"),
        Some(&PyRep::Integer(st_id))
    );
    assert_eq!(sess.station_id, Some(st_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dock_warp_settle_plan_handles_arriving_warp_post_exit_race_and_rejections() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_101_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let struct_id = 1_040_000_895_101_i64;
    let kw = HashMap::new();

    let st_pos = ctx.sde.stations[&st_id].position;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, st_pos);
    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&char_id].corporation_id;
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                name: "Test Astrahus".into(),
                position: (st_pos + DVec3::new(0.0, 500_000.0, 0.0)).to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        corp_id
    });
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, st_ball) = sys_handle.get_ball_snapshot(st_id).await.unwrap().unwrap();
    let st_radius = st_ball.radius;
    let struct_pos = st_pos + DVec3::new(0.0, 500_000.0, 0.0);
    let struct_radius = ctx
        .sde
        .types
        .get(&35832)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(15_000.0);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Mid-warp (> 4.0s from destination) must return None and leave ship in warp (`ShipInWarp`).
    let far_origin = st_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let landing = st_pos + DVec3::new(st_radius + 2_300.0, 0.0, 0.0);
    let mut ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ball.position = st_pos + DVec3::new(500_000_000.0, 0.0, 0.0);
    ball.mode = MovementMode::Warp {
        origin: far_origin,
        destination: landing,
        warp_speed_au_s: 15.0,
        total_distance: far_origin.distance(landing),
        phase: eve_destiny::WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    sys_handle.spawn_ball(ball.clone()).await.unwrap();
    assert!(
        prepare_dock_warp_settle(&ctx, &sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)])
            .await
            .is_none(),
        "mid-warp Dock must fail fast without a settle wait"
    );
    let after_mid = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(
        matches!(after_mid.mode, MovementMode::Warp { .. }),
        "mid-warp prepare_dock_warp_settle must not force-stop warp"
    );

    // 2. Arriving station warp (< 4.0s, 300m short of landing = outside static 2,500m shell):
    //    stationSvc.Dock stops warp immediately, snaps to `landing`, clears active_warps, and queues Stop.
    let _ = sys_handle.drain_observer_updates(ship_id).await;
    ball.position = landing + DVec3::new(300.0, 0.0, 0.0);
    ball.mode = MovementMode::Warp {
        origin: far_origin,
        destination: landing,
        warp_speed_au_s: 15.0,
        total_distance: far_origin.distance(landing),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(ball.clone()).await.unwrap();
    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, st_id);
    assert_eq!(evaluate_dock_perimeter(&ball, st_pos, st_radius).unwrap(), true);

    let plan = prepare_dock_warp_settle(
        &ctx,
        &sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(st_id)],
    )
    .await
    .expect("arriving stationSvc.Dock must produce a warp settle plan");
    assert!(plan.stopped_now);
    assert_eq!(plan.sys_id, sys_id);
    assert_eq!(plan.ship_id, ship_id);
    assert!(plan.settle_deadline > tokio::time::Instant::now());
    assert!(
        !ctx.active_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&ship_id),
        "stop_arriving_warp_for_dock must clear active_warps"
    );

    let stopped_ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(stopped_ball.mode, MovementMode::Stop);
    assert!(
        stopped_ball.position.distance(landing) < 1e-3,
        "finish_warp_for_dock must snap arriving warp to verified landing_pos inside perimeter"
    );
    assert_eq!(
        evaluate_dock_perimeter(&stopped_ball, st_pos, st_radius).unwrap(),
        false,
        "post-settle Dock perimeter re-check must succeed after landing snap"
    );

    let (_tick, events, _pyrep) = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    assert!(events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::SetBallMassive { ball_id, is_massive: true } if *ball_id == ship_id
    )));
    assert!(events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_id
    )));
    assert!(events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_id
    )));
    assert!(!events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::RemoveBall { ball_id } if *ball_id == ship_id
    )));

    // 3. Post-OnWarpArrived coast race: ship is already stopped inside perimeter, but
    //    warp_stopped_at is still within WARP_DOCK_SETTLE_DURATION -> stopped_now == false + deadline.
    assert!(
        WARP_DOCK_SETTLE_DURATION >= std::time::Duration::from_millis(3_000),
        "WARP_DOCK_SETTLE_DURATION must cover the +2.6s post-AdvanceClock WarpLoop deceleration window (#1051)"
    );
    let race_plan = prepare_dock_warp_settle(
        &ctx,
        &sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(st_id)],
    )
    .await
    .expect("docking immediately after server warp exit must still wait out settle deadline");
    assert!(!race_plan.stopped_now);
    assert!(race_plan.settle_deadline > tokio::time::Instant::now());

    // After settle timestamp clears (or expires), stationary dock requires no extra wait.
    sys_handle.clear_warp_stopped_at(ship_id);
    assert!(
        prepare_dock_warp_settle(&ctx, &sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)])
            .await
            .is_none(),
        "docking after settle window clears must not delay"
    );

    // `record_warp_stops_from_events` records SetBallMassive(true) and clears on WarpTo / RemoveBall.
    sys_handle.record_warp_stops_from_events(&[eve_destiny::DestinyEvent::SetBallMassive {
        ball_id: ship_id,
        is_massive: true,
    }]);
    assert!(sys_handle
        .warp_stop_settle_deadline(ship_id, WARP_DOCK_SETTLE_DURATION)
        .is_some());
    sys_handle.record_warp_stops_from_events(&[eve_destiny::DestinyEvent::WarpTo {
        ball_id: ship_id,
        destination: landing.to_array(),
        warp_speed_au_s: 15.0,
    }]);
    assert!(sys_handle
        .warp_stop_settle_deadline(ship_id, WARP_DOCK_SETTLE_DURATION)
        .is_none());

    // 4. `beyonce.CmdDock` during arriving station warp also prepares settle plan and stops warp.
    ball.position = landing + DVec3::new(300.0, 0.0, 0.0);
    ball.mode = MovementMode::Warp {
        origin: far_origin,
        destination: landing,
        warp_speed_au_s: 15.0,
        total_distance: far_origin.distance(landing),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(ball.clone()).await.unwrap();
    let cmd_dock_plan = prepare_dock_warp_settle(
        &ctx,
        &sess,
        "beyonce",
        "CmdDock",
        &[PyRep::Integer(st_id)],
    )
    .await
    .expect("beyonce.CmdDock during arriving warp must prepare settle plan");
    assert!(cmd_dock_plan.stopped_now);
    assert_eq!(
        sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode,
        MovementMode::Stop
    );
    sys_handle.clear_warp_stopped_at(ship_id);

    // 5. Fast-fail rejections while warp_stopped_at is active:
    //    5a. OutsideDockingPerimeter must return None (fast error response).
    sys_handle.record_warp_stop(ship_id);
    let _ = sys_handle
        .set_ball_position(ship_id, st_pos + DVec3::new(st_radius + 8_000.0, 0.0, 0.0))
        .await;
    assert!(
        prepare_dock_warp_settle(&ctx, &sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)])
            .await
            .is_none(),
        "OutsideDockingPerimeter must return None immediately"
    );

    //    5b. SessionChangeInProgress must return None.
    let _ = sys_handle
        .set_ball_position(ship_id, st_pos + DVec3::new(st_radius + 2_000.0, 0.0, 0.0))
        .await;
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            char_id,
            crate::services::space_destiny::SessionChangeTimerState {
                expires_unix: ctx.now_unix() + 20,
                system_id: sys_id,
                started_tick: 0,
                reason: "test".into(),
                location_change_invul_until: 0,
            },
        );
    assert!(
        prepare_dock_warp_settle(&ctx, &sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)])
            .await
            .is_none(),
        "SessionChangeInProgress must return None immediately"
    );
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    sys_handle.clear_warp_stopped_at(ship_id);

    // 6. Upwell `structureDocking.Dock`: ShipWarpScrambled returns None; unscrambled arriving warp stops + settles.
    let struct_origin = struct_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let struct_landing = struct_pos + DVec3::new(struct_radius + 2_300.0, 0.0, 0.0);
    ball.position = struct_landing + DVec3::new(300.0, 0.0, 0.0);
    ball.mode = MovementMode::Warp {
        origin: struct_origin,
        destination: struct_landing,
        warp_speed_au_s: 15.0,
        total_distance: struct_origin.distance(struct_landing),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(ball.clone()).await.unwrap();
    let _ = sys_handle.set_warp_scramble_strength(ship_id, 2).await;
    assert!(
        prepare_dock_warp_settle(
            &ctx,
            &sess,
            "structureDocking",
            "Dock",
            &[PyRep::Integer(struct_id)],
        )
        .await
        .is_none(),
        "Upwell structureDocking.Dock while warp scrambled must fail fast without stopping warp"
    );
    assert!(matches!(
        sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));

    let _ = sys_handle.set_warp_scramble_strength(ship_id, 0).await;
    let _ = sys_handle.drain_observer_updates(ship_id).await;
    let struct_plan = prepare_dock_warp_settle(
        &ctx,
        &sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(struct_id)],
    )
    .await
    .expect("arriving structureDocking.Dock must prepare settle plan");
    assert!(struct_plan.stopped_now);
    assert!(struct_plan.settle_deadline > tokio::time::Instant::now());
    let (_tick2, struct_events, _) = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    assert!(struct_events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_id
    )));

    // End-to-end synchronous dispatch after settle still completes docking cleanly.
    let struct_dock_res = crate::services::station_structure::handle(
        &ctx,
        &mut sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("structureDocking.Dock must succeed after arriving warp snap");
    assert_ne!(struct_dock_res, PyRep::None);
    assert_eq!(sess.structure_id, Some(struct_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supercapital_dock_and_gate_perimeters_include_ship_ball_radius() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_201_i64;
    let sys_id = 30_000_142_u32;
    let keepstar_pos = DVec3::new(1_000_000.0, 0.0, 0.0);
    let keepstar_radius = 150_000.0;
    let avatar_radius = 6_800.0;
    let aeon_radius = 4_674.0;

    // 1. Avatar Titan (radius 6,800 m) at Keepstar (radius 150,000 m):
    //    GH #1072 repro had surface_dist = 725 m (center_dist = 157,525 m).
    let mut titan = Ball {
        id: ship_id,
        type_id: 11_567,
        position: keepstar_pos + DVec3::new(keepstar_radius + avatar_radius + 725.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 60.0,
        inertia: 0.5,
        mass: 2_400_000_000.0,
        radius: avatar_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    assert_eq!(
        evaluate_dock_perimeter(&titan, keepstar_pos, keepstar_radius).unwrap(),
        false,
        "Titan at 725 m surface distance from Keepstar must be inside docking perimeter"
    );

    // Exact 2,500.0 m surface boundary passes; 2,501.0 m rejects with OutsideDockingPerimeter / DockingApproach.
    titan.position =
        keepstar_pos + DVec3::new(keepstar_radius + avatar_radius + 2_500.0, 0.0, 0.0);
    assert_eq!(
        evaluate_dock_perimeter(&titan, keepstar_pos, keepstar_radius).unwrap(),
        false
    );
    titan.position =
        keepstar_pos + DVec3::new(keepstar_radius + avatar_radius + 2_501.0, 0.0, 0.0);
    let outside_err = evaluate_dock_perimeter(&titan, keepstar_pos, keepstar_radius).unwrap_err();
    assert_eq!(outside_err.code, "OutsideDockingPerimeter");
    assert_eq!(
        outside_err.client_error.as_deref(),
        Some(&eve_proto::ClientError::user(
            eve_proto::UserErrorKey::DockingApproach
        ))
    );

    // 2. Aeon Supercarrier (radius 4,674 m) boundary check.
    let mut aeon = titan.clone();
    aeon.type_id = 23_919;
    aeon.radius = aeon_radius;
    aeon.position = keepstar_pos + DVec3::new(keepstar_radius + aeon_radius + 2_499.0, 0.0, 0.0);
    assert_eq!(
        evaluate_dock_perimeter(&aeon, keepstar_pos, keepstar_radius).unwrap(),
        false
    );
    aeon.position = keepstar_pos + DVec3::new(keepstar_radius + aeon_radius + 2_501.0, 0.0, 0.0);
    assert_eq!(
        evaluate_dock_perimeter(&aeon, keepstar_pos, keepstar_radius)
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );

    // 3. Arriving warp & stop_arriving_warp_for_dock for Avatar Titan:
    //    3a. Already inside max_dock_dist (725 m surface dist, > keepstar_radius + 2,500 m) -> no snap.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let far_origin = keepstar_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let warp_dest = keepstar_pos + DVec3::new(keepstar_radius + avatar_radius + 200.0, 0.0, 0.0);
    let inside_warp_pos =
        keepstar_pos + DVec3::new(keepstar_radius + avatar_radius + 725.0, 0.0, 0.0);
    titan.position = inside_warp_pos;
    titan.mode = MovementMode::Warp {
        origin: far_origin,
        destination: warp_dest,
        warp_speed_au_s: 1.5,
        total_distance: far_origin.distance(warp_dest),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    assert_eq!(
        evaluate_dock_perimeter(&titan, keepstar_pos, keepstar_radius).unwrap(),
        true
    );
    sys_handle.spawn_ball(titan.clone()).await.unwrap();
    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, char_id);
    stop_arriving_warp_for_dock(&ctx, &sys_handle, &titan, keepstar_pos, keepstar_radius);
    let after_inside_stop = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(after_inside_stop.mode, MovementMode::Stop);
    assert!(
        after_inside_stop.position.distance(inside_warp_pos) < 1e-6,
        "stop_arriving_warp_for_dock must not snap when Titan is already inside max_dock_dist"
    );
    assert!(!ctx
        .active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&ship_id));
    assert_eq!(
        evaluate_dock_perimeter(&after_inside_stop, keepstar_pos, keepstar_radius).unwrap(),
        false
    );

    //    3b. 300 m outside max_dock_dist within <= 4.0 s deceleration -> snaps to warp_dest.
    titan.position =
        keepstar_pos + DVec3::new(keepstar_radius + avatar_radius + 2_800.0, 0.0, 0.0);
    titan.velocity = DVec3::new(-10_000.0, 0.0, 0.0);
    titan.mode = MovementMode::Warp {
        origin: far_origin,
        destination: warp_dest,
        warp_speed_au_s: 5.0,
        total_distance: far_origin.distance(warp_dest),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    assert_eq!(
        evaluate_dock_perimeter(&titan, keepstar_pos, keepstar_radius).unwrap(),
        true
    );
    sys_handle.spawn_ball(titan.clone()).await.unwrap();
    stop_arriving_warp_for_dock(&ctx, &sys_handle, &titan, keepstar_pos, keepstar_radius);
    let after_snap = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(after_snap.position.distance(warp_dest) < 1e-3);
    assert_eq!(
        evaluate_dock_perimeter(&after_snap, keepstar_pos, keepstar_radius).unwrap(),
        false
    );

    // 4. Stargate jump perimeter with Avatar Titan (radius 6,800 m) and Stargate (radius 14,051 m):
    let gate_pos = DVec3::new(-5_000_000.0, 0.0, 0.0);
    let gate_radius = 14_051.0;
    titan.mode = MovementMode::Stop;
    titan.velocity = DVec3::ZERO;
    titan.position = gate_pos + DVec3::new(gate_radius + avatar_radius + 500.0, 0.0, 0.0);
    assert_eq!(
        evaluate_stargate_jump_perimeter(&titan, gate_pos, gate_radius).unwrap(),
        false
    );
    titan.position = gate_pos + DVec3::new(gate_radius + avatar_radius + 2_500.0, 0.0, 0.0);
    assert_eq!(
        evaluate_stargate_jump_perimeter(&titan, gate_pos, gate_radius).unwrap(),
        false
    );
    titan.position = gate_pos + DVec3::new(gate_radius + avatar_radius + 2_600.0, 0.0, 0.0);
    let gate_err = evaluate_stargate_jump_perimeter(&titan, gate_pos, gate_radius).unwrap_err();
    assert_eq!(gate_err.code, "NotCloseEnoughToJump");
    assert!(
        gate_err.message.contains("2600.0"),
        "expected surface distance 2600.0 m in message, got: {}",
        gate_err.message
    );

    let gate_warp_dest = gate_pos + DVec3::new(gate_radius + avatar_radius + 725.0, 0.0, 0.0);
    titan.position = gate_warp_dest;
    titan.mode = MovementMode::Warp {
        origin: far_origin,
        destination: gate_warp_dest,
        warp_speed_au_s: 1.5,
        total_distance: far_origin.distance(gate_warp_dest),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    assert_eq!(
        evaluate_stargate_jump_perimeter(&titan, gate_pos, gate_radius).unwrap(),
        true
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn titan_keepstar_dock_and_fax_gate_jump_succeed_within_surface_perimeter() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let titan_ship_id = 895_301_i64;
    let fax_ship_id = 895_302_i64;
    let keepstar_id = 1_040_000_895_301_i64;
    let kw = HashMap::new();

    // Pick a real non-Pochven, non-Zarzakh lowsec/nullsec stargate pair so capital gate jump is legal.
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.solar_systems.get(&j.from_solar_system_id).is_some_and(|s| {
                    s.security_status < 0.45 && s.region_id != 10_000_070
                })
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
                    s.security_status < 0.45 && s.region_id != 10_000_070
                })
        })
        .cloned()
        .expect("SDE must contain a non-Zarzakh non-Pochven lowsec/nullsec stargate");
    let sys_id = gate.from_solar_system_id;
    let keepstar_pos = DVec3::new(2_000_000.0, 0.0, 0.0);
    let keepstar_radius = ctx
        .sde
        .types
        .get(&35_834)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(150_000.0);
    let titan_radius = ctx
        .sde
        .types
        .get(&11_567)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(6_800.0);
    assert!(titan_radius > 2_500.0, "Titan radius must exceed 2,500 m");

    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters[&char_id].corporation_id;
        snap.structures.insert(
            keepstar_id,
            eve_db::UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: 35_834,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                name: "1DQ1-A - Citadel".into(),
                position: keepstar_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        corp_id
    });
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: keepstar_id,
            type_id: 35_834,
            position: keepstar_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000_000_000.0,
            radius: keepstar_radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // Spawn Avatar Titan at 2,501 m surface distance -> structureDocking.Dock rejects;
    // move to 725 m surface distance -> prepare_dock_warp_settle + structureDocking.Dock succeed.
    spawn_ship_in_space(
        &ctx,
        char_id,
        titan_ship_id,
        11_567,
        sys_id,
        keepstar_pos + DVec3::new(keepstar_radius + titan_radius + 2_501.0, 0.0, 0.0),
    );
    let mut titan_ball = sys_handle.get_ball_snapshot(titan_ship_id).await.unwrap().unwrap().1;
    titan_ball.radius = titan_radius;
    sys_handle.spawn_ball(titan_ball.clone()).await.unwrap();

    let mut sess = SessionState {
        session_id: 77,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(titan_ship_id),
        address: "tcp:127.0.0.1:44477".to_string(),
        ..Default::default()
    };

    assert_eq!(
        crate::services::station_structure::handle(
            &ctx,
            &mut sess,
            "structureDocking",
            "Dock",
            &[PyRep::Integer(keepstar_id), PyRep::Integer(titan_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OutsideDockingPerimeter"
    );

    let dock_pos = keepstar_pos + DVec3::new(keepstar_radius + titan_radius + 725.0, 0.0, 0.0);
    let far_origin = keepstar_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    titan_ball.position = dock_pos;
    titan_ball.mode = MovementMode::Warp {
        origin: far_origin,
        destination: dock_pos,
        warp_speed_au_s: 1.5,
        total_distance: far_origin.distance(dock_pos),
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(titan_ball).await.unwrap();

    let settle = prepare_dock_warp_settle(
        &ctx,
        &sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(keepstar_id), PyRep::Integer(titan_ship_id)],
    )
    .await
    .expect("Titan arriving warp inside 2,500 m surface perimeter must produce settle plan");
    assert!(settle.stopped_now);

    let docked = crate::services::station_structure::handle(
        &ctx,
        &mut sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(keepstar_id), PyRep::Integer(titan_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Avatar Titan at 725 m surface distance must dock at Keepstar");
    assert_eq!(docked.dict_get("locationID"), Some(&PyRep::Integer(keepstar_id)));
    assert_eq!(sess.structure_id, Some(keepstar_id));

    // Also verify Apostle Force Auxiliary (radius 3,000 m > 2,500 m) jumps stargate at 725 m surface dist.
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let fax_radius = ctx
        .sde
        .types
        .get(&37_604)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(3_000.0);
    assert!(fax_radius > 2_500.0, "FAX radius must exceed 2,500 m");
    let gate_radius = sys_handle
        .get_ball_snapshot(gate.stargate_id)
        .await
        .unwrap()
        .map(|(_, b)| b.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or_else(|| {
            ctx.sde
                .types
                .get(&16)
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0)
        });

    sess.structure_id = None;
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(fax_ship_id);
    spawn_ship_in_space(
        &ctx,
        char_id,
        fax_ship_id,
        37_604,
        sys_id,
        gate.position + DVec3::new(gate_radius + fax_radius + 2_501.0, 0.0, 0.0),
    );
    let mut fax_ball = sys_handle.get_ball_snapshot(fax_ship_id).await.unwrap().unwrap().1;
    fax_ball.radius = fax_radius;
    sys_handle.spawn_ball(fax_ball).await.unwrap();

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
        "NotCloseEnoughToJump"
    );

    assert!(
        sys_handle
            .set_ball_position(
                fax_ship_id,
                gate.position + DVec3::new(gate_radius + fax_radius + 725.0, 0.0, 0.0),
            )
            .await
    );
    let jump_ok = handle(
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
    .expect("Apostle FAX at 725 m surface distance must jump stargate");
    assert_eq!(
        jump_ok.dict_get("solarSystemID"),
        Some(&PyRep::Integer(gate.to_solar_system_id as i64))
    );
    assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stargate_jump_negative_paths_and_outbound_tidi_rebase_order() {
    // parity: feat:navigation.stargate-jump, feat:navigation.session-change-timer, feat:serverops.time-dilation, notify:DoSimClockRebase
    use crate::outbound_sequencer::{
        sequence_with_events, DestinyBatch, DestinyDrain, OutFrame, RpcCompletion,
        SessionWireState, TypedNotification,
    };
    use eve_proto::{ClientError, UserErrorKey};

    let ctx = test_ctx();
    let kw = HashMap::new();
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id != j.to_solar_system_id
                && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
        })
        .cloned()
        .expect("SDE must contain a stargate");
    let other_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id != gate.from_solar_system_id)
        .cloned()
        .expect("SDE must contain a stargate in another solar system");

    let char_id = 90_000_001_i64;
    let ship_id = 895_401_i64;
    let mut sess = SessionState::new(98_040);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(gate.from_solar_system_id);

    // 1. Docked session -> ShipDocked with no session change.
    sess.station_id = Some(60_003_760);
    let docked_err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .expect("CmdStargateJump docked must return Result")
    .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");
    assert_eq!(sess.solar_system_id, Some(gate.from_solar_system_id));

    // 2. Wrong-system stargateID -> StargateWrongSystem with no session change.
    sess.station_id = None;
    spawn_ship_in_space(&ctx, char_id, ship_id, 587, gate.from_solar_system_id, gate.position);
    let wrong_sys_err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &[PyRep::Integer(other_gate.stargate_id)],
        &kw,
    )
    .await
    .expect("CmdStargateJump wrong-system must return Result")
    .unwrap_err();
    assert_eq!(wrong_sys_err.code, "StargateWrongSystem");
    assert_eq!(sess.solar_system_id, Some(gate.from_solar_system_id));

    // 3. Typed ClientError wire representations for GateClosed and NotCloseEnoughToJump.
    let closed_err = BridgeRpcError::gate_closed(gate.stargate_id);
    match closed_err.to_client_error("beyonce", "CmdStargateJump") {
        ClientError::User { key, args } => {
            assert_eq!(key, UserErrorKey::CustomInfo);
            assert_eq!(
                args,
                vec![(
                    "info".to_string(),
                    PyRep::Tuple(vec![
                        PyRep::Integer(101),
                        PyRep::String(eve_proto::UI_LABEL_GATE_CLOSED.into()),
                    ]),
                )]
            );
        }
        other => panic!("expected CustomInfo User error, got {other:?}"),
    }

    let range_err = BridgeRpcError::not_close_enough_to_jump(2_600.0, 16_691.0, 16_591.0, 2_500.0);
    match range_err.to_client_error("beyonce", "CmdStargateJump") {
        ClientError::User { key, args } => {
            assert_eq!(key, UserErrorKey::NotCloseEnoughToJump);
            assert!(
                args.iter()
                    .any(|(k, v)| k == "maxdist" && *v == PyRep::Integer(2_500)),
                "NotCloseEnoughToJump must carry maxdist=2500, got {args:?}"
            );
        }
        other => panic!("expected NotCloseEnoughToJump User error, got {other:?}"),
    }

    // 4. OutboundSequencer Attach hoists DoSimClockRebase ahead of SetState while keeping other notifications after.
    let mut wire = SessionWireState::new(1, 1);
    wire.enter_space(Some(gate.from_solar_system_id));
    let new_epoch = wire.enter_space(Some(gate.to_solar_system_id));
    let set_state_batch = DestinyBatch::new(
        new_epoch,
        42,
        true,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(42),
            PyRep::Tuple(vec![PyRep::String("SetState".into()), PyRep::Tuple(vec![])]),
        ])]),
    );
    let outcome = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 7,
            response_frame: PyRep::None,
            session_change_frame: Some(PyRep::Tuple(vec![PyRep::String("SessionChange".into())])),
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::Attach {
                epoch: new_epoch,
                set_state: set_state_batch,
            },
            notifications: vec![
                TypedNotification {
                    event: "OnWarpFinished".into(),
                    args: vec![PyRep::Integer(101)],
                    service: None,
                },
                TypedNotification {
                    event: "DoSimClockRebase".into(),
                    args: vec![PyRep::Tuple(vec![PyRep::Integer(100), PyRep::Integer(100)])],
                    service: None,
                },
            ],
            item_change_before_session_change: false,
        },
    );
    let rebase_idx = outcome
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "DoSimClockRebase"))
        .expect("DoSimClockRebase frame");
    let warp_fin_idx = outcome
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnWarpFinished"))
        .expect("OnWarpFinished frame");
    let set_state_idx = outcome
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, .. }))
        .expect("SetState frame");
    assert!(rebase_idx < set_state_idx, "DoSimClockRebase must precede SetState");
    assert!(set_state_idx < warp_fin_idx, "non-clock notifications must follow SetState");
}

async fn run_stargate_jump_fx_probe(
    gate_id: i64,
    ship_id: i64,
    sess_id: u64,
) -> (
    u32,
    i64,
    u32,
    PyRep,
    Vec<eve_destiny::DestinyEvent>,
    Vec<eve_destiny::DestinyEvent>,
    Vec<eve_destiny::DestinyEvent>,
) {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();
    let jump = ctx
        .sde
        .jumps
        .get(&gate_id)
        .cloned()
        .unwrap_or_else(|| panic!("SDE missing jump for gate {gate_id}"));
    let gate_type_id = ctx
        .sde
        .stargate_meta
        .get(&gate_id)
        .map(|(tid, _)| *tid)
        .unwrap_or_else(|| panic!("SDE missing stargate_meta for gate {gate_id}"));

    let src_handle = ctx.systems.get_or_spawn(jump.from_solar_system_id);
    let dest_handle = ctx.systems.get_or_spawn(jump.to_solar_system_id);
    let (_, src_gate_ball) = src_handle
        .get_ball_snapshot(gate_id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("missing source gate ball {gate_id}"));
    let dest_gate_pos = ctx
        .sde
        .jumps
        .get(&jump.destination_stargate_id)
        .map(|j| j.position)
        .unwrap_or_else(|| panic!("missing destination gate jump {}", jump.destination_stargate_id));

    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        jump.from_solar_system_id,
        jump.position + DVec3::new(src_gate_ball.radius + 500.0, 0.0, 0.0),
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = jump.from_solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let (_, ship_snap) = src_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let src_watcher_id = ship_id + 10_000;
    let dest_watcher_id = ship_id + 20_000;
    src_handle
        .spawn_ball(eve_destiny::Ball {
            id: src_watcher_id,
            position: jump.position + DVec3::new(src_gate_ball.radius + 1_500.0, 0.0, 0.0),
            ..ship_snap.clone()
        })
        .await
        .unwrap();
    dest_handle
        .spawn_ball(eve_destiny::Ball {
            id: dest_watcher_id,
            position: dest_gate_pos + DVec3::new(15_000.0, 0.0, 0.0),
            ..ship_snap
        })
        .await
        .unwrap();
    src_handle.push_set_state(src_watcher_id).await.unwrap();
    dest_handle.push_set_state(dest_watcher_id).await.unwrap();
    let _ = src_handle.drain_observer_updates(src_watcher_id).await.unwrap();
    let _ = dest_handle.drain_observer_updates(dest_watcher_id).await.unwrap();

    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(jump.from_solar_system_id);
    sess.address = format!("tcp:127.0.0.1:{}", 46000 + (sess_id % 1000));

    let reply = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &[PyRep::Integer(gate_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdStargateJump should succeed");

    let (_, src_evs, _) = src_handle.drain_observer_updates(src_watcher_id).await.unwrap();
    let (_, dest_evs, _) = dest_handle.drain_observer_updates(dest_watcher_id).await.unwrap();
    let (_, pilot_dest_evs, _) = dest_handle.drain_observer_updates(ship_id).await.unwrap();
    (
        gate_type_id,
        jump.destination_stargate_id,
        jump.to_solar_system_id,
        reply,
        src_evs,
        dest_evs,
        pilot_dest_evs,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stargate_jump_never_emits_gate_activity_across_standard_and_damaged_gates() {
    // parity: feat:navigation.stargate-jump, destiny:OnSpecialFX, destiny:OnClientControllerEvent, destiny:SetState
    // Standard Maurasi -> Perimeter stargate (`type_id=16`) and damaged Maurasi -> Jita gate (`type_id=93250`).
    for (gate_id, expected_gate_type_id, ship_id, sess_id) in [
        (50_016_474_i64, 16_u32, 895_451_i64, 98_051_u64),
        (50_000_802_i64, 93_250_u32, 895_452_i64, 98_052_u64),
    ] {
        let (
            gate_type_id,
            dest_gate_id,
            to_solar_system_id,
            reply,
            src_evs,
            dest_evs,
            pilot_dest_evs,
        ) = run_stargate_jump_fx_probe(gate_id, ship_id, sess_id).await;
        assert_eq!(gate_type_id, expected_gate_type_id);
        assert_eq!(
            reply.dict_get("solarSystemID"),
            Some(&PyRep::Integer(to_solar_system_id as i64))
        );

        for (label, evs) in [
            ("source watcher", &src_evs),
            ("destination watcher", &dest_evs),
            ("pilot destination", &pilot_dest_evs),
        ] {
            let gate_activity_count = evs
                .iter()
                .filter(|ev| {
                    matches!(
                        ev,
                        eve_destiny::DestinyEvent::OnSpecialFXTimed { guid, .. }
                            if guid == "effects.GateActivity"
                    )
                })
                .count();
            assert_eq!(
                gate_activity_count, 0,
                "{label} for gate {gate_id} (type {expected_gate_type_id}) emitted broken GateActivity: {evs:?}"
            );
        }

        let src_gate_activate: Vec<&str> = src_evs
            .iter()
            .filter_map(|ev| match ev {
                eve_destiny::DestinyEvent::OnClientControllerEvent {
                    ball_id,
                    target_id: Some(target),
                    event_name,
                    value: 1,
                    delay: None,
                } if *ball_id == gate_id && *target == ship_id => Some(event_name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(src_gate_activate, vec!["GateActivate"]);

        let src_jump_out_dests: Vec<Vec<i64>> = src_evs
            .iter()
            .filter_map(|ev| match ev {
                eve_destiny::DestinyEvent::OnSpecialFXTimed {
                    ball_id,
                    target_id: Some(target),
                    guid,
                    graphic_info: Some(info),
                    ..
                } if *ball_id == ship_id && *target == gate_id && guid == "effects.JumpOut" => {
                    Some(info.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(src_jump_out_dests, vec![vec![to_solar_system_id as i64]]);

        let dest_arrival: Vec<&str> = dest_evs
            .iter()
            .filter_map(|ev| match ev {
                eve_destiny::DestinyEvent::OnClientControllerEvent {
                    ball_id,
                    target_id: Some(target),
                    event_name,
                    value: 1,
                    delay: None,
                } if *ball_id == dest_gate_id && *target == ship_id => {
                    Some(event_name.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(dest_arrival, vec!["Arrival"]);
        assert_eq!(
            dest_evs
                .iter()
                .filter(|ev| matches!(
                    ev,
                    eve_destiny::DestinyEvent::AddBalls2 { balls }
                        if balls.iter().any(|b| b.id == ship_id)
                ))
                .count(),
            0,
            "destination watcher must not receive AddBalls2 while ship is gate-cloaked: {dest_evs:?}"
        );
        assert_eq!(
            dest_evs
                .iter()
                .filter(|ev| matches!(
                    ev,
                    eve_destiny::DestinyEvent::OnSpecialFXTimed { ball_id, .. } if *ball_id == ship_id
                ))
                .count(),
            0,
            "destination watcher must not receive ship FX while ship is gate-cloaked: {dest_evs:?}"
        );

        assert_eq!(
            pilot_dest_evs.len(),
            3,
            "unexpected pilot destination queue: {pilot_dest_evs:?}"
        );
        assert_eq!(
            match &pilot_dest_evs[0] {
                eve_destiny::DestinyEvent::SetState { observer_ball_id } => Some(*observer_ball_id),
                _ => None,
            },
            Some(ship_id)
        );
        let pilot_arrival: Vec<&str> = pilot_dest_evs
            .iter()
            .filter_map(|ev| match ev {
                eve_destiny::DestinyEvent::OnClientControllerEvent {
                    ball_id,
                    target_id: Some(target),
                    event_name,
                    value: 1,
                    delay: None,
                } if *ball_id == dest_gate_id && *target == ship_id => {
                    Some(event_name.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(pilot_arrival, vec!["Arrival"]);
        let pilot_jump_in_ms: Vec<Option<f64>> = pilot_dest_evs
            .iter()
            .filter_map(|ev| match ev {
                eve_destiny::DestinyEvent::OnSpecialFXTimed {
                    ball_id,
                    guid,
                    duration_ms,
                    ..
                } if *ball_id == ship_id && guid == "effects.JumpIn" => Some(*duration_ms),
                _ => None,
            })
            .collect();
        assert_eq!(pilot_jump_in_ms, vec![Some(5_000.0)]);
    }
}
