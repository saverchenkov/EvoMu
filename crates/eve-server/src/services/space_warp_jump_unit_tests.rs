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

fn mark_char_online(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) {
    let char_uid = ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| c.user_id)
            .filter(|uid| snap.accounts.values().any(|a| a.user_id == *uid))
            .unwrap_or_else(|| {
                let demo_uid = snap.accounts.values().next().map(|a| a.user_id).unwrap_or(1);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.user_id = demo_uid;
                }
                demo_uid
            })
    });
    sess.user_id = Some(char_uid);
    ctx.db
        .acquire_online_lock(
            char_uid,
            sess.session_id,
            Some(char_id),
            true,
            ctx.now_unix_secs(),
        )
        .expect("acquire online lock for character notification delivery");
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
    // parity: feat:navigation.stargate-jump, destiny:OnSpecialFX, destiny:SetState
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

#[tokio::test]
async fn physical_radius_resolution_chain_provenance_and_keepstar_titan_dock_check() {
    // 1. Pure 6-step resolution chain order:
    //    item.physicalRadius -> type.physicalRadius -> item.radius -> type.radius -> interactionRadius -> 15_000.0
    assert_eq!(
        physical_radius(&PhysicalRadiusInput {
            item_physical_radius: Some(111.0),
            type_physical_radius: Some(222.0),
            item_radius: Some(333.0),
            type_radius: Some(444.0),
            interaction_radius: Some(555.0),
        }),
        (111.0, RadiusSource::ItemPhysicalRadius)
    );
    assert_eq!(
        physical_radius(&PhysicalRadiusInput {
            item_physical_radius: Some(0.0), // invalid non-positive skipped cleanly
            type_physical_radius: Some(222.0),
            item_radius: Some(333.0),
            type_radius: Some(444.0),
            interaction_radius: Some(555.0),
        }),
        (222.0, RadiusSource::TypePhysicalRadius)
    );
    assert_eq!(
        physical_radius(&PhysicalRadiusInput {
            item_physical_radius: Some(f64::NAN), // non-finite skipped cleanly
            type_physical_radius: None,
            item_radius: Some(333.0),
            type_radius: Some(444.0),
            interaction_radius: Some(555.0),
        }),
        (333.0, RadiusSource::ItemRadius)
    );
    assert_eq!(
        physical_radius(&PhysicalRadiusInput {
            type_radius: Some(444.0),
            interaction_radius: Some(555.0),
            ..Default::default()
        }),
        (444.0, RadiusSource::TypeRadius)
    );
    assert_eq!(
        physical_radius(&PhysicalRadiusInput {
            interaction_radius: Some(555.0),
            ..Default::default()
        }),
        (555.0, RadiusSource::InteractionRadius)
    );
    assert_eq!(
        physical_radius(&PhysicalRadiusInput::default()),
        (DEFAULT_PHYSICAL_RADIUS_METERS, RadiusSource::FallbackDefault)
    );

    // 2. Once-per-type_id warning gate for fallback default
    let synthetic_unknown_type = 9_991_088_u32;
    assert!(log_fallback_radius_once(Some(synthetic_unknown_type), Some(1), RadiusSource::FallbackDefault));
    assert!(!log_fallback_radius_once(Some(synthetic_unknown_type), Some(2), RadiusSource::FallbackDefault));

    // 3. Real SDE resolution across Station, Stargate, Keepstar (35834), and Titan (11567 Avatar)
    let ctx = ServerContext::new();
    let jita_station_id = 60_003_760_i64;
    let st = ctx.sde.stations.get(&jita_station_id).expect("Jita 4-4 station");
    let (sta_r, sta_src) = resolve_physical_radius(
        &ctx,
        Some(st.station_type_id),
        Some(jita_station_id),
        PhysicalRadiusInput::default(),
    );
    assert_eq!(sta_src, RadiusSource::TypeRadius);
    assert!(sta_r > 10_000.0, "station radius resolved via SDE type: {sta_r}");

    let (gate_tid, _) = ctx
        .sde
        .stargate_type_and_name(50_001_249)
        .expect("Jita->Perimeter stargate");
    let (gate_r, gate_src) = resolve_physical_radius(
        &ctx,
        Some(gate_tid),
        Some(50_001_249),
        PhysicalRadiusInput::default(),
    );
    assert_eq!(gate_src, RadiusSource::TypeRadius);
    assert!((gate_r - ctx.sde.types[&gate_tid].radius).abs() < 1e-6);

    let (keepstar_r, keepstar_src) = resolve_physical_radius(
        &ctx,
        Some(35_834),
        Some(1_000_000_999_001),
        PhysicalRadiusInput::default(),
    );
    assert_eq!(keepstar_src, RadiusSource::TypeRadius);
    assert!((keepstar_r - 150_000.0).abs() < 1e-6);

    let (titan_r, titan_src) = resolve_physical_radius(
        &ctx,
        Some(11_567),
        Some(1_000_000_999_002),
        PhysicalRadiusInput::default(),
    );
    assert_eq!(titan_src, RadiusSource::TypeRadius);
    assert!((titan_r - 6_800.0).abs() < 1e-6);

    // Zero-radius Titan ship ball (e.g. legacy/uninitialized ball) resolves Titan physical radius
    // so Keepstar surface-perimeter dock check at 725 m surface distance (157,525 m center) succeeds (#1072).
    let keepstar_pos = DVec3::new(100_000_000.0, 0.0, 0.0);
    let mut zero_radius_titan = eve_destiny::Ball {
        id: 1_000_000_999_002,
        type_id: 11_567,
        position: keepstar_pos + DVec3::new(keepstar_r + titan_r + 725.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 60.0,
        inertia: 5.0,
        mass: 2_400_000_000.0,
        radius: 0.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let resolved_titan = resolve_ship_ball_for_perimeter(&ctx, &zero_radius_titan);
    assert!((resolved_titan.radius - 6_800.0).abs() < 1e-6);
    assert_eq!(
        evaluate_dock_perimeter(&resolved_titan, keepstar_pos, keepstar_r).unwrap(),
        false
    );
    // Without resolution (0.0 ship radius), 157,525 m > 150,000 + 2,500 would falsely fail:
    zero_radius_titan.radius = 0.0;
    assert!(evaluate_dock_perimeter(&zero_radius_titan, keepstar_pos, keepstar_r).is_err());
}

#[tokio::test]
async fn merlin_3_95au_warp_under_dedicated_ticker_avoids_spurious_tidi_and_dock_stop_errors_1207() {
    let ctx = test_ctx();
    ctx.dedicated_physics_ticker_active
        .store(true, std::sync::atomic::Ordering::Release);

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_144_u32;
    let ship_id = 1_897_362_930_i64;
    let astra_id = 1_001_897_110_848_i64;
    let astra_pos = DVec3::ZERO;
    let start_pos = DVec3::new(590_860_935_345.6, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            astra_id,
            eve_db::UpwellStructureRecord {
                structure_id: astra_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Perimeter Astrahus".into(),
                state: "ShieldVulnerable".into(),
                position: astra_pos.to_array(),
                shield_hp: 500.0,
                armor_hp: 500.0,
                hull_hp: 500.0,
                quantum_core_installed: true,
                fuel_blocks: 1000,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let (astra_radius, _) = resolve_physical_radius(
        &ctx,
        Some(35832),
        Some(astra_id),
        PhysicalRadiusInput::default(),
    );

    spawn_ship_in_space(&ctx, char_id, ship_id, 603, sys_id, start_pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Align Merlin physics attributes (5.0 AU/s, mass=997000, inertia=3.6, max_velocity=320.0).
    let _ = sys_handle.spawn_ball(Ball {
        id: astra_id,
        type_id: 35832,
        position: astra_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1e12,
        radius: astra_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: 603,
        position: start_pos,
        velocity: DVec3::ZERO,
        max_velocity: 320.0,
        inertia: 3.6,
        mass: 997_000.0,
        radius: 39.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(astra_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdWarpToStuff to Astrahus must succeed");

    // Step at 1 Hz under dedicated physics ticker budget; with zero cluster lag TiDi must stay 1.00
    // and Merlin must exit warp cleanly by tick 26 (5s align + 20s warp).
    for _ in 0..26 {
        crate::sp6_lifecycle::step_active_systems_physics_tick_with_budget(
            &ctx,
            1,
            0.0,
            crate::sp6_lifecycle::PHYSICS_TICK_STEP_BUDGET,
        )
        .await;
    }

    assert!(
        (sys_handle.tidi_factor() - 1.0).abs() < 1e-6,
        "dedicated physics ticker within budget must never engage TiDi ({})",
        sys_handle.tidi_factor()
    );
    let arrived_ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(
        !matches!(arrived_ball.mode, MovementMode::Warp { .. }),
        "Merlin 3.95 AU warp must exit warp within 26 1s ticks (mode={:?})",
        arrived_ball.mode
    );
    assert!(
        evaluate_dock_perimeter(&arrived_ball, astra_pos, astra_radius).is_ok(),
        "arrived Merlin must satisfy Astrahus dock perimeter without ShipInWarp"
    );
    let beyonce_bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "beyonce".into(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    crate::services::space_destiny::handle(
        &ctx,
        &mut sess,
        Some(&beyonce_bound),
        "beyonce",
        "CmdStop",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdStop after arrival must succeed without ShipAlreadyInWarp");
}

#[tokio::test]
async fn cmd_warp_rejected_paths_do_not_clear_cloak_or_mutate_db_gh_1351() {
    let ctx = test_ctx();
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let sys_id = 30_000_142_u32;
    let (st_id, st_pos) = ctx
        .sde
        .stations
        .iter()
        .find(|(_, st)| st.solar_system_id == sys_id)
        .map(|(&id, st)| (id, st.position))
        .expect("station in Jita");
    let ship_id = 88_511_001_i64;
    let cloak_mod_id = 88_511_002_i64;
    let far_pos = st_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, far_pos);

    let non_covert_cloak_tid = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| {
            t.group_id == 330
                && !t.effect_ids.iter().any(|&eid| {
                    ctx.sde
                        .effects
                        .get(&eid)
                        .is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe)
                })
        })
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .unwrap_or(11_577);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: far_pos,
            velocity: DVec3::ZERO,
            max_velocity: 320.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn ship ball");

    let mut sess = SessionState {
        session_id: 88_511,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    mark_char_online(&ctx, &mut sess, char_id);
    let beyonce_id = ctx
        .bind_object(&mut sess, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    let kw = HashMap::new();

    let arm_invuln_and_safe_logoff = || {
        let now = ctx.now_unix();
        grant_location_change_invulnerability(&ctx, char_id, now + 60);
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
            st.safe_logoffs.insert(
                char_id,
                eve_db::SafeLogoffEntry {
                    char_id,
                    ship_id,
                    solar_system_id: sys_id,
                    started_unix: now,
                    complete_at_unix: now + 30,
                    completed: false,
                },
            );
        });
    };

    // Seed default crimewatch entry and PvE exploration registry once in setup before
    // snapshotting read-purity DB epochs, and arm active location-change invulnerability + safe logoff.
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let _ = cw.get_or_create(char_id);
    }
    arm_invuln_and_safe_logoff();
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Cloaked (stale CombatState flag with empty module_active_states) + warp-scrambled:
    //    CmdWarpToStuff & CmdWarpToStuffAutopilot via full `dispatch_rpc` reject without clearing
    //    `cs.is_cloaked`, cancelling invulnerability/safe-logoff, or mutating DB.
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = true;
        cs.is_covert_cloak = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.warp_scramble_strength = 1;
        cs.capacitor = 500.0;
        cs.max_capacitor = 500.0;
    });
    let epoch_0 = ctx.db.state_epoch();
    let tx_0 = ctx.db.tx_commit_count();
    for method in ["CmdWarpToStuff", "CmdWarpToStuffAutopilot"] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                method,
                &[PyRep::String("item".into()), PyRep::Integer(st_id)],
                &kw,
                None,
            )
            .await
            .expect_err(&format!("{method} must reject when scrambled + cloaked"));
        assert_eq!(err.code, "WarpScrambled");
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "{method} rejection must preserve is_cloaked");
        assert_eq!(cs.capacitor, 500.0, "{method} rejection must preserve capacitor");
        assert!(
            is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()),
            "{method} rejection must preserve location_change_invuln"
        );
        assert!(
            ctx.db
                .with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&char_id)),
            "{method} rejection must preserve active safe_logoff"
        );
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(
            !notifs
                .iter()
                .any(|n| n.event == "OnInvulnCancelled" || n.event == "OnSafeLogoffAborted"),
            "{method} rejection must not emit cancellation notifications: {notifs:?}"
        );
        assert_eq!(ctx.db.state_epoch(), epoch_0);
        assert_eq!(ctx.db.tx_commit_count(), tx_0);
    }

    // 2. Clear scramble while keeping active non-covert cloak:
    //    regular warp AND unseeded dungeon warp (`needs_pve_seed`) must reject with CloakedCannotWarp
    //    without clearing `is_cloaked`, invulnerability, safe logoff, or seeding PvE anomalies.
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 0;
    });
    for args in [
        vec![PyRep::String("item".into()), PyRep::Integer(st_id)],
        vec![PyRep::String("dungeon".into()), PyRep::Integer(99_999_001)],
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdWarpToStuff",
                &args,
                &kw,
                None,
            )
            .await
            .expect_err("non-covert cloak must reject CmdWarpToStuff");
        assert_eq!(err.code, "CloakedCannotWarp");
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "CloakedCannotWarp must preserve is_cloaked");
        let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(matches!(ball.mode, MovementMode::Stop));
        assert!(is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
        assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(!notifs
            .iter()
            .any(|n| n.event == "OnInvulnCancelled" || n.event == "OnSafeLogoffAborted"));
        assert_eq!(ctx.db.state_epoch(), epoch_0);
        assert_eq!(ctx.db.tx_commit_count(), tx_0);
    }

    // 2b. Unsynced non-covert cloak in `snap.module_active_states` (`cs.is_cloaked == false`)
    //     must ALSO reject with CloakedCannotWarp without mutating invuln, safe-logoff, or DB.
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.is_covert_cloak = false;
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: non_covert_cloak_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    let mod_epoch_0 = ctx.db.state_epoch();
    let mod_tx_0 = ctx.db.tx_commit_count();
    let mod_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
            None,
        )
        .await
        .expect_err("unsynced active non-covert cloak module must reject warp");
    assert_eq!(mod_err.code, "CloakedCannotWarp");
    assert!(is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    assert_eq!(ctx.db.state_epoch(), mod_epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), mod_tx_0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
        snap.items.remove(&cloak_mod_id);
    });

    // 3. Covert-cloak state with insufficient capacitor (`NotEnoughCapacitor`) must preserve
    //    cloak, capacitor, ball mode, invulnerability, safe-logoff, and DB state.
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |_| {});
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = true;
        cs.is_covert_cloak = true;
        cs.capacitor = 0.0;
    });
    let cap_epoch_0 = ctx.db.state_epoch();
    let cap_tx_0 = ctx.db.tx_commit_count();
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
            None,
        )
        .await
        .expect_err("0 capacitor must reject CmdWarpToStuff");
    assert_eq!(cap_err.code, "NotEnoughCapacitor");
    let cs_after_cap = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_cap.is_cloaked, "NotEnoughCapacitor must preserve is_cloaked");
    assert_eq!(cs_after_cap.capacitor, 0.0, "NotEnoughCapacitor must not mutate capacitor");
    let (_, cap_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(cap_ball.mode, MovementMode::Stop));
    assert!(is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let cap_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(!cap_notifs
        .iter()
        .any(|n| n.event == "OnInvulnCancelled" || n.event == "OnSafeLogoffAborted"));
    assert_eq!(ctx.db.state_epoch(), cap_epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), cap_tx_0);

    // 4. Valid uncloaked warp with sufficient capacitor succeeds cleanly AND aborts
    //    location_change_invuln and safe_logoff only when committed.
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.is_covert_cloak = false;
        cs.capacitor = 500.0;
    });
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(st_id)],
        &kw,
        None,
    )
    .await
    .expect("valid CmdWarpToStuff must succeed");
    let (_, warp_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(warp_ball.mode, MovementMode::Warp { .. }));
    assert!(!is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    assert!(!ctx.db.with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let commit_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        commit_notifs.iter().any(|n| n.event == "OnInvulnCancelled"),
        "committed warp must emit OnInvulnCancelled: {commit_notifs:?}"
    );
    assert!(
        commit_notifs.iter().any(|n| n.event == "OnSafeLogoffAborted"),
        "committed warp must emit OnSafeLogoffAborted: {commit_notifs:?}"
    );
}

#[tokio::test]
async fn tethered_warp_no_cap_refund_and_unsynced_covert_cloak_preserved_gh_1351() {
    let ctx = test_ctx();
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].corporation_id);
    let sys_id = 30_000_142_u32;
    let (st_id, st_pos) = ctx
        .sde
        .stations
        .iter()
        .find(|(_, st)| st.solar_system_id == sys_id)
        .map(|(&id, st)| (id, st.position))
        .expect("station in Jita");

    // Seed an operational quantum-cored Astrahus at origin within tether perimeter (~15 km)
    // and warp toward the far Jita station (~2 AU away).
    let astra_id = 1_001_999_888_777_i64;
    let astra_pos = st_pos + DVec3::new(300_000_000_000.0, 0.0, 0.0); // ~2.0 AU from target station
    let ship_start = astra_pos + DVec3::new(15_000.0, 0.0, 0.0);
    let ship_id = 88_512_001_i64;
    let cloak_mod_id = 88_512_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            astra_id,
            eve_db::UpwellStructureRecord {
                structure_id: astra_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Tether Warp Astrahus".into(),
                state: "ShieldVulnerable".into(),
                position: astra_pos.to_array(),
                shield_hp: 500.0,
                armor_hp: 500.0,
                hull_hp: 500.0,
                quantum_core_installed: true,
                fuel_blocks: 1000,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, ship_start);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Heavy mass (100_000_000 kg) ensures a partial warp from 10.0 GJ drains capacitor to 0.0 GJ
    // cleanly (> 150 km affordable) so any +25% tether refund (+250 GJ) would be unmistakable.
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: ship_start,
            velocity: DVec3::ZERO,
            max_velocity: 320.0,
            inertia: 2.0,
            mass: 100_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn ship ball");
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.max_capacitor = 1_000.0;
        cs.capacitor = 1_000.0;
    });
    crate::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;
    assert!(
        sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered,
        "ship at cored friendly Astrahus must start tethered"
    );

    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 10.0;
    });
    let mut sess = SessionState {
        session_id: 88_512,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let beyonce_id = ctx
        .bind_object(&mut sess, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(st_id)],
        &kw,
        None,
    )
    .await
    .expect("tethered partial warp must succeed");

    let (_, warp_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(warp_ball.mode, MovementMode::Warp { .. }));
    let cs_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        !cs_after.is_tethered,
        "warping must break tether cleanly"
    );
    assert!(
        cs_after.capacitor.abs() < 1e-6,
        "tethered warp must NOT refund +25% max capacitor (expected 0.0, got {})",
        cs_after.capacitor
    );

    // Unsynced active Covert Ops cloak (`snap.module_active_states` active while `cs.is_cloaked == false`)
    // must warp without decloaking and reconcile `cs.is_cloaked && cs.is_covert_cloak`.
    let covert_tid = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| {
            t.group_id == 330
                && t.effect_ids.iter().any(|&eid| {
                    ctx.sde
                        .effects
                        .get(&eid)
                        .is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe)
                })
        })
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .unwrap_or(11_578);
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.is_covert_cloak = false;
        cs.capacitor = 1_000.0;
        cs.max_capacitor = 1_000.0;
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: covert_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(st_id)],
        &kw,
        None,
    )
    .await
    .expect("unsynced Covert Ops cloak warp must succeed");

    let (_, cov_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(cov_ball.mode, MovementMode::Warp { .. }));
    let cov_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        cov_cs.is_cloaked && cov_cs.is_covert_cloak,
        "unsynced Covert Ops cloak must remain cloaked after warp (is_cloaked={}, is_covert_cloak={})",
        cov_cs.is_cloaked,
        cov_cs.is_covert_cloak
    );
    assert!(ctx
        .db
        .with_snapshot(|s| s.module_active_states.contains(&cloak_mod_id)));
}

#[tokio::test]
async fn committed_warp_emits_gate_cloak_and_invuln_cancelled_while_rejected_warp_preserves_them_gh_1351() {
    let ctx = test_ctx();
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let sys_id = 30_000_142_u32;
    let (st_id, st_pos) = ctx
        .sde
        .stations
        .iter()
        .find(|(_, st)| st.solar_system_id == sys_id)
        .map(|(&id, st)| (id, st.position))
        .expect("station in Jita");
    let ship_id = 88_513_001_i64;
    let far_pos = st_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, far_pos);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .modify_combat_state(ship_id, |cs| {
            cs.is_cloaked = true;
            cs.gate_cloak_remaining_ticks = 30;
            cs.undock_invulnerable_remaining_ticks = 15;
            cs.warp_scramble_strength = 1;
            cs.capacitor = 500.0;
            cs.max_capacitor = 500.0;
        })
        .await
        .expect("seed gate cloak + undock invuln combat state");

    let mut sess = SessionState {
        session_id: 88_513,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    mark_char_online(&ctx, &mut sess, char_id);
    let beyonce_id = ctx
        .bind_object(&mut sess, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    let kw = HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Rejected warp while scrambled must NOT emit OnJumpCloakCancelled/OnInvulnCancelled
    //    and must preserve gate cloak and undock invulnerability ticks.
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &kw,
            None,
        )
        .await
        .expect_err("scrambled warp must fail");
    assert_eq!(err.code, "WarpScrambled");
    let rejected_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !rejected_notifs
            .iter()
            .any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"),
        "rejected warp must not emit cloak/invuln cancellation notifications: {rejected_notifs:?}"
    );
    let cs_after_reject = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_reject.is_cloaked);
    assert_eq!(cs_after_reject.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_after_reject.undock_invulnerable_remaining_ticks, 15);

    // 2. Unscrambled committed warp breaks both timers and emits BOTH notifications.
    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 0;
    });
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(st_id)],
        &kw,
        None,
    )
    .await
    .expect("gate-cloaked undock-invuln warp must succeed");

    let commit_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        commit_notifs.iter().any(|n| n.event == "OnJumpCloakCancelled"),
        "committed warp must emit OnJumpCloakCancelled: {commit_notifs:?}"
    );
    assert!(
        commit_notifs.iter().any(|n| n.event == "OnInvulnCancelled"),
        "committed warp must emit OnInvulnCancelled: {commit_notifs:?}"
    );
    let cs_after_commit = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_after_commit.is_cloaked);
    assert_eq!(cs_after_commit.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_after_commit.undock_invulnerable_remaining_ticks, 0);
}

#[tokio::test]
async fn fleet_warp_skips_caller_and_tourney_restrictions_preserve_invuln_and_safelogoff_gh_1351() {
    let ctx = test_ctx();
    let (_fleet_id, mut fc_sess) =
        crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::seed_multi_member_fleet(&ctx).await;
    let fc_char_id = fc_sess.character_id.unwrap();
    let fc_ship_id = fc_sess.ship_id.unwrap();
    let sys_id = 30_000_142_u32;
    fc_sess.station_id = None;
    fc_sess.solar_system_id = Some(sys_id);
    mark_char_online(&ctx, &mut fc_sess, fc_char_id);
    let beyonce_id = ctx
        .bind_object(&mut fc_sess, "beyonce", sys_id as i64)
        .expect("bind beyonce for FC");
    let st_id = ctx
        .sde
        .stations
        .iter()
        .find(|(_, st)| st.solar_system_id == sys_id && st.position.length() > 1_000_000.0)
        .map(|(&id, _)| id)
        .expect("distant station in Jita");

    let arm_fc_timers = || {
        let now = ctx.now_unix();
        grant_location_change_invulnerability(&ctx, fc_char_id, now + 60);
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
            st.safe_logoffs.insert(
                fc_char_id,
                eve_db::SafeLogoffEntry {
                    char_id: fc_char_id,
                    ship_id: fc_ship_id,
                    solar_system_id: sys_id,
                    started_unix: now,
                    complete_at_unix: now + 30,
                    completed: false,
                },
            );
        });
    };
    arm_fc_timers();
    let _ = ctx.drain_notifications_for_char(fc_char_id);

    // 1. Scramble FC (`100_000_001`) while fleetmate (`100_000_002`) warps:
    //    `handle_fleet_warp` returns Ok(keyval) excluding `fc_char_id` from `warpedMemberIDs`,
    //    so FC must keep location_change_invuln and SafeLogoffEntry.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_update_combat_state(fc_ship_id, |cs| {
        cs.warp_scramble_strength = 1;
    });
    let mut fleet_kw = HashMap::new();
    fleet_kw.insert("fleet".to_string(), PyRep::Bool(true));
    let fw_res = ctx
        .dispatch_rpc(
            &mut fc_sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &fleet_kw,
            None,
        )
        .await
        .expect("fleet warp skipping scrambled FC must still succeed for fleetmate");
    let warped_ids: Vec<i64> = fw_res
        .dict_get("warpedMemberIDs")
        .and_then(|v| match v {
            PyRep::List(items) => Some(items.iter().filter_map(PyRep::as_i64).collect()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        !warped_ids.contains(&fc_char_id),
        "scrambled FC must be excluded from warpedMemberIDs: {warped_ids:?}"
    );
    assert!(
        !warped_ids.is_empty(),
        "unscrambled fleetmate should still fleet-warp: {warped_ids:?}"
    );
    assert!(
        is_location_change_invulnerable(&ctx, fc_char_id, ctx.now_unix()),
        "FC excluded from warpedMemberIDs must keep location_change_invuln"
    );
    assert!(
        ctx.db
            .with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&fc_char_id)),
        "FC excluded from warpedMemberIDs must keep active safe_logoff"
    );
    let skipped_notifs = ctx.drain_notifications_for_char(fc_char_id);
    assert!(
        !skipped_notifs
            .iter()
            .any(|n| n.event == "OnInvulnCancelled" || n.event == "OnSafeLogoffAborted"),
        "FC excluded from warpedMemberIDs must not receive cancellation notices: {skipped_notifs:?}"
    );

    // 2. Tourney `move_restrict` and `warp_restrict` reject both solo & fleet CmdWarpToStuff
    //    WITHOUT breaking FC location_change_invuln or safe_logoff.
    sys_handle.try_update_combat_state(fc_ship_id, |cs| {
        cs.warp_scramble_strength = 0;
    });
    ctx.db.with_mut_snapshot(|snap| {
        let mut tm = eve_db::TourneyMatch::new(9_101, 1, i64::from(sys_id), 100, 600, 120, 1);
        tm.state = eve_db::TourneyMatchState::InProgress;
        tm.move_restrict = true;
        tm.warp_restrict = false;
        tm.red_players.push(eve_db::TourneyPilot {
            character_id: fc_char_id,
            ship_type_id: 587,
            ship_id: fc_ship_id,
            points: 10,
        });
        snap.tourney_state.matches.insert(9_101, tm);
    });
    for (kw_ref, label) in [(&HashMap::new(), "solo"), (&fleet_kw, "fleet")] {
        let err = ctx
            .dispatch_rpc(
                &mut fc_sess,
                "beyonce",
                Some(beyonce_id),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(st_id)],
                kw_ref,
                None,
            )
            .await
            .expect_err(&format!("{label} CmdWarpToStuff must reject MoveRestrictedInTourney"));
        assert_eq!(err.code, "MoveRestrictedInTourney");
        assert!(is_location_change_invulnerable(&ctx, fc_char_id, ctx.now_unix()));
        assert!(ctx
            .db
            .with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&fc_char_id)));
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.tourney_state.matches.get_mut(&9_101).unwrap().warp_restrict = true;
    });
    let warp_err = ctx
        .dispatch_rpc(
            &mut fc_sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &fleet_kw,
            None,
        )
        .await
        .expect_err("fleet CmdWarpToStuff must reject WarpRestrictedInTourney before handle_fleet_warp");
    assert_eq!(warp_err.code, "WarpRestrictedInTourney");
    assert!(is_location_change_invulnerable(&ctx, fc_char_id, ctx.now_unix()));
    assert!(ctx
        .db
        .with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&fc_char_id)));

    // 3. Clear tourney match: fleet warp including `fc_char_id` clears both invuln and safe logoff.
    ctx.db.with_mut_snapshot(|snap| {
        snap.tourney_state.matches.remove(&9_101);
    });
    let fw_ok = ctx
        .dispatch_rpc(
            &mut fc_sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &fleet_kw,
            None,
        )
        .await
        .expect("unrestricted fleet warp including FC must succeed");
    let warped_ok_ids: Vec<i64> = fw_ok
        .dict_get("warpedMemberIDs")
        .and_then(|v| match v {
            PyRep::List(items) => Some(items.iter().filter_map(PyRep::as_i64).collect()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        warped_ok_ids.contains(&fc_char_id),
        "unscrambled FC must appear in warpedMemberIDs: {warped_ok_ids:?}"
    );
    assert!(
        !is_location_change_invulnerable(&ctx, fc_char_id, ctx.now_unix()),
        "fleet warp including FC must break location_change_invuln"
    );
    assert!(
        !ctx.db
            .with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&fc_char_id)),
        "fleet warp including FC must abort active safe_logoff"
    );
    let fw_notifs = ctx.drain_notifications_for_char(fc_char_id);
    assert!(
        fw_notifs.iter().any(|n| n.event == "OnInvulnCancelled"),
        "fleet warp including FC must emit OnInvulnCancelled: {fw_notifs:?}"
    );
    assert!(
        fw_notifs.iter().any(|n| n.event == "OnSafeLogoffAborted"),
        "fleet warp including FC must emit OnSafeLogoffAborted: {fw_notifs:?}"
    );
}

#[tokio::test]
async fn bookmark_validation_runs_before_ship_movement_and_preserves_read_purity_gh_1408() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_408_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    mark_char_online(&ctx, &mut sess, char_id);

    spawn_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::ZERO);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .set_combat_state(
            ship_id,
            CombatState {
                warp_scramble_strength: 2,
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let kind_bm = PyRep::String("bookmark".into());
    let valid_bm_id = 900_408_1_i64;
    let expired_bm_id = 900_408_2_i64;
    let zarzakh_bm_id = 900_408_3_i64;
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            valid_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: valid_bm_id,
                owner_id: char_id,
                label: "ValidFar".into(),
                solar_system_id: sys_id,
                x: 1e11,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            expired_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: expired_bm_id,
                owner_id: char_id,
                label: "ExpiredFar".into(),
                solar_system_id: sys_id,
                x: 1e11,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmark_expiries.insert(expired_bm_id, now_unix);
        snap.bookmarks.insert(
            zarzakh_bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: zarzakh_bm_id,
                owner_id: char_id,
                label: "ZarzakhBM".into(),
                solar_system_id: SOLAR_SYSTEM_ZARZAKH,
                x: 1e11,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    // Warm up lazy crimewatch entry creation once so baseline `epoch_0`/`tx_0` measures only RPC read-purity.
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let _ = cw.get_or_create(char_id);
    }

    let epoch_0 = ctx.db.state_epoch();
    let tx_0 = ctx.db.tx_commit_count();

    // 1. Missing bookmark reports BookmarkNotFound even while scrambled and cloaked (zero DB/cloak mutation).
    let missing_err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdWarpToStuff",
        &[kind_bm.clone(), PyRep::Integer(999_408)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(missing_err.code, "BookmarkNotFound");
    assert_eq!(ctx.db.state_epoch(), epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), tx_0);
    assert!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);

    // 2. Expired bookmark reports BookmarkExpired even while scrambled.
    let exp_err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdWarpToStuff",
        &[kind_bm.clone(), PyRep::Integer(expired_bm_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(exp_err.code.to_ascii_lowercase(), "bookmarkexpired");
    assert_eq!(ctx.db.state_epoch(), epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), tx_0);

    // 3. Valid bookmark while scrambled reports WarpScrambled; while non-covert cloaked & unscrambled reports CloakedCannotWarp.
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[kind_bm.clone(), PyRep::Integer(valid_bm_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
    sys_h
        .set_combat_state(
            ship_id,
            CombatState {
                warp_scramble_strength: 0,
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[kind_bm.clone(), PyRep::Integer(valid_bm_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CloakedCannotWarp"
    );
    assert_eq!(ctx.db.state_epoch(), epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), tx_0);
    assert!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);

    // 4. Zarzakh bookmark warp ban reports DunBlacklistCannotWarp even if scrambled.
    spawn_ship_in_space(&ctx, char_id, ship_id, 587, SOLAR_SYSTEM_ZARZAKH, DVec3::ZERO);
    let zz_h = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
    zz_h.set_combat_state(
        ship_id,
        CombatState {
            warp_scramble_strength: 2,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sess.solar_system_id = Some(SOLAR_SYSTEM_ZARZAKH);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdWarpToStuff",
            &[kind_bm, PyRep::Integer(zarzakh_bm_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DunBlacklistCannotWarp"
    );
}

#[tokio::test]
async fn cmd_stargate_jump_lazy_wake_and_rejection_guards_gh_1413() {
    let pick_highsec_gate = |ctx: &ServerContext| {
        ctx.sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != j.to_solar_system_id
                    && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx
                        .sde
                        .solar_systems
                        .get(&j.from_solar_system_id)
                        .is_some_and(|s| s.security_status >= 0.45 && s.region_id != 10_000_070)
                    && ctx
                        .sde
                        .solar_systems
                        .get(&j.to_solar_system_id)
                        .is_some_and(|s| s.security_status >= 0.45 && s.region_id != 10_000_070)
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a round-trip highsec stargate")
    };

    // Part 1: Direct CmdStargateJump within 2,500 m of a lazy-unloaded highsec gate succeeds
    // without prior CmdWarpToStuff/CmdFollowBall (#1413).
    {
        let ctx = test_ctx();
        ctx.systems.set_lazy_stargate_activation(true);
        let gate = pick_highsec_gate(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001_i64;
        let ship_id = 895_413_001_i64;
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position,
        );
        let src_h = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("source system loaded");
        assert!(!ctx.systems.is_system_loaded(gate.to_solar_system_id));
        assert_eq!(
            src_h
                .stargate_activation_state(gate.stargate_id)
                .map(|s| s.activation_state),
            Some(eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED)
        );

        let mut sess = SessionState::new(98_413_001);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        let res = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
        )
        .await
        .expect("CmdStargateJump must be handled")
        .expect("direct CmdStargateJump within 2,500 m must wake lazy-unloaded gate (#1413)");
        let PyRep::KeyVal(kv) = res else {
            panic!("expected KeyVal from CmdStargateJump, got {res:?}");
        };
        assert_eq!(
            kv.get("solarSystemID").and_then(|v| v.as_i64()),
            Some(gate.to_solar_system_id as i64)
        );
        assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));
        assert!(ctx.systems.is_system_loaded(gate.to_solar_system_id));
        assert_eq!(
            src_h
                .stargate_activation_state(gate.stargate_id)
                .map(|s| s.activation_state),
            Some(eve_destiny::STARGATE_ACTIVATION_STATE_OPEN)
        );
    }

    // Part 2: Pre-jump rejection guards (out-of-range, docked, cloaked, lance-disrupted,
    // session-change timer, kicked session) must NOT wake a lazy-unloaded destination.
    {
        let ctx = test_ctx();
        ctx.systems.set_lazy_stargate_activation(true);
        let gate = pick_highsec_gate(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001_i64;
        let ship_id = 895_413_002_i64;
        let gate_radius = ctx
            .sde
            .celestials_in_system_slice(gate.from_solar_system_id)
            .iter()
            .find(|c| c.celestial_id == gate.stargate_id)
            .map(|c| c.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(14_051.0);
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position + DVec3::new(gate_radius + 40.0 + 3_000.0, 0.0, 0.0),
        );
        let src_h = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("source system loaded");
        let mut sess = SessionState::new(98_413_002);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        let assert_still_unloaded = || {
            assert!(!ctx.systems.is_system_loaded(gate.to_solar_system_id));
            assert_eq!(
                src_h
                    .stargate_activation_state(gate.stargate_id)
                    .map(|s| s.activation_state),
                Some(eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED)
            );
        };

        // Out of range
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "NotCloseEnoughToJump");
        assert_still_unloaded();

        // Move within 2,500 m
        assert!(src_h.set_ball_position(ship_id, gate.position).await);

        // Docked
        sess.station_id = Some(60_003_760);
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "ShipDocked");
        assert_still_unloaded();
        sess.station_id = None;

        // Cloaked (active non-gate cloak)
        src_h.try_update_combat_state(ship_id, |cs| {
            cs.is_cloaked = true;
            cs.is_covert_cloak = false;
            cs.gate_cloak_remaining_ticks = 0;
        });
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "ShipCloaked");
        assert_still_unloaded();

        // Disruptive Lance debuff
        src_h.try_update_combat_state(ship_id, |cs| {
            cs.is_cloaked = false;
            cs.lance_disrupted_remaining_s = 10.0;
        });
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "DisruptiveLanceDebuffActive");
        assert_still_unloaded();
        src_h.try_update_combat_state(ship_id, |cs| {
            cs.lance_disrupted_remaining_s = 0.0;
        });

        // Session-change timer active
        let now_unix = ctx.now_unix();
        record_session_change_timer(
            &ctx,
            char_id,
            now_unix,
            gate.from_solar_system_id,
            0,
            "test_guard",
        );
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "SessionChangeInProgress");
        assert_still_unloaded();
        ctx.session_change_timers.lock().unwrap().remove(&char_id);

        // Displaced/kicked session lease must reject before waking destination
        ctx.db
            .acquire_online_lock(1, sess.session_id, Some(char_id), true, ctx.now_unix())
            .expect("initial session acquires lease");
        ctx.db
            .acquire_online_lock(1, 999_413, Some(char_id), true, ctx.now_unix())
            .expect("other session displaces current session");
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "SessionKicked");
        assert_still_unloaded();
    }

    // Part 3: Explicitly closed gate (lazy disabled, or loaded destination while CLOSED) and
    // mid-transition ACTIVATING gate still reject with GateClosed without loading destination.
    {
        let ctx = test_ctx();
        let gate = pick_highsec_gate(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001_i64;
        let ship_id = 895_413_003_i64;
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position,
        );
        let src_h = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("source system loaded");
        let mut sess = SessionState::new(98_413_003);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        // Explicitly closed with lazy activation disabled -> GateClosed, destination stays unloaded.
        assert!(src_h.set_stargate_activation_state(
            gate.stargate_id,
            eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED,
            0,
            0,
        ));
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "GateClosed");
        assert!(!ctx.systems.is_system_loaded(gate.to_solar_system_id));

        // Explicitly closed under lazy activation when destination is already loaded -> GateClosed.
        ctx.systems.set_lazy_stargate_activation(true);
        let _ = ctx.systems.get_or_spawn(gate.to_solar_system_id);
        assert!(ctx.systems.is_system_loaded(gate.to_solar_system_id));
        assert!(src_h.set_stargate_activation_state(
            gate.stargate_id,
            eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED,
            0,
            0,
        ));
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "GateClosed");

        // Mid-transition ACTIVATING gate -> GateClosed.
        let now_ms = ctx.now_machonet_wallclock_ms();
        assert!(src_h.set_stargate_activation_state(
            gate.stargate_id,
            eve_destiny::STARGATE_ACTIVATION_STATE_ACTIVATING,
            eve_destiny::STARGATE_ACTIVATION_TRANSITION_MS,
            now_ms.saturating_add(eve_destiny::STARGATE_ACTIVATION_TRANSITION_MS),
        ));
        let err = handle(
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
        .unwrap_err();
        assert_eq!(err.code, "GateClosed");
    }
}
