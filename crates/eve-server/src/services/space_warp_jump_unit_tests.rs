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
    crate::router::session_transition::clear_all_session_change_timers(&ctx);
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
    sys_handle.insert_active_warp_for_test(ship_id, char_id);

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
    assert!(sys_handle.is_active_warp(ship_id));

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
    assert!(!sys_handle.is_active_warp(ship_id));
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
    crate::router::session_transition::clear_all_session_change_timers(&ctx);
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
    crate::router::session_transition::clear_all_session_change_timers(&ctx);

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
    sys_handle.insert_active_warp_for_test(ship_id, st_id);
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
        !sys_handle.is_active_warp(ship_id),
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
    crate::router::session_transition::seed_session_change_timer(
        &ctx,
        char_id,
        crate::router::session_transition::SessionChangeTimerState {
            expires_unix: ctx.now_unix() + 20,
            system_id: sys_id,
            started_tick: 0,
            reason: "test".into(),
        },
    );
    assert!(
        prepare_dock_warp_settle(&ctx, &sess, "stationSvc", "Dock", &[PyRep::Integer(st_id)])
            .await
            .is_none(),
        "SessionChangeInProgress must return None immediately"
    );
    crate::router::session_transition::clear_all_session_change_timers(&ctx);
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
    sys_handle.insert_active_warp_for_test(ship_id, char_id);
    stop_arriving_warp_for_dock(&ctx, &sys_handle, &titan, keepstar_pos, keepstar_radius);
    let after_inside_stop = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert_eq!(after_inside_stop.mode, MovementMode::Stop);
    assert!(
        after_inside_stop.position.distance(inside_warp_pos) < 1e-6,
        "stop_arriving_warp_for_dock must not snap when Titan is already inside max_dock_dist"
    );
    assert!(!sys_handle.is_active_warp(ship_id));
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
    crate::router::session_transition::clear_all_session_change_timers(&ctx);

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
    crate::router::session_transition::clear_all_session_change_timers(&ctx);
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
                    scene_scope: None,
                },
                TypedNotification {
                    event: "DoSimClockRebase".into(),
                    args: vec![PyRep::Tuple(vec![PyRep::Integer(100), PyRep::Integer(100)])],
                    service: None,
                    scene_scope: None,
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
        sys_handle.try_update_combat_state(ship_id, |cs| {
            cs.undock_invulnerable_remaining_ticks = 25;
        });
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
    // snapshotting read-purity DB epochs, and arm active arrival/undock invulnerability + safe logoff.
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
        assert_eq!(
            cs.undock_invulnerable_remaining_ticks, 25,
            "{method} rejection must preserve arrival/undock invuln"
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
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 25);
        let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(matches!(ball.mode, MovementMode::Stop));
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
    assert_eq!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        25
    );
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
    assert_eq!(cs_after_cap.undock_invulnerable_remaining_ticks, 25);
    let (_, cap_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(cap_ball.mode, MovementMode::Stop));
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.safe_logoffs.contains_key(&char_id)));
    let cap_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(!cap_notifs
        .iter()
        .any(|n| n.event == "OnInvulnCancelled" || n.event == "OnSafeLogoffAborted"));
    assert_eq!(ctx.db.state_epoch(), cap_epoch_0);
    assert_eq!(ctx.db.tx_commit_count(), cap_tx_0);

    // 4. Valid uncloaked warp with sufficient capacitor succeeds cleanly AND aborts
    //    arrival/undock invuln and safe_logoff only when committed.
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
    assert_eq!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        0
    );
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

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let arm_fc_timers = || {
        let now = ctx.now_unix();
        sys_handle.try_update_combat_state(fc_ship_id, |cs| {
            cs.undock_invulnerable_remaining_ticks = 25;
        });
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
    //    so FC must keep arrival/undock invuln and SafeLogoffEntry.
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
    assert_eq!(
        sys_handle
            .get_combat_state(fc_ship_id)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        25,
        "FC excluded from warpedMemberIDs must keep arrival/undock invuln"
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
    //    WITHOUT breaking FC arrival/undock invuln or safe_logoff.
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
        assert_eq!(
            sys_handle
                .get_combat_state(fc_ship_id)
                .await
                .unwrap()
                .unwrap()
                .undock_invulnerable_remaining_ticks,
            25
        );
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
    assert_eq!(
        sys_handle
            .get_combat_state(fc_ship_id)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        25
    );
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
    assert_eq!(
        sys_handle
            .get_combat_state(fc_ship_id)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        0,
        "fleet warp including FC must break arrival/undock invuln"
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
        crate::router::session_transition::clear_session_change_timer(&ctx, char_id);

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

#[tokio::test]
async fn cmd_stargate_jump_zarzakh_toll_and_db_prevalidation_guards_gh_1429() {
    let ctx = test_ctx();
    ctx.systems.set_lazy_stargate_activation(true);
    let gate = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH
                && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
        })
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must include a stargate leading into Zarzakh (30_100_000)");

    let char_id = 90_000_001_i64;
    let ship_id = 895_429_001_i64;
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position,
    );
    let underfunded = eve_db::IskCents::from_isk(5_000);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = None;
        ch.balance = underfunded;
    });
    let src_h = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("source system loaded");
    assert!(!ctx.systems.is_system_loaded(SOLAR_SYSTEM_ZARZAKH));
    assert_eq!(
        src_h
            .stargate_activation_state(gate.stargate_id)
            .map(|s| s.activation_state),
        Some(eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED)
    );

    let mut sess = SessionState::new(98_429_001);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(gate.from_solar_system_id);

    let v24_args = [
        PyRep::Integer(gate.stargate_id),
        PyRep::Integer(gate.destination_stargate_id),
        PyRep::Integer(ship_id),
    ];
    let empty_kw = HashMap::new();
    let missing_char_id = 99_429_999_i64;
    let epoch_0 = ctx.db.state_epoch();
    let journal_len_0 = ctx.db.with_snapshot(|s| s.wallet_journal.len());

    let assert_untouched = |expected_bal: eve_db::IskCents, expected_epoch: u64, sess: &SessionState| {
        assert!(
            !ctx.systems.is_system_loaded(SOLAR_SYSTEM_ZARZAKH),
            "rejected Zarzakh jump must NOT lazily load 30_100_000"
        );
        assert_eq!(
            src_h
                .stargate_activation_state(gate.stargate_id)
                .map(|s| s.activation_state),
            Some(eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED),
            "rejected Zarzakh jump must leave source stargate CLOSED"
        );
        assert_eq!(
            ctx.db.state_epoch(),
            expected_epoch,
            "rejected Zarzakh jump must NOT mutate EveDb / state_epoch"
        );
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.characters[&char_id].balance, expected_bal);
            assert_eq!(s.characters[&char_id].solar_system_id, gate.from_solar_system_id);
            assert_eq!(s.wallet_journal.len(), journal_len_0);
        });
        assert_eq!(sess.solar_system_id, Some(gate.from_solar_system_id));
    };

    // 1. Declined Zarzakh toll -> ZarzakhGateTollDeclined; Zarzakh stays unloaded, gate stays CLOSED, DB untouched.
    let mut decline_kw = HashMap::new();
    decline_kw.insert("confirmToll".to_string(), PyRep::Bool(false));
    let err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &v24_args,
        &decline_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "ZarzakhGateTollDeclined");
    assert_eq!(
        err.client_error.as_deref(),
        Some(&eve_proto::ClientError::CustomInfo(err.message.clone()))
    );
    assert_ne!(err.message, err.code);
    assert_untouched(underfunded, epoch_0, &sess);

    // 2. Unconfirmed V24 3-arg jump -> AskConfirmZarzakhGateToll; Zarzakh stays unloaded, gate stays CLOSED, DB untouched.
    let err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &v24_args,
        &empty_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "AskConfirmZarzakhGateToll");
    assert!(err.message.contains("10000 ISK"));
    assert_eq!(
        err.client_error.as_deref(),
        Some(&eve_proto::ClientError::CustomInfo(err.message.clone()))
    );
    assert_untouched(underfunded, epoch_0, &sess);

    // 3. Confirmed via stored quote but underfunded (5,000 < 10,000 ISK) -> InsufficientFundsForZarzakhGateToll;
    //    Zarzakh stays unloaded, gate stays CLOSED, DB untouched.
    let err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &v24_args,
        &empty_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InsufficientFundsForZarzakhGateToll");
    assert_eq!(
        err.client_error.as_deref(),
        Some(&eve_proto::ClientError::user(
            eve_proto::UserErrorKey::NotEnoughMoney
        ))
    );
    assert_untouched(underfunded, epoch_0, &sess);

    // 4. Explicitly confirmed (`confirmToll: true`) but underfunded -> InsufficientFundsForZarzakhGateToll;
    //    Zarzakh stays unloaded, gate stays CLOSED, DB untouched.
    let mut confirm_kw = HashMap::new();
    confirm_kw.insert("confirmToll".to_string(), PyRep::Bool(true));
    let err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &v24_args,
        &confirm_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InsufficientFundsForZarzakhGateToll");
    assert_eq!(
        err.client_error.as_deref(),
        Some(&eve_proto::ClientError::user(
            eve_proto::UserErrorKey::NotEnoughMoney
        ))
    );
    assert_untouched(underfunded, epoch_0, &sess);

    // 5. Missing DB character (`check_stargate_entry_db_state` -> CharacterNotFound) rejects BEFORE lazy wake.
    let missing_ship_id = 895_429_999_i64;
    src_h.try_spawn_ball_with_combat(
        Ball {
            id: missing_ship_id,
            type_id: 587,
            position: gate.position,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
    let mut missing_sess = SessionState::new(98_429_999);
    missing_sess.user_id = Some(999_429);
    missing_sess.character_id = Some(missing_char_id);
    missing_sess.ship_id = Some(missing_ship_id);
    missing_sess.solar_system_id = Some(gate.from_solar_system_id);
    let err = handle(
        &ctx,
        &mut missing_sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &confirm_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CharacterNotFound");
    assert_untouched(underfunded, epoch_0, &sess);

    // Direct unit checks on `check_stargate_entry_db_state` concurrent-dock/move guards (read-only).
    assert!(
        check_stargate_entry_db_state(&ctx, char_id, Some((None, None, gate.from_solar_system_id)))
            .is_ok()
    );
    assert_eq!(
        check_stargate_entry_db_state(&ctx, char_id, Some((None, None, gate.from_solar_system_id + 1)))
            .unwrap_err()
            .code,
        "SystemTransitionFailed"
    );
    assert_eq!(ctx.db.state_epoch(), epoch_0);

    // 6. Fund character, confirm two-step V24 quote, and verify lazy wake + toll debit + clean arrival.
    let funded = eve_db::IskCents::from_isk(100_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = funded;
    });
    let epoch_funded = ctx.db.state_epoch();

    // Direct read-only check on `prevalidate_zarzakh_gate_toll` for explicit confirm does not debit or bump epoch.
    let (_, ship_ball) = src_h
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball present");
    assert_eq!(
        crate::services::space_navigation_advanced::prevalidate_zarzakh_gate_toll(
            &ctx,
            char_id,
            ship_id,
            gate.stargate_id,
            gate.destination_stargate_id,
            &ship_ball,
            &v24_args,
            &confirm_kw,
        )
        .expect("funded explicit confirm pre-validation succeeds"),
        Some(eve_db::IskCents::from_isk(10_000))
    );
    assert_untouched(funded, epoch_funded, &sess);

    // Step 1 of 2-step V24 jump: quotes toll without waking Zarzakh or mutating DB.
    let err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &v24_args,
        &empty_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "AskConfirmZarzakhGateToll");
    assert_eq!(
        err.client_error.as_deref(),
        Some(&eve_proto::ClientError::CustomInfo(err.message.clone()))
    );
    assert_untouched(funded, epoch_funded, &sess);

    // Step 2 of 2-step V24 jump: consumes quote, lazily wakes Zarzakh, opens gate, debits 10,000 ISK.
    let res = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &v24_args,
        &empty_kw,
    )
    .await
    .expect("CmdStargateJump must be handled")
    .expect("funded confirmed direct CmdStargateJump into Zarzakh must succeed (#1429)");
    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal from CmdStargateJump, got {res:?}");
    };
    assert_eq!(
        kv.get("solarSystemID").and_then(|v| v.as_i64()),
        Some(SOLAR_SYSTEM_ZARZAKH as i64)
    );
    assert_eq!(sess.solar_system_id, Some(SOLAR_SYSTEM_ZARZAKH));
    assert!(ctx.systems.is_system_loaded(SOLAR_SYSTEM_ZARZAKH));
    assert_eq!(
        src_h
            .stargate_activation_state(gate.stargate_id)
            .map(|s| s.activation_state),
        Some(eve_destiny::STARGATE_ACTIVATION_STATE_OPEN)
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.characters[&char_id].balance,
            eve_db::IskCents::from_isk(90_000)
        );
        assert_eq!(s.characters[&char_id].solar_system_id, SOLAR_SYSTEM_ZARZAKH);
        assert!(s.wallet_journal.iter().any(|j| {
            j.character_id == char_id
                && j.entry_type == "zarzakh_gate_toll"
                && j.amount == eve_db::IskCents::from_isk(10_000)
        }));
    });
}

#[test]
fn zarzakh_toll_prevalidate_and_process_emit_typed_client_errors_gh_1442() {
    use crate::services::space_navigation_advanced::{
        prevalidate_zarzakh_gate_toll, process_zarzakh_gate_toll_with_amount,
    };

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let missing_char_id = 99_442_999_i64;
    let ship_id = 895_442_001_i64;
    let from_gate_id = 50_442_001_i64;
    let to_gate_id = 50_442_002_i64;
    let underfunded = eve_db::IskCents::from_isk(5_000);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = None;
        ch.balance = underfunded;
        snap.characters.remove(&missing_char_id);
    });
    let journal_len_0 = ctx.db.with_snapshot(|s| s.wallet_journal.len());

    let ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let v24_args = [
        PyRep::Integer(from_gate_id),
        PyRep::Integer(to_gate_id),
        PyRep::Integer(ship_id),
    ];
    let empty_kw = HashMap::new();
    let mut decline_kw = HashMap::new();
    decline_kw.insert("confirmToll".to_string(), PyRep::Bool(false));
    let mut confirm_kw = HashMap::new();
    confirm_kw.insert("confirmToll".to_string(), PyRep::Bool(true));

    let assert_typed_custom_info = |err: &eve_proto::BridgeRpcError, expected_code: &str| {
        assert_eq!(err.code, expected_code);
        assert_ne!(
            err.message, err.code,
            "{expected_code} must carry a descriptive message rather than echoing raw code"
        );
        assert!(
            err.client_error.is_some(),
            "{expected_code} must populate typed client_error"
        );
        let expected_ce = eve_proto::ClientError::CustomInfo(err.message.clone());
        assert_eq!(err.client_error.as_deref(), Some(&expected_ce));
        assert_eq!(err.to_client_error("beyonce", "CmdStargateJump"), expected_ce);
    };
    let assert_typed_not_enough_money = |err: &eve_proto::BridgeRpcError| {
        assert_eq!(err.code, "InsufficientFundsForZarzakhGateToll");
        assert!(err.message.contains("10000.00 ISK"));
        assert!(
            err.client_error.is_some(),
            "InsufficientFundsForZarzakhGateToll must populate typed client_error"
        );
        let expected_ce = eve_proto::ClientError::user(eve_proto::UserErrorKey::NotEnoughMoney);
        assert_eq!(err.client_error.as_deref(), Some(&expected_ce));
        assert_eq!(err.to_client_error("beyonce", "CmdStargateJump"), expected_ce);
    };

    // --- 1. prevalidate_zarzakh_gate_toll ---
    let err = prevalidate_zarzakh_gate_toll(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &decline_kw,
    )
    .unwrap_err();
    assert_typed_custom_info(&err, "ZarzakhGateTollDeclined");

    let err = prevalidate_zarzakh_gate_toll(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &empty_kw,
    )
    .unwrap_err();
    assert_typed_custom_info(&err, "AskConfirmZarzakhGateToll");
    assert!(err.message.contains("10000 ISK"));

    let err = prevalidate_zarzakh_gate_toll(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &empty_kw,
    )
    .unwrap_err();
    assert_typed_not_enough_money(&err);

    let err = prevalidate_zarzakh_gate_toll(
        &ctx,
        missing_char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &confirm_kw,
    )
    .unwrap_err();
    assert_typed_custom_info(&err, "CharacterNotFound");
    assert!(err.message.contains(&missing_char_id.to_string()));

    // --- 2. process_zarzakh_gate_toll_with_amount ---
    let err = process_zarzakh_gate_toll_with_amount(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &decline_kw,
    )
    .unwrap_err();
    assert_typed_custom_info(&err, "ZarzakhGateTollDeclined");

    let err = process_zarzakh_gate_toll_with_amount(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &empty_kw,
    )
    .unwrap_err();
    assert_typed_custom_info(&err, "AskConfirmZarzakhGateToll");
    assert!(err.message.contains("10000 ISK"));

    let err = process_zarzakh_gate_toll_with_amount(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &empty_kw,
    )
    .unwrap_err();
    assert_typed_not_enough_money(&err);

    let err = process_zarzakh_gate_toll_with_amount(
        &ctx,
        char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &confirm_kw,
    )
    .unwrap_err();
    assert_typed_not_enough_money(&err);

    let err = process_zarzakh_gate_toll_with_amount(
        &ctx,
        missing_char_id,
        ship_id,
        from_gate_id,
        to_gate_id,
        &ball,
        &v24_args,
        &confirm_kw,
    )
    .unwrap_err();
    assert_typed_custom_info(&err, "CharacterNotFound");
    assert!(err.message.contains(&missing_char_id.to_string()));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].balance, underfunded);
        assert_eq!(s.wallet_journal.len(), journal_len_0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn taken_ball_guard_drop_on_cancellation_restores_ball_and_combat_state() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_701_i64;
    let sys_id = 30_000_142_u32;
    let orig_pos = DVec3::new(12_345.0, -6_789.0, 42_000.0);

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, orig_pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .modify_combat_state(ship_id, |cs| {
            cs.shield_hp = 123.5;
            cs.armor_hp = 234.5;
            cs.capacitor = 77.25;
        })
        .await
        .expect("modify_combat_state succeeds")
        .expect("combat state present");

    let taken = sys_handle
        .take_ball(ship_id)
        .await
        .expect("take_ball must remove live ship ball");
    assert!(
        sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "ball must be absent immediately after take_ball"
    );

    // Simulate async RPC cancellation mid-handoff via `tokio::select!`.
    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_tx = Arc::clone(&entered);
    let guard_handle = sys_handle.clone();
    let handoff_fut = async move {
        let _guard = TakenBallGuard::new(guard_handle, taken);
        entered_tx.notify_one();
        std::future::pending::<()>().await;
    };
    tokio::select! {
        _ = handoff_fut => unreachable!("pending future cannot complete"),
        _ = entered.notified() => {}
    }

    let (_, restored_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("dropping armed TakenBallGuard on cancellation must restore ball to source system");
    assert!(
        restored_ball.position.distance(orig_pos) < 1e-6,
        "restored ball must keep original position"
    );
    let restored_cs = sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("restored ball must preserve combat state");
    assert!((restored_cs.shield_hp - 123.5).abs() < 1e-6);
    assert!((restored_cs.armor_hp - 234.5).abs() < 1e-6);
    assert!((restored_cs.capacitor - 77.25).abs() < 1e-6);

    // Dropping an armed guard after a concurrent caller re-spawned the ball must NOT clobber the newer ball.
    let taken_again = sys_handle
        .take_ball(ship_id)
        .await
        .expect("second take_ball succeeds");
    let guard_stale = TakenBallGuard::new(sys_handle.clone(), taken_again);
    let newer_pos = DVec3::new(999_000.0, 111.0, -222.0);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: newer_pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("concurrent respawn succeeds");
    drop(guard_stale);

    let (_, live_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("concurrently respawned ball must remain present");
    assert!(
        live_ball.position.distance(newer_pos) < 1e-6,
        "stale TakenBallGuard::drop must not overwrite concurrently respawned ball"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn taken_ball_guard_disarm_on_commit_does_not_reinsert_ball() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 895_702_i64;
    let sys_id = 30_000_142_u32;
    let pos = DVec3::new(5_000.0, 1_000.0, -2_000.0);

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let taken = sys_handle
        .take_ball(ship_id)
        .await
        .expect("take_ball removes ship");
    let guard = TakenBallGuard::new(sys_handle.clone(), taken);
    let committed = guard.disarm();
    assert_eq!(committed.ball.id, ship_id);
    drop(committed);

    assert!(
        sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "dropping a disarmed TakenBallGuard must NOT re-insert the ball into the source system"
    );

    // Explicit `restore().await` restores synchronously and disarms cleanly.
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("respawn ship for restore test");
    let taken_for_restore = sys_handle
        .take_ball(ship_id)
        .await
        .expect("take_ball for explicit restore");
    let guard_restore = TakenBallGuard::new(sys_handle.clone(), taken_for_restore);
    guard_restore.restore().await;
    let (_, restored) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("explicit TakenBallGuard::restore must re-insert the ball");
    assert!(restored.position.distance(pos) < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stargate_jump_cancelled_mid_handoff_restores_ball_via_taken_ball_guard() {
    let ctx = Arc::new(test_ctx());
    let char_id = 90_000_001_i64;
    let ship_id = 895_703_i64;
    let old_sys = 30_000_142_u32;
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == old_sys && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH)
        .cloned()
        .expect("Jita must have a non-Zarzakh stargate");
    let ship_pos = gate.position + DVec3::new(500.0, 0.0, 0.0);

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, old_sys, ship_pos);
    let old_handle = ctx.systems.get_or_spawn(old_sys);
    old_handle
        .modify_combat_state(ship_id, |cs| {
            cs.shield_hp = 210.0;
            cs.capacitor = 140.0;
        })
        .await
        .unwrap()
        .expect("ship combat state present");

    let mut sess = SessionState {
        session_id: 70_301,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(old_sys),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    mark_char_online(&ctx, &mut sess, char_id);

    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_tx = Arc::clone(&entered);
    let old_handle_check = old_handle.clone();
    set_post_take_ball_hook_for_test(
        &ctx,
        char_id,
        Some(Box::new(move || {
            Box::pin(async move {
                assert!(
                    old_handle_check
                        .get_ball_snapshot(ship_id)
                        .await
                        .unwrap()
                        .is_none(),
                    "ball must already be taken when post-take-ball hook fires"
                );
                entered_tx.notify_one();
                std::future::pending::<()>().await;
            })
        })),
    );

    let kw = HashMap::new();
    let jump_args = [PyRep::Integer(gate.stargate_id)];
    tokio::select! {
        _ = handle(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &jump_args, &kw) => {
            unreachable!("cancelled stargate jump future must not finish")
        }
        _ = entered.notified() => {}
    }

    let (_, restored) = old_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("cancelling CmdStargateJump mid-handoff must restore ship ball via TakenBallGuard");
    assert!(restored.position.distance(ship_pos) < 1e-6);
    let restored_cs = old_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("restored combat state present");
    assert!((restored_cs.shield_hp - 210.0).abs() < 1e-6);
    assert!((restored_cs.capacitor - 140.0).abs() < 1e-6);
    assert_eq!(sess.solar_system_id, Some(old_sys));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].solar_system_id, old_sys);
        assert_eq!(snap.items[&ship_id].location_id, i64::from(old_sys));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dock_cancelled_mid_handoff_restores_ball_via_taken_ball_guard() {
    let ctx = Arc::new(test_ctx());
    let char_id = 90_000_001_i64;
    let ship_id = 895_704_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let st_pos = ctx.sde.stations[&st_id].position;
    let ship_pos = st_pos + DVec3::new(500.0, 0.0, 0.0);

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, ship_pos);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .modify_combat_state(ship_id, |cs| {
            cs.shield_hp = 315.0;
            cs.capacitor = 188.0;
        })
        .await
        .unwrap()
        .expect("ship combat state present");

    let mut sess = SessionState {
        session_id: 70_401,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };

    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_tx = Arc::clone(&entered);
    let sys_handle_check = sys_handle.clone();
    set_post_take_ball_hook_for_test(
        &ctx,
        char_id,
        Some(Box::new(move || {
            Box::pin(async move {
                assert!(
                    sys_handle_check
                        .get_ball_snapshot(ship_id)
                        .await
                        .unwrap()
                        .is_none(),
                    "ball must already be taken for docking when hook fires"
                );
                entered_tx.notify_one();
                std::future::pending::<()>().await;
            })
        })),
    );

    let kw = HashMap::new();
    let dock_args = [PyRep::Integer(st_id)];
    tokio::select! {
        _ = handle(&ctx, &mut sess, None, "beyonce", "CmdDock", &dock_args, &kw) => {
            unreachable!("cancelled CmdDock future must not finish")
        }
        _ = entered.notified() => {}
    }

    let (_, restored) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("cancelling CmdDock mid-handoff must restore ship ball via TakenBallGuard");
    assert!(restored.position.distance(ship_pos) < 1e-6);
    let restored_cs = sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("restored combat state present");
    assert!((restored_cs.shield_hp - 315.0).abs() < 1e-6);
    assert!((restored_cs.capacitor - 188.0).abs() < 1e-6);
    assert_eq!(sess.station_id, None);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].station_id, None);
        assert_eq!(snap.characters[&char_id].solar_system_id, sys_id);
        assert_eq!(snap.items[&ship_id].location_id, i64::from(sys_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stargate_jump_spawn_ball_with_meta_failure_rolls_back_db_toll_and_restores_ball() {
    // --- Case 1: Standard stargate jump when destination spawn_ball_with_meta fails ---
    {
        let ctx = test_ctx();
        let char_id = 90_000_001_i64;
        let ship_id = 895_705_i64;
        let old_sys = 30_000_142_u32;
        let gate = ctx
            .sde
            .jumps
            .values()
            .find(|j| {
                j.from_solar_system_id == old_sys && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
            })
            .cloned()
            .expect("Jita must have a non-Zarzakh stargate");
        let dest_sys = gate.to_solar_system_id;
        let ship_pos = gate.position + DVec3::new(500.0, 0.0, 0.0);

        spawn_ship_in_space(&ctx, char_id, ship_id, 606, old_sys, ship_pos);
        let (orig_constellation, orig_region) = ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            (ch.constellation_id, ch.region_id)
        });
        let old_handle = ctx.systems.get_or_spawn(old_sys);
        old_handle
            .modify_combat_state(ship_id, |cs| {
                cs.shield_hp = 175.0;
                cs.capacitor = 99.5;
            })
            .await
            .unwrap()
            .expect("combat state present");

        // Pre-spawn destination actor and inject failure on its next command (`SpawnBallWithMeta`).
        let dest_handle = ctx.systems.get_or_spawn(dest_sys);
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let mut sess = SessionState {
            session_id: 70_501,
            user_id: Some(1),
            character_id: Some(char_id),
            solar_system_id: Some(old_sys),
            station_id: None,
            ship_id: Some(ship_id),
            ..Default::default()
        };
        mark_char_online(&ctx, &mut sess, char_id);

        let kw = HashMap::new();
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
        .expect("CmdStargateJump handled")
        .expect_err("spawn_ball_with_meta failure must return Err instead of silently succeeding");
        assert_eq!(err.code, "SystemTransitionFailed");
        assert!(
            matches!(
                err.client_error.as_deref(),
                Some(eve_proto::ClientError::CustomInfo(_))
            ),
            "expected typed ClientError::CustomInfo, got {:?}",
            err.client_error
        );
        assert!(err.message.contains(&dest_sys.to_string()));

        // Session, DB, and ballparks must all be rolled back cleanly to `old_sys`.
        assert_eq!(sess.solar_system_id, Some(old_sys));
        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.solar_system_id, old_sys);
            assert_eq!(ch.constellation_id, orig_constellation);
            assert_eq!(ch.region_id, orig_region);
            assert_eq!(ch.station_id, None);
            assert_eq!(ch.structure_id, None);
            assert_eq!(snap.items[&ship_id].location_id, i64::from(old_sys));
        });
        let (_, restored) = old_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("ship ball must be restored into source system on destination spawn error");
        assert!(restored.position.distance(ship_pos) < 1e-6);
        let restored_cs = old_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .expect("restored combat state present");
        assert!((restored_cs.shield_hp - 175.0).abs() < 1e-6);
        assert!((restored_cs.capacitor - 99.5).abs() < 1e-6);
        assert!(
            dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
            "ship ball must NOT exist in destination system after failed spawn"
        );
    }

    // --- Case 2: Zarzakh stargate jump with toll when destination spawn_ball_with_meta fails ---
    {
        let ctx = test_ctx();
        let char_id = 90_000_001_i64;
        let ship_id = 895_706_i64;
        let zarzakh_gate = ctx
            .sde
            .jumps
            .values()
            .find(|j| j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH)
            .cloned()
            .expect("SDE must include a stargate leading to Zarzakh");
        let old_sys = zarzakh_gate.from_solar_system_id;
        let ship_pos = zarzakh_gate.position + DVec3::new(500.0, 0.0, 0.0);
        let start_isk = eve_db::IskCents::from_isk(500_000_000);

        spawn_ship_in_space(&ctx, char_id, ship_id, 587, old_sys, ship_pos);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.balance = start_isk;
            snap.zarzakh_gate_locks.remove(&char_id);
        });
        let old_handle = ctx.systems.get_or_spawn(old_sys);
        let dest_handle = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let mut sess = SessionState {
            session_id: 70_601,
            user_id: Some(1),
            character_id: Some(char_id),
            solar_system_id: Some(old_sys),
            station_id: None,
            ship_id: Some(ship_id),
            ..Default::default()
        };
        mark_char_online(&ctx, &mut sess, char_id);

        let mut kw = HashMap::new();
        kw.insert("confirmToll".to_string(), PyRep::Bool(true));
        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(zarzakh_gate.stargate_id)],
            &kw,
        )
        .await
        .expect("CmdStargateJump handled")
        .expect_err("Zarzakh spawn_ball_with_meta failure must return Err");
        assert_eq!(err.code, "SystemTransitionFailed");
        assert!(matches!(
            err.client_error.as_deref(),
            Some(eve_proto::ClientError::CustomInfo(_))
        ));

        assert_eq!(sess.solar_system_id, Some(old_sys));
        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.solar_system_id, old_sys);
            assert_eq!(
                ch.balance, start_isk,
                "Zarzakh toll must be refunded in full when destination spawn fails"
            );
            assert!(
                !snap.zarzakh_gate_locks.contains_key(&char_id),
                "Zarzakh 6-hour gate lock must NOT be recorded when destination spawn fails"
            );
            assert_eq!(snap.items[&ship_id].location_id, i64::from(old_sys));
            assert!(
                snap.wallet_journal.iter().any(|j| {
                    j.character_id == char_id
                        && j.entry_type == "zarzakh_gate_toll_refund"
                        && j.amount == eve_db::IskCents::from_isk(10_000)
                }),
                "Zarzakh toll refund journal entry must be recorded"
            );
        });
        assert!(
            old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
            "ship ball must be restored into origin system after failed Zarzakh spawn"
        );
        assert!(
            dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
            "ship ball must NOT exist in Zarzakh after failed spawn"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_cancel_queued_spawn_discards_closed_reply_and_rolls_back_1237() {
    // Subcheck A: direct SpawnBallWithMeta with dropped reply receiver must not spawn a ball.
    {
        let ctx = test_ctx();
        let sys_h = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
        let direct_ship_id = 895_707_i64;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        drop(reply_rx);
        sys_h
            .cmd_tx
            .send(crate::system_actor::SystemCommand::SpawnBallWithMeta {
                ball: Ball {
                    id: direct_ship_id,
                    type_id: 587,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_100_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                combat_state: CombatState::default(),
                slim_meta: eve_destiny::SlimItemMeta::default(),
                reply: reply_tx,
            })
            .await
            .expect("enqueue closed-reply SpawnBallWithMeta");
        assert!(
            sys_h.get_ball_snapshot(direct_ship_id).await.unwrap().is_none(),
            "closed-reply SpawnBallWithMeta must discard spawning without mutating ballpark"
        );
    }

    // Subcheck B: cancel CmdStargateJump while SpawnBallWithMeta sits queued behind paused actor.
    let ctx = test_ctx();
    let gate = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH
                && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
        })
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must include a Zarzakh-bound gate");

    let char_id = 90_000_001_i64;
    let ship_id = 895_708_i64;
    let start_isk = eve_db::IskCents::from_isk(100_000);
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.remove(&char_id);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = None;
        ch.balance = start_isk;
    });
    let old_handle = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("origin system loaded");
    assert!(old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

    let dest_handle = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
    let (paused_tx, paused_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    *dest_handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = paused_tx.send(());
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
    }));
    let step_task = tokio::spawn({
        let h = dest_handle.clone();
        async move { h.step_manual(0.0).await }
    });
    paused_rx
        .await
        .expect("destination actor must pause inside step_manual hook");

    let (enqueued_tx, mut enqueued_rx) = tokio::sync::oneshot::channel::<()>();
    crate::system_actor::set_spawn_ball_with_meta_after_send_hook_for_test(
        SOLAR_SYSTEM_ZARZAKH,
        ship_id,
        Some(Box::new(move || {
            Box::pin(async move {
                let _ = enqueued_tx.send(());
                std::future::pending::<()>().await;
            })
        })),
    );

    let mut sess = SessionState::new(70_701);
    mark_char_online(&ctx, &mut sess, char_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(gate.from_solar_system_id);

    let args = [
        PyRep::Integer(gate.stargate_id),
        PyRep::Integer(gate.destination_stargate_id),
        PyRep::Integer(ship_id),
    ];
    let mut confirm_kw = HashMap::new();
    confirm_kw.insert("confirmToll".to_string(), PyRep::Bool(true));

    {
        let mut jump_fut = Box::pin(handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &confirm_kw,
        ));
        tokio::select! {
            biased;
            res = &mut enqueued_rx => {
                res.expect("after_send_hook must signal queued SpawnBallWithMeta");
            }
            res = &mut jump_fut => {
                panic!("expected cancellation while awaiting queued SpawnBallWithMeta, got {res:?}");
            }
        }
        drop(jump_fut);
    }

    release_tx
        .send(())
        .expect("unblock paused destination actor");
    step_task
        .await
        .expect("step_manual join")
        .expect("step_manual result");

    assert!(
        dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "cancelled queued SpawnBallWithMeta must discard spawn and leave zero ball in Zarzakh"
    );
    assert!(
        old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
        "origin ship ball must be restored on queued spawn cancellation"
    );
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.solar_system_id, gate.from_solar_system_id);
        assert_eq!(ch.balance, start_isk, "Zarzakh toll must be refunded cleanly");
        assert!(
            !snap.zarzakh_gate_locks.contains_key(&char_id),
            "Zarzakh gate lock must NOT be recorded on cancelled jump"
        );
        assert!(
            snap.wallet_journal.iter().any(|j| {
                j.character_id == char_id
                    && j.entry_type == "zarzakh_gate_toll_refund"
                    && j.amount == eve_db::IskCents::from_isk(10_000)
            }),
            "cancelled Zarzakh jump must record toll refund journal entry"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_cancel_after_dest_spawn_removes_dest_ball_and_restores_origin_1237() {
    let ctx = test_ctx();
    let gate = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH
                && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
        })
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must include a Zarzakh-bound gate");

    let char_id = 90_000_001_i64;
    let ship_id = 895_709_i64;
    let start_isk = eve_db::IskCents::from_isk(100_000);
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.remove(&char_id);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = None;
        ch.balance = start_isk;
    });
    let old_handle = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("origin system loaded");
    assert!(old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

    let dest_handle = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
    let dest_handle_hook = dest_handle.clone();
    let (materialized_tx, mut materialized_rx) = tokio::sync::oneshot::channel::<bool>();
    crate::system_actor::set_spawn_ball_with_meta_pre_reply_hook_for_test(
        SOLAR_SYSTEM_ZARZAKH,
        ship_id,
        Some(Box::new(move || {
            Box::pin(async move {
                let present = dest_handle_hook
                    .get_ball_snapshot(ship_id)
                    .await
                    .unwrap()
                    .is_some();
                let _ = materialized_tx.send(present);
                std::future::pending::<()>().await;
            })
        })),
    );

    let mut sess = SessionState::new(70_702);
    mark_char_online(&ctx, &mut sess, char_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(gate.from_solar_system_id);

    let args = [
        PyRep::Integer(gate.stargate_id),
        PyRep::Integer(gate.destination_stargate_id),
        PyRep::Integer(ship_id),
    ];
    let mut confirm_kw = HashMap::new();
    confirm_kw.insert("confirmToll".to_string(), PyRep::Bool(true));

    {
        let mut jump_fut = Box::pin(handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &confirm_kw,
        ));
        tokio::select! {
            biased;
            res = &mut materialized_rx => {
                assert!(
                    res.expect("pre_reply_hook signal"),
                    "SpawnBallWithMeta must have already materialized destination ball before cancel"
                );
            }
            res = &mut jump_fut => {
                panic!("expected cancellation during pre_reply_hook suspension, got {res:?}");
            }
        }
        drop(jump_fut);
    }

    assert!(
        dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "StargateRollbackGuard::drop must remove already-spawned destination ball after post-spawn cancellation"
    );
    assert!(
        old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
        "origin ship ball must be restored after post-spawn cancellation"
    );
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.solar_system_id, gate.from_solar_system_id);
        assert_eq!(ch.balance, start_isk, "Zarzakh toll must be refunded cleanly");
        assert!(
            !snap.zarzakh_gate_locks.contains_key(&char_id),
            "Zarzakh gate lock must NOT exist after post-spawn cancellation"
        );
        assert!(
            snap.wallet_journal.iter().any(|j| {
                j.character_id == char_id
                    && j.entry_type == "zarzakh_gate_toll_refund"
                    && j.amount == eve_db::IskCents::from_isk(10_000)
            }),
            "post-spawn cancellation must record Zarzakh toll refund journal entry"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_concurrent_transition_suppresses_toll_refund_while_unrelated_version_bump_rolls_back_1237() {
    // Subtest A: superseding DB transition causes DB rollback to return false -> toll refund MUST NOT fire.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH
                    && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a Zarzakh-bound gate");
        let other_sys = ctx
            .sde
            .solar_systems
            .keys()
            .copied()
            .find(|&sid| sid != gate.from_solar_system_id && sid != SOLAR_SYSTEM_ZARZAKH)
            .expect("third solar system");

        let char_id = 90_000_001_i64;
        let ship_id = 895_710_i64;
        let start_isk = eve_db::IskCents::from_isk(100_000);
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks.remove(&char_id);
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.war_faction_id = None;
            ch.balance = start_isk;
        });
        let old_handle = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("origin system loaded");
        assert!(old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

        let dest_handle = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
        let db_hook = Arc::clone(&ctx.db);
        let sde_hook = Arc::clone(&ctx.sde);
        let (raced_tx, mut raced_rx) = tokio::sync::oneshot::channel::<()>();
        crate::system_actor::set_spawn_ball_with_meta_pre_reply_hook_for_test(
            SOLAR_SYSTEM_ZARZAKH,
            ship_id,
            Some(Box::new(move || {
                Box::pin(async move {
                    db_hook
                        .transition_character_solar_system_if_at(
                            char_id,
                            SOLAR_SYSTEM_ZARZAKH,
                            other_sys,
                            &sde_hook,
                        )
                        .expect("superseding DB transition during pre_reply_hook must commit");
                    let _ = raced_tx.send(());
                    std::future::pending::<()>().await;
                })
            })),
        );

        let mut sess = SessionState::new(70_703);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let mut confirm_kw = HashMap::new();
        confirm_kw.insert("confirmToll".to_string(), PyRep::Bool(true));

        {
            let mut jump_fut = Box::pin(handle(
                &ctx,
                &mut sess,
                None,
                "beyonce",
                "CmdStargateJump",
                &args,
                &confirm_kw,
            ));
            tokio::select! {
                biased;
                res = &mut raced_rx => res.expect("superseding transition signal"),
                res = &mut jump_fut => panic!("expected cancellation after superseding transition, got {res:?}"),
            }
            drop(jump_fut);
        }

        assert!(
            dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
            "refused DB rollback must NOT remove the live destination ball"
        );
        assert!(
            old_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
            "refused DB rollback must NOT restore a duplicate ghost ball in origin system"
        );
        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.solar_system_id, other_sys, "superseding system must remain active");
            assert_eq!(
                ch.balance,
                eve_db::IskCents::from_isk(90_000),
                "Zarzakh toll MUST NOT be refunded when DB rollback returns false"
            );
            assert!(
                !snap
                    .wallet_journal
                    .iter()
                    .any(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll_refund"),
                "wallet journal must NOT contain a toll refund when DB rollback refused"
            );
        });
    }

    // Subtest B: unrelated `ch.version` increment before actor failure MUST still allow `rollback_now`
    // to revert the DB transition and refund the Zarzakh toll.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH
                    && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a Zarzakh-bound gate");

        let char_id = 90_000_001_i64;
        let ship_id = 895_711_i64;
        let start_isk = eve_db::IskCents::from_isk(100_000);
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks.remove(&char_id);
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.war_faction_id = None;
            ch.balance = start_isk;
        });
        let old_handle = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("origin system loaded");
        assert!(old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

        let dest_handle = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
        let (paused_tx, paused_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        *dest_handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
            let _ = paused_tx.send(());
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
        }));
        let step_task = tokio::spawn({
            let h = dest_handle.clone();
            async move { h.step_manual(0.0).await }
        });
        paused_rx
            .await
            .expect("destination actor must pause inside step_manual hook");

        let db_hook = Arc::clone(&ctx.db);
        let dest_hook = dest_handle.clone();
        crate::system_actor::set_spawn_ball_with_meta_after_send_hook_for_test(
            SOLAR_SYSTEM_ZARZAKH,
            ship_id,
            Some(Box::new(move || {
                Box::pin(async move {
                    db_hook.with_mut_snapshot(|snap| {
                        snap.characters.get_mut(&char_id).unwrap().version += 1;
                    });
                    dest_hook
                        .panic_next_command_for_test
                        .store(true, std::sync::atomic::Ordering::Release);
                    let _ = release_tx.send(());
                })
            })),
        );

        let mut sess = SessionState::new(70_704);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let mut confirm_kw = HashMap::new();
        confirm_kw.insert("confirmToll".to_string(), PyRep::Bool(true));

        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &confirm_kw,
        )
        .await
        .expect("CmdStargateJump must be handled")
        .expect_err("simulated actor panic must fail transition and invoke rollback_now");
        assert_eq!(err.code, "SystemTransitionFailed");
        step_task
            .await
            .expect("step_manual join")
            .expect("step_manual result");

        assert!(
            old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some(),
            "origin ball must be restored even after unrelated ch.version bump"
        );
        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.solar_system_id, gate.from_solar_system_id);
            assert_eq!(
                ch.balance, start_isk,
                "Zarzakh toll MUST still be refunded when only ch.version bumped"
            );
            assert!(
                snap.wallet_journal.iter().any(|j| {
                    j.character_id == char_id
                        && j.entry_type == "zarzakh_gate_toll_refund"
                        && j.amount == eve_db::IskCents::from_isk(10_000)
                }),
                "toll refund journal must be recorded when rollback succeeds after ch.version bump"
            );
        });
    }
}

#[tokio::test]
async fn cmd_stargate_jump_rollback_restores_extra_ship_and_preserves_nested_corp_ownership_1237() {
    let ctx = test_ctx();
    let gate = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
        })
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must include a non-Zarzakh gate");

    let char_id = 90_000_001_i64;
    let active_ship_id = 895_712_i64;
    let extra_ship_id = 895_713_i64;
    let nested_item_id = 895_713_999_i64;
    let corp_owner_id = 98_000_001_i64;

    spawn_ship_in_space(
        &ctx,
        char_id,
        extra_ship_id,
        587,
        gate.from_solar_system_id,
        gate.position,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            active_ship_id,
            InventoryItemRecord {
                item_id: active_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: gate.from_solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = active_ship_id;
        snap.items.insert(
            nested_item_id,
            InventoryItemRecord {
                item_id: nested_item_id,
                type_id: 34,
                owner_id: corp_owner_id,
                location_id: extra_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 42,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let old_handle = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("origin system loaded");
    assert!(old_handle.get_ball_snapshot(extra_ship_id).await.unwrap().is_some());

    let dest_handle = ctx.systems.get_or_spawn(gate.to_solar_system_id);
    dest_handle
        .panic_next_command_for_test
        .store(true, std::sync::atomic::Ordering::Release);

    let mut sess = SessionState::new(70_705);
    mark_char_online(&ctx, &mut sess, char_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(extra_ship_id);
    sess.solar_system_id = Some(gate.from_solar_system_id);

    let args = [
        PyRep::Integer(gate.stargate_id),
        PyRep::Integer(gate.destination_stargate_id),
        PyRep::Integer(extra_ship_id),
    ];
    let empty_kw = HashMap::new();
    let err = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdStargateJump",
        &args,
        &empty_kw,
    )
    .await
    .expect("CmdStargateJump must be handled")
    .expect_err("simulated actor panic must fail transition and trigger atomic rollback");
    assert_eq!(err.code, "SystemTransitionFailed");

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.solar_system_id, gate.from_solar_system_id);
        assert_eq!(ch.active_ship_id, active_ship_id);
        let active_rec = &snap.items[&active_ship_id];
        assert_eq!(active_rec.location_id, gate.from_solar_system_id as i64);
        assert_eq!(active_rec.flag, InventoryFlag::None);
        let extra_rec = &snap.items[&extra_ship_id];
        assert_eq!(extra_rec.location_id, gate.from_solar_system_id as i64);
        assert_eq!(extra_rec.flag, InventoryFlag::None);
        let nested_rec = &snap.items[&nested_item_id];
        assert_eq!(nested_rec.location_id, extra_ship_id);
        assert_eq!(nested_rec.flag, InventoryFlag::Cargo);
        assert_eq!(
            nested_rec.owner_id, corp_owner_id,
            "atomic rollback MUST preserve nested corp-owned item owner_id without recursive re-ownership"
        );
    });
    assert!(
        old_handle.get_ball_snapshot(extra_ship_id).await.unwrap().is_some(),
        "extra_ship_id ball must be restored to origin system on failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_ball_if_absent_and_taken_guard_drop_do_not_overwrite_queued_spawn_1237() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Part A: explicit restore_ball_if_absent must not clobber a concurrently spawned live ball.
    let direct_ship_id = 895_714_i64;
    spawn_ship_in_space(
        &ctx,
        90_000_001,
        direct_ship_id,
        587,
        sys_id,
        DVec3::new(100.0, 0.0, 0.0),
    );
    assert!(sys_handle.get_ball_snapshot(direct_ship_id).await.unwrap().is_some());
    let taken_a = sys_handle
        .take_ball(direct_ship_id)
        .await
        .expect("ball present to take");
    let replacement_pos = DVec3::new(9_000.0, 0.0, 0.0);
    sys_handle
        .spawn_ball(Ball {
            id: direct_ship_id,
            type_id: 587,
            position: replacement_pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("respawn replacement ball");
    sys_handle
        .restore_ball_if_absent(taken_a)
        .await
        .expect("restore_ball_if_absent must succeed cleanly as a no-op when ball exists");
    let (_, after_a) = sys_handle
        .get_ball_snapshot(direct_ship_id)
        .await
        .unwrap()
        .expect("replacement ball remains live");
    assert_eq!(after_a.position, replacement_pos);

    // Part B: queued SpawnBall ahead of TakenBallGuard::drop while actor is paused must win atomically.
    let queued_ship_id = 895_715_i64;
    spawn_ship_in_space(
        &ctx,
        90_000_001,
        queued_ship_id,
        587,
        sys_id,
        DVec3::new(200.0, 0.0, 0.0),
    );
    assert!(sys_handle.get_ball_snapshot(queued_ship_id).await.unwrap().is_some());
    let taken_b = sys_handle
        .take_ball(queued_ship_id)
        .await
        .expect("ball present to take");
    assert!(sys_handle.get_ball_snapshot(queued_ship_id).await.unwrap().is_none());

    let guard = TakenBallGuard::new(sys_handle.clone(), taken_b);
    let (paused_tx, paused_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    *sys_handle.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = paused_tx.send(());
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
    }));
    let step_task = tokio::spawn({
        let h = sys_handle.clone();
        async move { h.step_manual(0.0).await }
    });
    paused_rx
        .await
        .expect("system actor must pause inside step_manual hook");

    let newer_pos = DVec3::new(42_000.0, 1.0, -2.0);
    let (spawn_tx, _spawn_rx) = tokio::sync::oneshot::channel();
    sys_handle
        .cmd_tx
        .send(crate::system_actor::SystemCommand::SpawnBall {
            ball: Ball {
                id: queued_ship_id,
                type_id: 587,
                position: newer_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            reply: spawn_tx,
        })
        .await
        .expect("queue SpawnBall behind paused actor");
    drop(guard);

    release_tx.send(()).expect("unblock paused actor");
    step_task
        .await
        .expect("step_manual join")
        .expect("step_manual result");

    let (_, final_ball) = sys_handle
        .get_ball_snapshot(queued_ship_id)
        .await
        .unwrap()
        .expect("queued SpawnBall must remain live");
    assert_eq!(
        final_ball.position, newer_pos,
        "TakenBallGuard::drop RestoreBallIfAbsent queued behind SpawnBall must NOT clobber newer live ball"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_preserves_matching_space_entry_epoch_for_post_jump_persistence_and_rollback_1237() {
    // Part A (BF-1237C-01 forward): `dest_handle` caches the post-increment `space_entry_epoch`
    // so post-jump `flush_condition_checkpoint` / `apply_condition_checkpoint_for_system_in_db`,
    // `item_positions`, and warp-landing persistence all succeed in `dest_sys_id`.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a non-Zarzakh stargate pair");

        let char_id = 90_000_001_i64;
        let ship_id = 895_720_i64;
        let pre_epoch = 4_u64;
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.ship_combat_hp.insert(
                -ship_id,
                eve_db::PersistedCombatHp {
                    shield_hp: 0.0,
                    armor_hp: gate.from_solar_system_id as f64,
                    hull_hp: pre_epoch as f64,
                },
            );
            snap.item_positions
                .insert(ship_id, gate.position.to_array());
        });

        let mut sess = SessionState::new(70_720);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("CmdStargateJump succeeds");

        let dest_sys_id = gate.to_solar_system_id;
        let dest_handle = ctx
            .systems
            .get_handle(dest_sys_id)
            .expect("destination system loaded");
        let expected_epoch = pre_epoch + 1;
        assert_eq!(
            crate::sp6_lifecycle::current_ship_space_entry_epoch(&ctx.db, ship_id),
            expected_epoch,
            "CmdStargateJump must advance DB space_entry_epoch by 1"
        );

        let (_, dest_ball) = dest_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("ship spawned in destination system");
        let moved_pos = dest_ball.position + DVec3::new(12_345.0, -500.0, 250.0);
        assert!(dest_handle.set_ball_position(ship_id, moved_pos).await);
        dest_handle
            .apply_true_damage(ship_id, 15.0)
            .await
            .expect("apply_true_damage");

        let written = dest_handle
            .flush_condition_checkpoint()
            .await
            .expect("flush_condition_checkpoint");
        assert_eq!(
            written, 1,
            "destination condition checkpoint MUST commit with matching space_entry_epoch == E + 1"
        );
        ctx.db.with_snapshot(|snap| {
            let persisted_pos = snap
                .item_positions
                .get(&ship_id)
                .copied()
                .expect("item_positions persisted by condition checkpoint");
            assert!(
                (DVec3::from_array(persisted_pos) - moved_pos).length() < 1e-3,
                "post-jump item_positions must persist via condition checkpoint"
            );
        });

        let warp_dest = moved_pos + DVec3::new(300_000.0, 0.0, 0.0);
        dest_handle
            .set_ball_position(ship_id, warp_dest - DVec3::new(100.0, 0.0, 0.0))
            .await;
        dest_handle
            .set_warp_movement_mode(
                ship_id,
                MovementMode::Warp {
                    origin: moved_pos,
                    destination: warp_dest,
                    warp_speed_au_s: 15.0,
                    total_distance: moved_pos.distance(warp_dest),
                    phase: eve_destiny::WarpPhase::Decelerating,
                    elapsed_warp_time: 100.0,
                },
                crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
            )
            .await
            .expect("enter warp in destination system");
        for _ in 0..40 {
            dest_handle.step_manual(1.0).await.expect("step_manual");
            let (_, cur) = dest_handle
                .get_ball_snapshot(ship_id)
                .await
                .unwrap()
                .expect("ship still in space");
            if !matches!(cur.mode, MovementMode::Warp { .. }) {
                break;
            }
        }
        let (_, landed_ball) = dest_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("ship present after warp");
        assert!(
            !matches!(landed_ball.mode, MovementMode::Warp { .. }),
            "ship must land from warp"
        );
        ctx.db.with_snapshot(|snap| {
            let persisted_pos = snap
                .item_positions
                .get(&ship_id)
                .copied()
                .expect("warp landing must persist item_positions");
            assert!(
                (DVec3::from_array(persisted_pos) - landed_ball.position).length() < 1.0,
                "post-jump warp landing must persist landed position cleanly: db={persisted_pos:?} ball={:?}",
                landed_ball.position
            );
        });
    }

    // Part B (BF-1237C-01 rollback): spawn failure reverts `ship_combat_hp[ship_id]`,
    // `ship_combat_hp[-ship_id]`, `item_positions[ship_id]`, and `DAMAGE`/`ARMOR_DAMAGE`.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a non-Zarzakh stargate pair");

        let char_id = 90_000_001_i64;
        let ship_id = 895_721_i64;
        let pre_epoch = 7_u64;
        let pre_pos = [1234.5, -678.0, 910.25];
        let pre_hp = eve_db::PersistedCombatHp {
            shield_hp: 111.0,
            armor_hp: 222.0,
            hull_hp: 333.0,
        };
        let pre_meta = eve_db::PersistedCombatHp {
            shield_hp: 42.0,
            armor_hp: gate.from_solar_system_id as f64,
            hull_hp: pre_epoch as f64,
        };
        let pre_armor_dmg = 17.5_f64;
        spawn_ship_in_space(
            &ctx,
            char_id,
            ship_id,
            587,
            gate.from_solar_system_id,
            gate.position,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.ship_combat_hp.insert(ship_id, pre_hp);
            snap.sp6_lifecycle
                .ship_combat_hp
                .insert(-ship_id, pre_meta);
            snap.item_positions.insert(ship_id, pre_pos);
            let sh = snap.items.get_mut(&ship_id).expect("ship item exists");
            sh.dynamic_attributes
                .insert(attr_ids::ARMOR_DAMAGE, pre_armor_dmg);
            sh.dynamic_attributes.remove(&attr_ids::DAMAGE);
        });
        let old_handle = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("origin handle");
        let dest_handle = ctx.systems.get_or_spawn(gate.to_solar_system_id);
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::Release);

        let mut sess = SessionState::new(70_721);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect_err("spawn panic must unwind transition");
        assert_eq!(err.code, "SystemTransitionFailed");

        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied(),
                Some(pre_hp),
                "ship_combat_hp[ship_id] must be rolled back atomically"
            );
            assert_eq!(
                snap.sp6_lifecycle.ship_combat_hp.get(&(-ship_id)).copied(),
                Some(pre_meta),
                "ship_combat_hp[-ship_id] epoch must be rolled back atomically"
            );
            assert_eq!(
                snap.item_positions.get(&ship_id).copied(),
                Some(pre_pos),
                "item_positions[ship_id] must be rolled back atomically"
            );
            let sh = &snap.items[&ship_id];
            assert_eq!(
                sh.dynamic_attributes.get(&attr_ids::ARMOR_DAMAGE).copied(),
                Some(pre_armor_dmg),
                "ARMOR_DAMAGE dynamic attribute must be rolled back atomically"
            );
            assert_eq!(
                sh.dynamic_attributes.get(&attr_ids::DAMAGE).copied(),
                None,
                "absent DAMAGE dynamic attribute must remain absent on rollback"
            );
        });
        assert!(old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_deactivates_hardeners_and_phenomena_before_dest_combat_state_and_rolls_back_1237() {
    let make_jumpable_phen_ctx = || {
        let mut ctx = test_ctx();
        let base_phen = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 1815 && t.published)
            .min_by_key(|t| t.type_id)
            .cloned()
            .expect("SDE must include a Phenomena Generator (group 1815)");
        let jumpable_phen_tid = 999_815_u32;
        let mut jumpable_phen = base_phen;
        jumpable_phen.type_id = jumpable_phen_tid;
        for aid in [852_u16, 2354_u16, 2453_u16, 1302_u16, 1303_u16] {
            jumpable_phen.default_attributes.remove(&aid);
        }
        jumpable_phen.default_attributes.insert(30_u16, 1.0);
        jumpable_phen.default_attributes.insert(50_u16, 1.0);
        Arc::make_mut(&mut ctx.sde)
            .types
            .insert(jumpable_phen_tid, jumpable_phen);
        (ctx, jumpable_phen_tid)
    };
    let setup_fit = |ctx: &ServerContext,
                     phen_type_id: u32,
                     char_id: i64,
                     ship_id: i64,
                     fleetmate_id: i64,
                     from_sys: u32,
                     gate_pos: DVec3| {
        let hull_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 27 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("SDE must include a Battleship hull (group 27)");
        let (hp_dbuff_id, buffs_shield) = ctx
            .sde
            .dbuff_collections
            .iter()
            .filter(|(_, c)| {
                c.item_modifiers.contains(&attr_ids::SHIELD_CAPACITY)
                    || c.location_modifiers.contains(&attr_ids::SHIELD_CAPACITY)
                    || c.item_modifiers.contains(&attr_ids::ARMOR_HP)
                    || c.location_modifiers.contains(&attr_ids::ARMOR_HP)
            })
            .min_by_key(|(&id, _)| id)
            .map(|(&id, c)| {
                let s = c.item_modifiers.contains(&attr_ids::SHIELD_CAPACITY)
                    || c.location_modifiers.contains(&attr_ids::SHIELD_CAPACITY);
                (id, s)
            })
            .expect("SDE must include a Shield Capacity or Armor HP dbuff collection");
        let hardener_mod_id = ship_id + 101;
        let phen_mod_id = ship_id + 102;

        spawn_ship_in_space(ctx, char_id, ship_id, hull_type_id, from_sys, gate_pos);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                hardener_mod_id,
                InventoryItemRecord {
                    item_id: hardener_mod_id,
                    type_id: 2281, // Multispectrum Shield Hardener II
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                phen_mod_id,
                InventoryItemRecord {
                    item_id: phen_mod_id,
                    type_id: phen_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(hardener_mod_id, true);
            snap.module_online_states.insert(phen_mod_id, true);
        });
        ctx.invalidate_ship_fitting_cache_only(ship_id);
        let clean_cs = ctx.build_ship_combat_state(char_id, ship_id, hull_type_id);

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.insert(hardener_mod_id);
            snap.module_active_states.insert(phen_mod_id);
            snap.ship_fit_inputs
                .entry(ship_id)
                .or_default()
                .overloaded_modules
                .insert(hardener_mod_id);
        });
        let dbuff_val = 25.0;
        let expiry = ctx.now_unix_secs() + 600;
        ctx.scanners_superweapons.with_ship_dbuffs_mut(|dbuffs| {
            dbuffs.insert(
                ship_id,
                vec![crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: hp_dbuff_id,
                    value: dbuff_val,
                    expires_at_unix: expiry,
                    source_module_id: phen_mod_id,
                }],
            );
            dbuffs.insert(
                fleetmate_id,
                vec![crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: hp_dbuff_id,
                    value: dbuff_val,
                    expires_at_unix: expiry,
                    source_module_id: phen_mod_id,
                }],
            );
        });
        ctx.invalidate_ship_fitting_cache_only(ship_id);
        let inflated_cs = ctx.build_ship_combat_state(char_id, ship_id, hull_type_id);
        assert!(
            inflated_cs.shield_resonance[0] < clean_cs.shield_resonance[0] - 1e-4,
            "active overloaded hardener must lower pre-jump EM resonance: inflated={:?} clean={:?}",
            inflated_cs.shield_resonance,
            clean_cs.shield_resonance
        );
        if buffs_shield {
            assert!(
                (inflated_cs.max_shield_hp - clean_cs.max_shield_hp).abs() > 1.0,
                "phenomena dbuff must change pre-jump max_shield_hp"
            );
        } else {
            assert!(
                (inflated_cs.max_armor_hp - clean_cs.max_armor_hp).abs() > 1.0,
                "phenomena dbuff must change pre-jump max_armor_hp"
            );
        }
        (hardener_mod_id, phen_mod_id, clean_cs, inflated_cs)
    };

    // Part A (BF-1237C-02 forward): destination CombatState and FitProjection match deactivated baseline.
    {
        let (ctx, jumpable_phen_tid) = make_jumpable_phen_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a non-Zarzakh stargate pair");
        let char_id = 90_000_001_i64;
        let ship_id = 895_730_i64;
        let fleetmate_id = 895_739_i64;
        let (hardener_mod_id, phen_mod_id, clean_cs, inflated_cs) =
            setup_fit(&ctx, jumpable_phen_tid, char_id, ship_id, fleetmate_id, gate.from_solar_system_id, gate.position);

        let old_handle = ctx
            .systems
            .get_handle(gate.from_solar_system_id)
            .expect("origin handle");
        old_handle
            .set_combat_state(ship_id, inflated_cs)
            .await
            .expect("seed inflated origin CombatState");

        let mut sess = SessionState::new(70_730);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);

        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("CmdStargateJump succeeds");

        let dest_handle = ctx
            .systems
            .get_handle(gate.to_solar_system_id)
            .expect("dest handle");
        let dest_cs = dest_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .expect("dest combat state present");
        for i in 0..4 {
            assert!(
                (dest_cs.shield_resonance[i] - clean_cs.shield_resonance[i]).abs() < 1e-6,
                "dest shield_resonance[{i}] must match deactivated baseline: got {} want {}",
                dest_cs.shield_resonance[i],
                clean_cs.shield_resonance[i]
            );
        }
        assert!(
            (dest_cs.max_shield_hp - clean_cs.max_shield_hp).abs() < 1e-2,
            "dest max_shield_hp must reflect deactivated/unbuffed stats: got {} want {}",
            dest_cs.max_shield_hp,
            clean_cs.max_shield_hp
        );
        assert!(
            dest_cs
                .base_max_shield_hp
                .is_some_and(|v| (v - clean_cs.max_shield_hp).abs() < 1e-2),
            "dest base_max_shield_hp must not pin inflated value: got {:?}",
            dest_cs.base_max_shield_hp
        );
        assert!(
            (dest_cs.max_armor_hp - clean_cs.max_armor_hp).abs() < 1e-2,
            "dest max_armor_hp must reflect deactivated/unbuffed stats: got {} want {}",
            dest_cs.max_armor_hp,
            clean_cs.max_armor_hp
        );
        assert!(
            dest_cs
                .base_max_armor_hp
                .is_some_and(|v| (v - clean_cs.max_armor_hp).abs() < 1e-2),
            "dest base_max_armor_hp must not pin inflated value: got {:?}",
            dest_cs.base_max_armor_hp
        );

        // Step once in destination system to ensure `apply_fit_projection` holds clean baseline.
        dest_handle.step_manual(1.0).await.expect("dest step");
        let after_step_cs = dest_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .expect("dest combat state after step");
        assert!(
            (after_step_cs.max_shield_hp - clean_cs.max_shield_hp).abs() < 1e-2
                && (after_step_cs.max_armor_hp - clean_cs.max_armor_hp).abs() < 1e-2,
            "post-jump system ticks must keep clean deactivated baseline"
        );
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.module_active_states.contains(&hardener_mod_id));
            assert!(!snap.module_active_states.contains(&phen_mod_id));
            assert!(
                snap.ship_fit_inputs
                    .get(&ship_id)
                    .is_none_or(|r| !r.overloaded_modules.contains(&hardener_mod_id)),
                "hardener overload must be cleared after successful jump"
            );
        });
        ctx.scanners_superweapons.with_ship_dbuffs(|dbuffs| {
            assert!(!dbuffs.contains_key(&ship_id));
            assert!(!dbuffs.contains_key(&fleetmate_id));
        });
    }

    // Part B (BF-1237C-02 rollback): failed spawn restores active modules, overloads, and self/fleetmate dbuffs.
    {
        let (ctx, jumpable_phen_tid) = make_jumpable_phen_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("SDE must include a non-Zarzakh stargate pair");
        let char_id = 90_000_001_i64;
        let ship_id = 895_731_i64;
        let fleetmate_id = 895_740_i64;
        let (hardener_mod_id, phen_mod_id, _clean_cs, _inflated_cs) =
            setup_fit(&ctx, jumpable_phen_tid, char_id, ship_id, fleetmate_id, gate.from_solar_system_id, gate.position);

        let dest_handle = ctx.systems.get_or_spawn(gate.to_solar_system_id);
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::Release);

        let mut sess = SessionState::new(70_731);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let err = handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdStargateJump",
            &args,
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect_err("spawn panic must trigger rollback");
        assert_eq!(err.code, "SystemTransitionFailed");

        ctx.db.with_snapshot(|snap| {
            assert!(
                snap.module_active_states.contains(&hardener_mod_id),
                "hardener active state must be restored on rollback"
            );
            assert!(
                snap.module_active_states.contains(&phen_mod_id),
                "phenomena active state must be restored on rollback"
            );
            assert!(
                snap.ship_fit_inputs
                    .get(&ship_id)
                    .is_some_and(|r| r.overloaded_modules.contains(&hardener_mod_id)),
                "hardener overload must be restored on rollback"
            );
        });
        ctx.scanners_superweapons.with_ship_dbuffs(|dbuffs| {
            assert_eq!(
                dbuffs
                    .get(&ship_id)
                    .map(|v| v.len()),
                Some(1),
                "ship's own phenomena dbuffs must be restored on rollback"
            );
            assert_eq!(
                dbuffs
                    .get(&fleetmate_id)
                    .map(|v| v.len()),
                Some(1),
                "fleetmate phenomena dbuffs must remain untouched on failed jump"
            );
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stargate_rollback_guard_drop_under_world_lock_defers_atomic_rollback_without_split_brain_1237() {
    let ctx = test_ctx();
    let gate = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH
                && j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
        })
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must include a Zarzakh gate");

    let char_id = 90_000_001_i64;
    let ship_id = 895_741_i64;
    let start_isk = eve_db::IskCents::from_isk(100_000);
    let toll = eve_db::IskCents::from_isk(10_000);
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.remove(&char_id);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.war_faction_id = None;
        ch.balance = start_isk;
    });
    let old_handle = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("origin handle");
    let dest_handle = ctx.systems.get_or_spawn(SOLAR_SYSTEM_ZARZAKH);
    let taken = old_handle
        .take_ball(ship_id)
        .await
        .expect("take origin ball");

    let mut sess = SessionState::new(70_741);
    mark_char_online(&ctx, &mut sess, char_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(gate.from_solar_system_id);

    let mut guard = StargateRollbackGuard::new(
        &ctx,
        char_id,
        ship_id,
        TakenBallGuard::new(old_handle.clone(), taken.clone()),
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance.0 -= toll.0;
    });
    guard.record_zarzakh_toll(Some(toll));

    let (_ch, db_rb) = commit_stargate_db_transition(
        &ctx,
        &sess,
        char_id,
        ship_id,
        SOLAR_SYSTEM_ZARZAKH,
        Some((None, None, gate.from_solar_system_id)),
        true,
        DVec3::new(1_000.0, 0.0, 0.0),
        taken.combat_state.as_ref(),
    )
    .expect("commit_stargate_db_transition succeeds");
    guard.record_db_commit(db_rb);

    let spawn_token = next_stargate_spawn_token();
    let mut dest_ball = taken.ball.clone();
    dest_ball.position = DVec3::new(1_000.0, 0.0, 0.0);
    guard.record_dest_spawn_target(dest_handle.clone(), ship_id, spawn_token);
    dest_handle
        .spawn_ball_with_meta_tagged(
            dest_ball,
            taken.combat_state.clone().unwrap_or_default(),
            ctx.build_ship_slim_meta(char_id, 587),
            spawn_token,
        )
        .await
        .expect("materialize tagged destination ball");

    // Drop `StargateRollbackGuard` while holding `ctx.db`'s `RANK_WORLD_INNER` write lock
    // (`!lock_safe_for_db`). It must defer the full atomic rollback+restore without split-brain.
    ctx.db.with_mut_snapshot(|_| {
        drop(guard);
    });

    let mut rolled_back = false;
    for _ in 0..100 {
        let done_db = ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            ch.solar_system_id == gate.from_solar_system_id && ch.balance == start_isk
        });
        let dest_gone = dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_none();
        let orig_back = old_handle.get_ball_snapshot(ship_id).await.unwrap().is_some();
        if done_db && dest_gone && orig_back {
            rolled_back = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        rolled_back,
        "deferred Drop while RANK_WORLD_INNER held must atomically roll back DB, toll, dest ball, and origin ball without split-brain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_remove_ball_if_token_does_not_clobber_subsequent_retry_jump_ball_1237() {
    let ctx = test_ctx();
    let dest_sys_id = 30_000_144_u32;
    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let ship_id = 895_742_i64;
    let pos_v1 = DVec3::new(1_000.0, 2_000.0, 3_000.0);
    let pos_v2 = DVec3::new(50_000.0, -4_000.0, 5_000.0);
    let make_ball = |pos: DVec3| Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    let tok_1 = next_stargate_spawn_token();
    dest_handle
        .spawn_ball_with_meta_tagged(
            make_ball(pos_v1),
            CombatState::default(),
            eve_destiny::SlimItemMeta::default(),
            tok_1,
        )
        .await
        .expect("spawn ball v1");
    let tok_2 = next_stargate_spawn_token();
    dest_handle
        .spawn_ball_with_meta_tagged(
            make_ball(pos_v2),
            CombatState::default(),
            eve_destiny::SlimItemMeta::default(),
            tok_2,
        )
        .await
        .expect("spawn ball v2");

    // Stale token removal (from a delayed rollback / mailbox-full drop task) must be a no-op
    // and must keep both the newer ball AND `ball_positions[ship_id]` intact.
    enqueue_remove_ball_nonblocking(&dest_handle, ship_id, tok_1);
    assert!(
        dest_handle.remove_ball_if_token(ship_id, tok_1).await.is_none(),
        "stale token RemoveBallIfToken must return None without removing v2"
    );
    let (_, live_ball) = dest_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("subsequent retry jump ball must remain live");
    assert_eq!(live_ball.position, pos_v2);
    assert_eq!(
        dest_handle.get_ball_position_sync(ship_id),
        Some(pos_v2),
        "stale RemoveBallIfToken must not clear ball_positions cache for newer jump"
    );

    // Matching token removes cleanly.
    let removed = dest_handle.remove_ball_if_token(ship_id, tok_2).await;
    assert!(removed.is_some());
    assert!(dest_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_cleans_attacker_cycles_ewar_remote_assist_and_notices_before_dest_combat_state_1237() {
    let ctx = test_ctx();
    let gate = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
                && ctx
                    .sde
                    .solar_systems
                    .get(&j.from_solar_system_id)
                    .is_some_and(|s| s.security_status <= 0.0)
        })
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must include a non-Zarzakh nullsec stargate pair");

    let hull_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 25 && t.published) // Frigate
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("SDE must include a published Frigate hull");
    let tp_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 379 && t.published) // Target Painter
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("SDE must include a Target Painter");
    let scram_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 52 && t.published) // Warp Scrambler
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("SDE must include a Warp Scrambler");
    let rsb_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 290 && t.published) // Remote Sensor Booster
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("SDE must include a Remote Sensor Booster");

    let char_a = 90_000_001_i64;
    let ship_a = 895_750_i64;
    let char_b = 90_000_751_i64;
    let user_b = 77_751_u64;
    let ship_b = 895_751_i64;

    let tp_mod_id = 895_801_i64;
    let scram_mod_id = 895_802_i64;
    let rsb_mod_id = 895_803_i64;
    let charge_tid = 29_005_u32;

    spawn_ship_in_space(&ctx, char_a, ship_a, hull_type_id, gate.from_solar_system_id, gate.position);
    let clean_cs = ctx.build_ship_combat_state(char_a, ship_a, hull_type_id);

    ctx.db.with_mut_snapshot(|snap| {
        let mut acc_b = snap.accounts.values().next().cloned().expect("seed account");
        acc_b.user_id = user_b;
        acc_b.username = "attacker_b_1237".to_string();
        snap.accounts.insert(acc_b.username.clone(), acc_b);

        let mut ch_b = snap.characters.get(&char_a).cloned().expect("seed char");
        ch_b.character_id = char_b;
        ch_b.user_id = user_b;
        ch_b.name = "Attacker B".to_string();
        ch_b.active_ship_id = ship_b;
        ch_b.solar_system_id = gate.from_solar_system_id;
        ch_b.station_id = None;
        ch_b.structure_id = None;
        snap.characters.insert(char_b, ch_b);

        for (item_id, type_id, flag) in [
            (tp_mod_id, tp_type_id, InventoryFlag::MedSlot0),
            (scram_mod_id, scram_type_id, InventoryFlag::MedSlot1),
            (rsb_mod_id, rsb_type_id, InventoryFlag::MedSlot2),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_b,
                    location_id: ship_b,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(item_id, true);
            snap.module_active_states.insert(item_id);
        }
        snap.module_active_charges.insert(rsb_mod_id, charge_tid);
    });
    spawn_ship_in_space(
        &ctx,
        char_b,
        ship_b,
        hull_type_id,
        gate.from_solar_system_id,
        gate.position + DVec3::new(1_000.0, 0.0, 0.0),
    );

    let mut sess_a = SessionState::new(70_750);
    mark_char_online(&ctx, &mut sess_a, char_a);
    sess_a.character_id = Some(char_a);
    sess_a.ship_id = Some(ship_a);
    sess_a.solar_system_id = Some(gate.from_solar_system_id);

    let mut sess_b = SessionState::new(70_751);
    mark_char_online(&ctx, &mut sess_b, char_b);
    sess_b.character_id = Some(char_b);
    sess_b.ship_id = Some(ship_b);
    sess_b.solar_system_id = Some(gate.from_solar_system_id);

    let old_handle = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("origin handle");
    old_handle
        .add_target_lock(
            ship_b,
            eve_destiny::TargetLock {
                target_id: ship_a,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .expect("lock target ship_a");

    // Seed Target Painter projected_ewar + active_ewar_starts + Remote Sensor Booster tactical state.
    let mut tp_eff = eve_dogma::DogmaItemState::from_sde(tp_mod_id, tp_type_id, &ctx.sde);
    tp_eff.base_attributes.insert(54, 50_000.0);
    tp_eff.base_attributes.insert(554, 30.0);
    ctx.projected_ewar_sources
        .write()
        .unwrap()
        .insert(tp_mod_id, ship_b);
    ctx.projected_ewar.write().unwrap().insert(
        ship_a,
        vec![eve_dogma::ProjectedEwarEffect {
            source_module_id: tp_mod_id,
            source_module_type_id: tp_type_id,
            charge_type_id: None,
            distance: 1_000.0,
            overloaded: false,
            effective_module: tp_eff,
        }],
    );
    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((ship_b, tp_mod_id), vec![(ship_a, "targetPainter")]);

    let rsb_type = ctx.sde.types.get(&rsb_type_id).expect("rsb type");
    let mut rsb_state = eve_dogma::DogmaItemState::from_sde(rsb_mod_id, rsb_type_id, &ctx.sde);
    rsb_state.base_attributes.insert(54, 50_000.0);
    crate::services::tactical_modules::apply_projected_remote_assistance(
        &ctx,
        char_b,
        ship_b,
        rsb_mod_id,
        rsb_type,
        &rsb_state,
        Some(ship_a),
        1_000.0,
    )
    .expect("apply_projected_remote_assistance");

    for (module_id, module_type_id, effect_name, ammo_type_id) in [
        (tp_mod_id, tp_type_id, "targetPaint", None),
        (scram_mod_id, scram_type_id, "warpScramble", None),
        (rsb_mod_id, rsb_type_id, "remoteSensorBoost", Some(charge_tid)),
    ] {
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id: ship_b,
                module_id,
                module_type_id,
                owner_char_id: Some(char_b),
                effect_name: effect_name.into(),
                target_id: Some(ship_a),
                repeat: 100,
                duration_s: 5.0,
                ammo_type_id,
                ..Default::default()
            })
            .await
            .expect("add attacker cycle");
    }

    // Confirm pre-jump TP & Remote Assistance are active against ship_a.
    let pre_jump_cs = ctx.build_ship_combat_state(char_a, ship_a, hull_type_id);
    assert!(
        pre_jump_cs.tp_sig_multiplier.is_some_and(|m| m > 1.0 + 1e-4),
        "pre-jump TP must inflate ship_a tp_sig_multiplier: {pre_jump_cs:?}"
    );
    assert!(
        pre_jump_cs.signature_radius > clean_cs.signature_radius + 1e-2,
        "pre-jump signature_radius must be painted"
    );

    let _ = ctx.drain_notifications_for_char(char_b);

    let args = [
        PyRep::Integer(gate.stargate_id),
        PyRep::Integer(gate.destination_stargate_id),
        PyRep::Integer(ship_a),
    ];
    handle(
        &ctx,
        &mut sess_a,
        None,
        "beyonce",
        "CmdStargateJump",
        &args,
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("CmdStargateJump succeeds when scrambler cycle is cleared before jump");

    // Attacker DB active states / charges and projected_ewar / remote_assistance must be cleared.
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&tp_mod_id));
        assert!(!snap.module_active_states.contains(&scram_mod_id));
        assert!(!snap.module_active_states.contains(&rsb_mod_id));
        assert!(!snap.module_active_charges.contains_key(&rsb_mod_id));
    });
    assert!(
        ctx.projected_ewar
            .read()
            .unwrap()
            .get(&ship_a)
            .is_none_or(|v| v.is_empty()),
        "projected_ewar on ship_a must be flushed on jump"
    );
    assert!(
        !ctx.tactical_state
            .read()
            .unwrap()
            .remote_assistance
            .values()
            .flatten()
            .any(|e| e.source_ship_id == ship_b),
        "remote_assistance from ship_b must be flushed on jump"
    );

    // Attacker must receive deactivation notices (OnGodmaShipEffect, OnSpecialFX, OnEwarEnd).
    let notifs_b = ctx.drain_notifications_for_char(char_b);
    for mod_id in [tp_mod_id, scram_mod_id, rsb_mod_id] {
        assert!(
            notifs_b.iter().any(|n| {
                n.event == "OnGodmaShipEffect"
                    && n.args.first().and_then(|r| r.as_i64()) == Some(mod_id)
                    && n.args.get(4).and_then(|r| r.as_i64()) == Some(0)
            }),
            "attacker char_b must receive OnGodmaShipEffect(active=0) for module {mod_id}"
        );
        assert!(
            notifs_b.iter().any(|n| {
                n.event == "OnSpecialFX"
                    && n.args.get(1).and_then(|r| r.as_i64()) == Some(mod_id)
                    && n.args.get(8).and_then(|r| r.as_i64()) == Some(0)
            }),
            "attacker char_b must receive OnSpecialFX(active=0) for module {mod_id}"
        );
    }
    assert!(
        notifs_b.iter().any(|n| n.event == "OnEwarEnd"),
        "attacker char_b must receive OnEwarEnd when TP stops on target jump"
    );

    // Destination ship CombatState must have clean un-painted signature radius and tp_sig_multiplier == None.
    let dest_handle = ctx
        .systems
        .get_handle(gate.to_solar_system_id)
        .expect("dest handle");
    let dest_cs = dest_handle
        .get_combat_state(ship_a)
        .await
        .unwrap()
        .expect("dest combat state");
    assert_eq!(dest_cs.tp_sig_multiplier, None);
    assert!(
        (dest_cs.signature_radius - clean_cs.signature_radius).abs() < 1e-4,
        "dest signature_radius {} must match clean baseline {}",
        dest_cs.signature_radius,
        clean_cs.signature_radius
    );
    assert!(
        old_handle
            .actor_state
            .read()
            .unwrap()
            .active_module_cycles
            .is_empty(),
        "origin actor must retain none of the attacker cycles targeting jumped ship"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_clears_and_rolls_back_cycle_transient_item_attrs_preserving_rack_heat_1237() {
    let setup_transient_ship = |ctx: &ServerContext,
                                char_id: i64,
                                ship_id: i64,
                                from_sys: u32,
                                gate_pos: DVec3| {
        let hull_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 27 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("Battleship hull");
        let adc_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 60 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("Damage Control module");
        let rah_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 1150 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(adc_type_id);
        let muta_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 1986 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(adc_type_id);

        let adc_mod_id = ship_id + 101;
        let rah_mod_id = ship_id + 102;
        let muta_mod_id = ship_id + 103;
        spawn_ship_in_space(ctx, char_id, ship_id, hull_type_id, from_sys, gate_pos);
        ctx.db.with_mut_snapshot(|snap| {
            for (item_id, type_id, flag) in [
                (adc_mod_id, adc_type_id, InventoryFlag::LoSlot0),
                (rah_mod_id, rah_type_id, InventoryFlag::LoSlot1),
                (muta_mod_id, muta_type_id, InventoryFlag::HiSlot0),
            ] {
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                snap.module_online_states.insert(item_id, true);
                snap.module_active_states.insert(item_id);
            }
            let ship_tr = snap.transient_item_attributes.entry(ship_id).or_default();
            for aid in [109_u16, 110, 111, 112, 113, 267, 268, 269, 270, 271, 272, 273, 274] {
                ship_tr.insert(aid, 0.25);
            }
            // Persisted rack heat must survive stargate transitions untouched.
            ship_tr.insert(1175, 0.42);
            ship_tr.insert(1176, 0.25);
            ship_tr.insert(1177, 0.10);

            let rah_tr = snap.transient_item_attributes.entry(rah_mod_id).or_default();
            for aid in 267_u16..=270 {
                rah_tr.insert(aid, 0.72);
            }
            snap.transient_item_attributes
                .entry(muta_mod_id)
                .or_default()
                .insert(84, 115.0);
        });
        (adc_mod_id, rah_mod_id, muta_mod_id, adc_type_id, rah_type_id)
    };

    // Part A: Successful jump clears ADC/EHE/RAH & mutadaptive transient attrs while preserving rack heat.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("non-Zarzakh gate");
        let char_id = 90_000_001_i64;
        let ship_id = 895_760_i64;
        let (adc_mod_id, rah_mod_id, muta_mod_id, adc_type_id, rah_type_id) =
            setup_transient_ship(&ctx, char_id, ship_id, gate.from_solar_system_id, gate.position);

        let old_handle = ctx.systems.get_handle(gate.from_solar_system_id).unwrap();
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: adc_mod_id,
                module_type_id: adc_type_id,
                owner_char_id: Some(char_id),
                effect_name: "assaultDamageControl".into(),
                repeat: 1,
                duration_s: 12.0,
                is_adc: true,
                ..Default::default()
            })
            .await
            .unwrap();
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: rah_mod_id,
                module_type_id: rah_type_id,
                owner_char_id: Some(char_id),
                effect_name: "reactiveArmorHardener".into(),
                repeat: 100,
                duration_s: 6.0,
                is_rah: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let mut sess = SessionState::new(70_760);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        handle(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &args, &HashMap::new())
            .await
            .expect("handled")
            .expect("jump succeeds");

        ctx.db.with_snapshot(|snap| {
            let ship_tr = snap
                .transient_item_attributes
                .get(&ship_id)
                .expect("rack heat keeps ship entry");
            for aid in [109_u16, 110, 111, 112, 113, 267, 268, 269, 270, 271, 272, 273, 274] {
                assert!(!ship_tr.contains_key(&aid), "ship transient attr {aid} must clear on jump");
            }
            assert!((ship_tr[&1175] - 0.42).abs() < 1e-6);
            assert!((ship_tr[&1176] - 0.25).abs() < 1e-6);
            assert!((ship_tr[&1177] - 0.10).abs() < 1e-6);
            assert!(!snap.transient_item_attributes.contains_key(&rah_mod_id));
            assert!(!snap.transient_item_attributes.contains_key(&muta_mod_id));
        });

        let ship_info = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("ItemGetInfo succeeds");
        let ship_attrs = ship_info
            .dict_get("attributes")
            .and_then(|r| match r {
                PyRep::Dict(d) => Some(d),
                _ => None,
            })
            .expect("ItemGetInfo attributes dict");
        assert!(ship_attrs.iter().any(|(k, v)| {
            k.as_i64() == Some(1175) && matches!(v, PyRep::Real(x) if (*x - 0.42).abs() < 1e-6)
        }));
        let muta_info = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "ItemGetInfo",
            &[PyRep::Integer(muta_mod_id)],
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("ItemGetInfo succeeds");
        let muta_attrs = muta_info
            .dict_get("attributes")
            .and_then(|r| match r {
                PyRep::Dict(d) => Some(d),
                _ => None,
            })
            .expect("muta attributes dict");
        assert!(
            !muta_attrs.iter().any(|(k, v)| {
                k.as_i64() == Some(84) && matches!(v, PyRep::Real(x) if (*x - 115.0).abs() < 1e-6)
            }),
            "mutadaptive spool transient attr 84 must clear on jump"
        );
    }

    // Part B: Failed jump rolls back ADC/EHE/RAH/mutadaptive transient attrs AND rack heat.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("non-Zarzakh gate");
        let char_id = 90_000_001_i64;
        let ship_id = 895_761_i64;
        let (adc_mod_id, rah_mod_id, muta_mod_id, adc_type_id, rah_type_id) =
            setup_transient_ship(&ctx, char_id, ship_id, gate.from_solar_system_id, gate.position);

        let old_handle = ctx.systems.get_handle(gate.from_solar_system_id).unwrap();
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: adc_mod_id,
                module_type_id: adc_type_id,
                owner_char_id: Some(char_id),
                effect_name: "assaultDamageControl".into(),
                repeat: 1,
                duration_s: 12.0,
                is_adc: true,
                ..Default::default()
            })
            .await
            .unwrap();
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: rah_mod_id,
                module_type_id: rah_type_id,
                owner_char_id: Some(char_id),
                effect_name: "reactiveArmorHardener".into(),
                repeat: 100,
                duration_s: 6.0,
                is_rah: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut pre_cs = old_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        pre_cs.rah_armor_resonance = Some([0.2, 0.3, 0.7, 0.8]);
        old_handle.set_combat_state(ship_id, pre_cs).await.unwrap();
        let (pre_ship_tr, pre_rah_tr, pre_muta_tr) = ctx.db.with_snapshot(|snap| {
            (
                snap.transient_item_attributes[&ship_id].clone(),
                snap.transient_item_attributes[&rah_mod_id].clone(),
                snap.transient_item_attributes[&muta_mod_id].clone(),
            )
        });

        let dest_handle = ctx.systems.get_or_spawn(gate.to_solar_system_id);
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::Release);

        let mut sess = SessionState::new(70_761);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let err = handle(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &args, &HashMap::new())
            .await
            .expect("handled")
            .expect_err("spawn panic must roll back");
        assert_eq!(err.code, "SystemTransitionFailed");

        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.transient_item_attributes[&ship_id], pre_ship_tr);
            assert_eq!(snap.transient_item_attributes[&rah_mod_id], pre_rah_tr);
            assert_eq!(snap.transient_item_attributes[&muta_mod_id], pre_muta_tr);
        });
        let restored_cs = old_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(restored_cs.adc_active);
        assert_eq!(restored_cs.rah_armor_resonance, Some([0.2, 0.3, 0.7, 0.8]));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cmd_stargate_jump_rollback_keeps_db_and_origin_actor_cycles_in_sync_1237() {
    let setup_cycled_ship = |ctx: &ServerContext,
                             char_id: i64,
                             ship_id: i64,
                             from_sys: u32,
                             gate_pos: DVec3| {
        let hull_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 27 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .expect("Battleship hull");
        let ab_mod_id = ship_id + 201;
        let hardener_mod_id = ship_id + 202;
        let wcs_mod_id = ship_id + 203;
        let ab_type_id = 12058_u32; // 10MN Afterburner II or fallback group 46
        let ab_type_id = if ctx.sde.types.contains_key(&ab_type_id) {
            ab_type_id
        } else {
            ctx.sde
                .types
                .values()
                .filter(|t| t.group_id == 46 && t.published)
                .min_by_key(|t| t.type_id)
                .map(|t| t.type_id)
                .expect("propulsion module")
        };
        let hardener_type_id = 2281_u32;
        let wcs_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 315 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(2281);

        spawn_ship_in_space(ctx, char_id, ship_id, hull_type_id, from_sys, gate_pos);
        ctx.db.with_mut_snapshot(|snap| {
            for (item_id, type_id, flag) in [
                (ab_mod_id, ab_type_id, InventoryFlag::MedSlot0),
                (hardener_mod_id, hardener_type_id, InventoryFlag::MedSlot1),
                (wcs_mod_id, wcs_type_id, InventoryFlag::LoSlot0),
            ] {
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                snap.module_online_states.insert(item_id, true);
                snap.module_active_states.insert(item_id);
            }
            snap.ship_fit_inputs
                .entry(ship_id)
                .or_default()
                .overloaded_modules
                .insert(hardener_mod_id);
        });
        (ab_mod_id, hardener_mod_id, wcs_mod_id, ab_type_id, hardener_type_id, wcs_type_id)
    };

    // Part A: Post-commit destination spawn failure restores both DB state AND origin-actor cycles.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("non-Zarzakh gate");
        let char_id = 90_000_001_i64;
        let ship_id = 895_770_i64;
        let (ab_mod_id, hardener_mod_id, wcs_mod_id, ab_tid, hard_tid, wcs_tid) =
            setup_cycled_ship(&ctx, char_id, ship_id, gate.from_solar_system_id, gate.position);

        let old_handle = ctx.systems.get_handle(gate.from_solar_system_id).unwrap();
        let pre_cs = old_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        let pre_core = pre_cs.warp_core_strength;
        let pre_ball = old_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;

        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: ab_mod_id,
                module_type_id: ab_tid,
                owner_char_id: Some(char_id),
                effect_name: "moduleBonusAfterburner".into(),
                repeat: 100,
                duration_s: 10.0,
                cap_need_per_cycle: 10.0,
                speed_boost_factor: 1.5,
                ..Default::default()
            })
            .await
            .unwrap();
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: hardener_mod_id,
                module_type_id: hard_tid,
                owner_char_id: Some(char_id),
                effect_name: "shieldHardener".into(),
                repeat: 100,
                duration_s: 12.0,
                cap_need_per_cycle: 8.0,
                is_overloaded: true,
                ..Default::default()
            })
            .await
            .unwrap();
        old_handle
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: wcs_mod_id,
                module_type_id: wcs_tid,
                owner_char_id: Some(char_id),
                effect_name: "warpCoreStabilizer".into(),
                repeat: 100,
                duration_s: 10.0,
                warp_core_stab_bonus: 2,
                grants_interdiction_nullification: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let dest_handle = ctx.systems.get_or_spawn(gate.to_solar_system_id);
        dest_handle
            .panic_next_command_for_test
            .store(true, std::sync::atomic::Ordering::Release);

        let mut sess = SessionState::new(70_770);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let err = handle(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &args, &HashMap::new())
            .await
            .expect("handled")
            .expect_err("dest spawn panic rolls back");
        assert_eq!(err.code, "SystemTransitionFailed");

        ctx.db.with_snapshot(|snap| {
            assert!(snap.module_active_states.contains(&ab_mod_id));
            assert!(snap.module_active_states.contains(&hardener_mod_id));
            assert!(snap.module_active_states.contains(&wcs_mod_id));
            assert!(snap
                .ship_fit_inputs
                .get(&ship_id)
                .is_some_and(|r| r.overloaded_modules.contains(&hardener_mod_id)));
        });

        let restored_ids: Vec<i64> = {
            let st = old_handle.actor_state.read().unwrap();
            st.active_module_cycles
                .iter()
                .filter(|c| c.ship_id == ship_id)
                .map(|c| c.module_id)
                .collect()
        };
        assert_eq!(restored_ids.len(), 3, "all 3 own cycles must be restored in origin actor: {restored_ids:?}");
        assert!(restored_ids.contains(&ab_mod_id));
        assert!(restored_ids.contains(&hardener_mod_id));
        assert!(restored_ids.contains(&wcs_mod_id));
        assert!(old_handle.view().active_module_ships.contains(&ship_id));

        let post_cs = old_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((post_cs.prop_velocity_multiplier - 2.5).abs() < 1e-6);
        assert_eq!(post_cs.warp_core_strength, pre_core + 2, "must not double-count warp_core_stab_bonus on rollback");
        assert!(post_cs.is_interdiction_nullified);
        let post_ball = old_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
        assert!(
            (post_ball.max_velocity - pre_ball.max_velocity * 2.5).abs() < 1e-3,
            "restored AB must restore max_velocity: got {} want {}",
            post_ball.max_velocity,
            pre_ball.max_velocity * 2.5
        );

        // Origin actor must keep advancing restored cycles when stepped.
        old_handle.step_manual(1.0).await.expect("origin step succeeds");
        let elapsed_after_step = {
            let st = old_handle.actor_state.read().unwrap();
            st.active_module_cycles
                .iter()
                .find(|c| c.module_id == ab_mod_id)
                .map(|c| c.elapsed_s)
        };
        assert!(
            elapsed_after_step.is_some_and(|e| e >= 0.99),
            "restored actor cycle must keep advancing on origin steps: {elapsed_after_step:?}"
        );
    }

    // Part B: Pre-commit lease failure rollback keeps DB active states & origin-actor cycles in sync.
    {
        let ctx = test_ctx();
        let gate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                j.from_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH
                    && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
            })
            .min_by_key(|j| j.stargate_id)
            .cloned()
            .expect("non-Zarzakh gate");
        let char_id = 90_000_001_i64;
        let ship_id = 895_771_i64;
        let other_id = 895_779_i64;
        let (ab_mod_id, hardener_mod_id, wcs_mod_id, ab_tid, hard_tid, wcs_tid) =
            setup_cycled_ship(&ctx, char_id, ship_id, gate.from_solar_system_id, gate.position);
        let cloak_mod_id = ship_id + 204;
        let target_mod_id = ship_id + 205;
        let tp_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| t.group_id == 379 && t.published)
            .min_by_key(|t| t.type_id)
            .map(|t| t.type_id)
            .unwrap_or(hard_tid);
        ctx.db.with_mut_snapshot(|snap| {
            for (item_id, type_id, flag) in [
                (cloak_mod_id, hard_tid, InventoryFlag::HiSlot0),
                (target_mod_id, tp_type_id, InventoryFlag::MedSlot2),
            ] {
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                snap.module_online_states.insert(item_id, true);
                snap.module_active_states.insert(item_id);
            }
        });

        let old_handle = ctx.systems.get_handle(gate.from_solar_system_id).unwrap();
        old_handle
            .try_spawn_ball_with_combat(
                Ball {
                    id: other_id,
                    type_id: 587,
                    position: gate.position + DVec3::new(5_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_100_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            );
        old_handle
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: other_id,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .unwrap();

        for (module_id, module_type_id, effect_name, speed_boost, target_id) in [
            (ab_mod_id, ab_tid, "moduleBonusAfterburner", 1.25, None),
            (hardener_mod_id, hard_tid, "shieldHardener", 0.0, None),
            (wcs_mod_id, wcs_tid, "warpCoreStabilizer", 0.0, None),
            (cloak_mod_id, hard_tid, "cloakingWarpSafe", -0.75, None),
            (target_mod_id, tp_type_id, "targetPaint", 0.0, Some(other_id)),
        ] {
            old_handle
                .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                    ship_id,
                    module_id,
                    module_type_id,
                    owner_char_id: Some(char_id),
                    effect_name: effect_name.into(),
                    target_id,
                    repeat: 100,
                    duration_s: 10.0,
                    speed_boost_factor: speed_boost,
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let mut cloak_cs = old_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        cloak_cs.is_covert_cloak = true;
        cloak_cs.cloak_stabilization_remaining_s = 420.0;
        cloak_cs.cloak_velocity_multiplier = Some(0.25);
        old_handle.set_combat_state(ship_id, cloak_cs).await.unwrap();

        let mut sess = SessionState::new(70_771);
        mark_char_online(&ctx, &mut sess, char_id);
        sess.character_id = Some(char_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(gate.from_solar_system_id);
        let _ = ctx.drain_notifications_for_char(char_id);

        let uid = sess.user_id.expect("online uid");
        let db_hook = Arc::clone(&ctx.db);
        set_post_take_ball_hook_for_test(
            &ctx,
            char_id,
            Some(Box::new(move || {
                let db = Arc::clone(&db_hook);
                Box::pin(async move {
                    // Supersede session lease right after TakeBall so pre-commit lease check fails cleanly.
                    let _ = db.acquire_online_lock(uid, 999_999, Some(char_id), true, db.clock().now_unix_secs());
                })
            })),
        );

        let args = [
            PyRep::Integer(gate.stargate_id),
            PyRep::Integer(gate.destination_stargate_id),
            PyRep::Integer(ship_id),
        ];
        let err = handle(&ctx, &mut sess, None, "beyonce", "CmdStargateJump", &args, &HashMap::new())
            .await
            .expect("handled")
            .expect_err("lease displacement after take_ball must roll back");
        assert!(!err.code.is_empty());

        ctx.db.with_snapshot(|snap| {
            assert!(snap.module_active_states.contains(&ab_mod_id));
            assert!(snap.module_active_states.contains(&hardener_mod_id));
            assert!(snap.module_active_states.contains(&wcs_mod_id));
            assert!(snap.module_active_states.contains(&cloak_mod_id));
            assert!(
                !snap.module_active_states.contains(&target_mod_id),
                "targeted cycle whose lock cleared on TakeBall must also clear DB active state on rollback"
            );
        });
        let restored_ids: Vec<i64> = {
            let st = old_handle.actor_state.read().unwrap();
            st.active_module_cycles
                .iter()
                .filter(|c| c.ship_id == ship_id)
                .map(|c| c.module_id)
                .collect()
        };
        assert_eq!(restored_ids.len(), 4, "pre-commit rollback must restore untargeted origin actor cycles in sync with DB: {restored_ids:?}");
        assert!(!restored_ids.contains(&target_mod_id));
        let restored_cloak_cs = old_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(restored_cloak_cs.is_covert_cloak);
        assert!((restored_cloak_cs.cloak_stabilization_remaining_s - 420.0).abs() < 1e-6);
        assert_eq!(restored_cloak_cs.cloak_velocity_multiplier, Some(0.25));
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(
            notifs.iter().any(|n| {
                n.event == "OnGodmaShipEffect"
                    && n.args.first().and_then(|r| r.as_i64()) == Some(target_mod_id)
                    && n.args.get(4).and_then(|r| r.as_i64()) == Some(0)
            }),
            "lock-broken targeted own cycle must emit OnGodmaShipEffect(active=0) on rollback"
        );
    }
}
