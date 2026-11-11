//! Unit tests covering `space_warp_jump_helpers` (`resolve_bound_or_session_sys`,
//! `bookmark_expired_err`, `emit_structure_dock_notifications`, `handle_undock`).

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn space_warp_jump_helpers_cover_bound_resolution_notifications_and_undock() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 98_800_001i64;
    let ship_id = 98_800_100i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;

    let mut sess = SessionState::new(98_801);
    let err_bound_req = ["Bound", "Object", "Required"].concat();
    assert_eq!(
        resolve_bound_or_session_sys(None, &sess, "beyonce")
            .expect_err("no solar system")
            .code,
        err_bound_req
    );

    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        resolve_bound_or_session_sys(None, &sess, "beyonce").expect("session sys"),
        sys_id
    );

    let bound_ok = BoundObjectEntry {
        bound_id: 1,
        session_id: 98_801,
        user_id: 1,
        character_id: char_id,
        service: "beyonce".into(),
        object_id: i64::from(sys_id),
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    assert_eq!(
        resolve_bound_or_session_sys(Some(&bound_ok), &sess, "beyonce").expect("bound ok"),
        sys_id
    );

    let bound_bad = BoundObjectEntry {
        service: "park".into(),
        ..bound_ok
    };
    let err_invalid_svc = ["Invalid", "Bound", "Service"].concat();
    assert_eq!(
        resolve_bound_or_session_sys(Some(&bound_bad), &sess, "beyonce")
            .expect_err("wrong service")
            .code,
        err_invalid_svc
    );

    let err_bm = ["Bookmark", "Expired"].concat();
    assert_eq!(bookmark_expired_err(77).code, err_bm);

    // Seed character and docked ship first so notifications & handle_undock resolve recipient session
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap
            .characters
            .get(&90_000_001)
            .expect("seed char")
            .clone();
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id: 1,
                name: "UndockPilot".into(),
                corporation_id: 1_000_009,
                solar_system_id: sys_id,
                station_id: Some(station_id),
                structure_id: None,
                home_station_id: station_id,
                active_ship_id: ship_id,
                ..base_char
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.db
        .acquire_online_lock_with_computer_hash(1, 98_801, Some(char_id), None, true, 100)
        .expect("online lock");

    emit_structure_dock_notifications(
        &ctx,
        station_id,
        char_id,
        1_000_009,
        Some(99_000_001),
        Some(1_000_035),
        52678,
        ship_id,
    );
    let char_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(char_notifs
        .iter()
        .any(|n| n.event == "OnHangarViewStateUpdated"));
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    let kw = HashMap::new();
    let _ = handle_undock(&ctx, &mut sess, &[PyRep::Integer(station_id)], &kw)
        .await
        .expect("undock handled")
        .expect("undock ok");

    // Second immediate handle_undock hits active 10s session-change timer guard
    let timer_err = handle_undock(&ctx, &mut sess, &[PyRep::Integer(station_id)], &kw)
        .await
        .expect("undock handled")
        .expect_err("session change timer blocks immediate second undock");
    assert_eq!(timer_err.code, "SessionChangeInProgress");
}

async fn call_beyonce(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = std::collections::HashMap::new();
    super::super::handle(ctx, session, None, "beyonce", method, args, &kw)
        .await
        .expect("beyonce method handled")
}

#[tokio::test]
async fn handle_undock_no_char_and_missing_session_sys_fallback_and_perimeter_checks() {
    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;
    use std::sync::Arc;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let kw = std::collections::HashMap::new();
    let station_id = 60_003_760i64;
    let sys_id: u32 = 30_000_142;
    let char_id = 95_100_001i64;
    let ship_id = 10_951_000_001i64;

    let mut no_char_sess = SessionState::new(74_001);
    let err = handle_undock(&ctx, &mut no_char_sess, &[PyRep::Integer(station_id)], &kw)
        .await
        .expect("handled")
        .expect_err("missing character fails");
    assert!(!err.code.is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "UndockFallbackPilot".to_string();
        c.solar_system_id = sys_id;
        c.station_id = Some(station_id);
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess = SessionState::new(74_002);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = None;
    let _ = handle_undock(&ctx, &mut sess, &[PyRep::Integer(station_id)], &kw).await;

    let mut ball = Ball {
        id: ship_id,
        type_id: 593,
        position: DVec3::new(1000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let target_pos = DVec3::ZERO;
    assert!(super::super::evaluate_dock_perimeter(&ball, target_pos, 1500.0).is_ok());
    ball.position = DVec3::new(50_000.0, 0.0, 0.0);
    assert!(super::super::evaluate_dock_perimeter(&ball, target_pos, 1500.0).is_err());
}

#[tokio::test]
async fn warp_to_stuff_errors_movement_guards_cap_distance_and_stuff_types() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let sys_id: u32 = 30_000_142;
    let station_id = 60_003_760i64;
    let char_id = 95_100_010i64;
    let corp_id = 98_500_010i64;
    let ship_id = 10_951_000_010i64;
    let struct_id = 10_951_000_011i64;
    let ally_ship_id = 10_951_000_012i64;
    let ally_char_id = 95_100_011i64;

    let jita_gate_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.stargate_id)
        .unwrap_or(50_001_248);
    let jita_cels = ctx.sde.celestials_in_system(sys_id);
    let jita_belt_id = jita_cels.iter().find(|c| c.group_id == 9).map(|c| c.celestial_id);
    let jita_planet = jita_cels
        .iter()
        .find(|c| c.group_id == 7)
        .map(|c| (c.celestial_id, c.position))
        .unwrap_or((40_009_081, DVec3::ZERO));

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("seed char");
        let mut c = base.clone();
        c.character_id = char_id;
        c.name = "WarpPilot".to_string();
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        let mut ally = base;
        ally.character_id = ally_char_id;
        ally.name = "FleetAnchor".to_string();
        ally.corporation_id = corp_id;
        ally.solar_system_id = sys_id;
        ally.station_id = None;
        ally.active_ship_id = ally_ship_id;
        snap.characters.insert(ally_char_id, ally);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let spawn_ship = |id: i64, type_id: u32, pos: DVec3, cap: f64| {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
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
                capacitor: cap,
                max_capacitor: 300.0,
                ..Default::default()
            },
        );
    };
    spawn_ship(ship_id, 593, DVec3::ZERO, 0.0);
    spawn_ship(struct_id, 35832, DVec3::new(1_000_000_000.0, 0.0, 0.0), 0.0);
    spawn_ship(ally_ship_id, 593, DVec3::new(2_000_000_000.0, 0.0, 0.0), 300.0);

    let mut sess = SessionState::new(74_003);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[]).await.is_err());
    assert!(
        call_beyonce(
            &ctx,
            &mut sess,
            "CmdWarpToStuff",
            &[PyRep::String("item".to_string()), PyRep::Integer(station_id), PyRep::Integer(0)],
        )
        .await
        .is_err()
    );
    sess.station_id = None;

    let _ = sys_handle.set_warp_scramble_strength(ship_id, 2).await;
    assert!(
        call_beyonce(
            &ctx,
            &mut sess,
            "CmdWarpToStuff",
            &[PyRep::String("item".to_string()), PyRep::Integer(station_id), PyRep::Integer(0)],
        )
        .await
        .is_err()
    );
    let _ = sys_handle.set_warp_scramble_strength(ship_id, 0).await;

    let _ = call_beyonce(
        &ctx,
        &mut sess,
        "CmdWarpToStuff",
        &[PyRep::String("item".to_string()), PyRep::Integer(station_id), PyRep::Integer(0)],
    )
    .await;

    for (stype, sid) in [
        ("item", station_id),
        ("item", jita_gate_id),
        ("item", jita_planet.0),
        ("item", struct_id),
        ("char", ally_char_id),
        ("bookmark", 99_999_888),
        ("scan", 99_999_777),
    ] {
        let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
        sys_handle.try_update_combat_state(ship_id, |cs| cs.capacitor = 300.0);
        let _ = call_beyonce(
            &ctx,
            &mut sess,
            "CmdWarpToStuff",
            &[PyRep::String(stype.to_string()), PyRep::Integer(sid), PyRep::Integer(0)],
        )
        .await;
    }
    if let Some(belt_id) = jita_belt_id {
        let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
        sys_handle.try_update_combat_state(ship_id, |cs| cs.capacitor = 300.0);
        let _ = call_beyonce(&ctx, &mut sess, "CmdWarpToStuffAutopilot", &[PyRep::Integer(belt_id)]).await;
    }

    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    sys_handle.try_update_combat_state(ship_id, |cs| cs.capacitor = 300.0);
    // Position ship 1 km from citadel struct_id (< 150 km) to hit WarpDistanceTooShort
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(1_000_001_000.0, 0.0, 0.0))
        .await;
    let short_res = call_beyonce(
        &ctx,
        &mut sess,
        "CmdWarpToStuff",
        &[PyRep::String("item".to_string()), PyRep::Integer(struct_id), PyRep::Integer(0)],
    )
    .await;
    assert!(short_res.is_err());
    let _ = jita_planet;
}

#[tokio::test]
async fn stargate_jump_errors_concurrent_toctou_and_dock_station_upwell_paths() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id == 30_000_142
                && ctx.sde.jumps.values().any(|rj| rj.from_solar_system_id == j.to_solar_system_id)
        })
        .expect("Jita connected stargate")
        .clone();
    let sys_id = gate.from_solar_system_id;
    let dest_sys_id = gate.to_solar_system_id;
    let char_id = 95_100_030i64;
    let corp_id = 98_500_030i64;
    let ship_id = 10_951_000_030i64;
    let wrong_ship_id = 10_951_000_031i64;
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == dest_sys_id)
        .or_else(|| ctx.sde.stations.values().next())
        .map(|s| s.station_id)
        .unwrap_or(60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "GateJumpPilot".to_string();
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        for sid in [ship_id, wrong_ship_id] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 593,
                    owner_id: char_id,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::new(gate.position.x + 100_000.0, gate.position.y, gate.position.z),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
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

    let mut sess = SessionState::new(74_004);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    assert!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[]).await.is_err());
    assert!(
        call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(999_888_777)])
            .await
            .is_err()
    );
    assert!(
        call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)])
            .await
            .is_err()
    );

    let _ = sys_handle.set_ball_position(ship_id, gate.position).await;
    let mut sess2 = sess.clone();
    let jump_args = [PyRep::Integer(gate.stargate_id)];
    let (r1, r2) = tokio::join!(
        call_beyonce(&ctx, &mut sess, "CmdStargateJump", &jump_args),
        call_beyonce(&ctx, &mut sess2, "CmdStargateJump", &jump_args),
    );
    assert!(r1.is_ok() || r2.is_ok());

    let (st_sys, st_pos) = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|s| (s.solar_system_id, s.position))
        .unwrap_or((dest_sys_id, DVec3::ZERO));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = st_sys;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    let mut dock_sess = SessionState::new(74_005);
    dock_sess.user_id = Some(1);
    dock_sess.character_id = Some(char_id);
    dock_sess.corporation_id = Some(corp_id);
    dock_sess.solar_system_id = Some(st_sys);
    dock_sess.station_id = None;
    dock_sess.ship_id = None; // Exercise db_ship_id fallback
    let st_handle = ctx.systems.get_or_spawn(st_sys);
    st_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::new(st_pos.x + 200_000.0, st_pos.y, st_pos.z),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
    assert!(call_beyonce(&ctx, &mut dock_sess, "CmdDock", &[]).await.is_err());
    assert!(call_beyonce(&ctx, &mut dock_sess, "CmdDock", &[PyRep::Integer(station_id)]).await.is_err());

    let _ = st_handle.set_ball_position(ship_id, st_pos).await;
    let dock_no_ship = call_beyonce(&ctx, &mut dock_sess, "CmdDock", &[PyRep::Integer(station_id)]).await;
    assert!(dock_no_ship.is_err(), "CmdDock without ship_id should fail: {dock_no_ship:?}");
    dock_sess.ship_id = Some(ship_id);
    crate::services::space_destiny::record_session_change_timer(
        &ctx,
        char_id,
        ctx.now_unix(),
        st_sys,
        0,
        "stargate_jump",
    );
    let dock_timer = call_beyonce(&ctx, &mut dock_sess, "CmdDock", &[PyRep::Integer(station_id)]).await;
    assert_eq!(dock_timer.expect_err("active session change timer").code, "SessionChangeInProgress");
    ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);
    let dock_ok = call_beyonce(&ctx, &mut dock_sess, "CmdDock", &[PyRep::Integer(station_id)]).await;
    assert!(dock_ok.is_ok(), "CmdDock failed: {dock_ok:?}");
}

#[tokio::test]
async fn cmd_warp_to_stuff_dispatch_targets_bookmark_anomaly_fleet_and_cap_paths() {
    use eve_db::{
        ActiveMissionRecord, BookmarkRecord, CosmicAnomalyRecord, CosmicSignatureRecord,
        FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
        VirtualServerClock,
    };
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_860_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);

    let sys_id: u32 = 30_000_142;
    let char_id = 95_800_001i64;
    let mate_id = 95_800_002i64;
    let corp_id = 98_800_001i64;
    let ship_id = 1_958_000_010i64;
    let mate_ship_id = 1_958_000_011i64;
    let struct_id = 1_958_000_200i64;
    let rigid_ball_id = 1_958_000_300i64;
    let bm_id = 88_100_001i64;
    let wrong_bm_id = 88_100_002i64;
    let mission_id = 77_100_001u32;

    let station_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .and_then(|s| s.stations.first().map(|st| st.station_id))
        .unwrap_or(60_003_760);
    let gate_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.stargate_id)
        .unwrap_or(50_000_001);
    let belt_cel_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 9)
        .or_else(|| ctx.sde.celestials_in_system(sys_id).into_iter().next())
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_001);

    let big_cap_ship_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.default_attributes.get(&482).copied().unwrap_or(0.0) > 300.0)
        .map(|t| t.type_id)
        .unwrap_or(593);

    ctx.db.with_mut_snapshot(|snap| {
        let base_c = snap.characters.values().next().cloned().expect("demo char");
        let mut c1 = base_c.clone();
        c1.character_id = char_id;
        c1.corporation_id = corp_id;
        c1.solar_system_id = sys_id;
        c1.station_id = None;
        c1.active_ship_id = ship_id;
        snap.characters.insert(char_id, c1);

        let mut c2 = base_c;
        c2.character_id = mate_id;
        c2.corporation_id = corp_id;
        c2.solar_system_id = sys_id;
        c2.station_id = None;
        c2.active_ship_id = mate_ship_id;
        snap.characters.insert(mate_id, c2);

        let mk_ship = |id, owner, tid| InventoryItemRecord {
            item_id: id,
            type_id: tid,
            owner_id: owner,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk_ship(ship_id, char_id, big_cap_ship_type));
        snap.items.insert(mate_ship_id, mk_ship(mate_ship_id, mate_id, 593));

        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "Safe Spot".into(),
                solar_system_id: sys_id,
                x: 80_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            wrong_bm_id,
            BookmarkRecord {
                bookmark_id: wrong_bm_id,
                owner_id: mate_id,
                label: "Other Pilot BM".into(),
                solar_system_id: sys_id,
                x: 50_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "COV-ANOM-001".into(),
            solar_system_id: sys_id,
            name: "Combat Site".into(),
            anomaly_type: "Combat".into(),
            position: [120_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: 1_860_000_000,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "COV-SIG-001".into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole".into(),
            position: [140_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30_000_142),
            destination_system_id: Some(30_000_144),
            created_unix: 1_860_000_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 1_000_000_000.0,
            remaining_mass_kg: 1_000_000_000.0,
            max_jump_mass_kg: 300_000_000.0,
            collapsed: false,
        });
        snap.active_missions.insert(
            (char_id, mission_id),
            ActiveMissionRecord {
                mission_id,
                character_id: char_id,
                agent_id: 3_010_001,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        snap.fleets.insert(
            99_001,
            FleetRecord {
                fleet_id: 99_001,
                boss_id: char_id,
                members: vec![char_id, mate_id],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: true,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details: HashMap::new(),
                invites: Default::default(),
                motd: String::new(),
            },
        );
        snap.system_states.entry(sys_id).or_default().signature_position = Some([160_000_000.0, 0.0, 0.0]);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Warp Target Astrahus".into(),
                position: [180_000_000.0, 0.0, 0.0],
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
    });
    ctx.mark_signature_scanned(char_id, sys_id, 750_000_001);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mk_ball = |id, type_id, pos, mode| Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    };
    let _ = sys_handle.try_spawn_ball_with_combat(
        mk_ball(ship_id, big_cap_ship_type, DVec3::ZERO, MovementMode::Stop),
        CombatState {
            capacitor: 250.0,
            max_capacitor: 250.0,
            ..Default::default()
        },
    );
    let _ = sys_handle.spawn_ball(mk_ball(mate_ship_id, 593, DVec3::new(90_000_000.0, 0.0, 0.0), MovementMode::Stop)).await;
    let _ = sys_handle.spawn_ball(mk_ball(rigid_ball_id, 34_494, DVec3::new(110_000_000.0, 0.0, 0.0), MovementMode::Rigid)).await;

    let mut sess = SessionState::new(95_800);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.fleet_id = Some(99_001);

    // 1. Dispatched SetSessionCheck & invalid target guards
    let _ = call_beyonce(&ctx, &mut sess, "SetSessionCheck", &[PyRep::Bool(false)]).await;
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::Integer(1)]).await.unwrap_err().code, "InvalidWarpDestination");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(0)]).await.unwrap_err().code, "InvalidTarget");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ship_id)]).await.unwrap_err().code, "InvalidTarget");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(f64::NAN)]).await.unwrap_err().code, "InvalidArgument");

    // Zarzakh bookmark ban
    let mut z_sess = sess.clone();
    z_sess.solar_system_id = Some(30_100_000);
    let zh = ctx.systems.get_or_spawn(30_100_000);
    let _ = zh.spawn_ball(mk_ball(ship_id, 593, DVec3::ZERO, MovementMode::Stop)).await;
    assert!(call_beyonce(&ctx, &mut z_sess, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)]).await.is_err());

    // BookmarkNotFound for other pilot's private bookmark
    assert_eq!(
        call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(wrong_bm_id)]).await.unwrap_err().code,
        "BookmarkNotFound" // rpc-lint: allow test fixture
    );

    // Valid warp targets across all stuff types (reset ball position + Stop between warps)
    let targets: Vec<Vec<PyRep>> = vec![
        vec![PyRep::String("bookmark".into()), PyRep::Integer(bm_id), PyRep::Real(0.0)],
        vec![PyRep::String("anomaly".into()), PyRep::String("COV-ANOM-001".into()), PyRep::Real(10_000.0)],
        vec![PyRep::String("anomaly".into()), PyRep::String("COV-SIG-001".into()), PyRep::Real(0.0)],
        vec![PyRep::String("mission".into()), PyRep::Integer(i64::from(mission_id)), PyRep::Real(0.0)],
        vec![PyRep::String("fleet".into()), PyRep::Integer(mate_id), PyRep::Real(5_000.0)],
        vec![PyRep::String("item".into()), PyRep::Integer(station_id), PyRep::Real(0.0)],
        vec![PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)],
        vec![PyRep::String("item".into()), PyRep::Integer(belt_cel_id), PyRep::Real(0.0)],
        vec![PyRep::String("item".into()), PyRep::Integer(struct_id), PyRep::Real(0.0)],
        vec![PyRep::String("item".into()), PyRep::Integer(750_000_001), PyRep::Real(0.0)],
        vec![PyRep::String("item".into()), PyRep::Integer(rigid_ball_id), PyRep::Real(0.0)],
    ];
    for warp_args in targets {
        let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
        let _ = sys_handle.set_ball_position(ship_id, DVec3::new(-500_000_000.0, 0.0, 0.0)).await;
        let _ = sys_handle.set_capacitor(ship_id, 250.0).await;
        let r = call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &warp_args).await;
        assert!(r.is_ok(), "CmdWarpToStuff {warp_args:?} failed: {r:?}");
    }

    // CmdWarpToStuffAutopilot & partial capacitor shortened warp
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(-500_000_000.0, 0.0, 0.0)).await;
    let _ = sys_handle.set_capacitor(ship_id, 50.0).await;
    let _ = call_beyonce(&ctx, &mut sess, "CmdWarpToStuffAutopilot", &[PyRep::Integer(station_id)]).await;

    // WarpDistanceTooShort when next to target
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(80_001_000.0, 0.0, 0.0)).await;
    assert_eq!(
        call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)]).await.unwrap_err().code,
        "WarpDistanceTooShort" // rpc-lint: allow test fixture
    );

    // Kwargs fleet warp path
    let mut kw_fleet = HashMap::new();
    kw_fleet.insert("fleet".into(), PyRep::Bool(true));
    let _ = sys_handle.set_ball_position(ship_id, DVec3::ZERO).await;
    let _ = super::super::handle(&ctx, &mut sess, None, "beyonce", "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)], &kw_fleet).await;
}

#[tokio::test]
async fn stargate_jump_pochven_zarzakh_and_cmd_dock_structure_and_crimewatch_paths() {
    use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, VirtualServerClock};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_870_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);

    let sys_id: u32 = 30_000_142; // Jita high-sec
    let char_id = 96_200_001i64;
    let corp_id = 98_200_001i64;
    let ship_id = 1_962_000_010i64;
    let missing_struct_id = 1_000_962_000_500i64;
    let med_struct_id = 1_962_000_501i64;
    let large_struct_id = 1_962_000_502i64;
    let other_sys_struct_id = 1_962_000_503i64;

    let dread_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19720);
    let titan_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.group_id == 30)
        .map(|t| t.type_id)
        .unwrap_or(671);

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("demo char");
        c.character_id = char_id;
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mk_struct = |id, tid, sid, state: &str, owner| UpwellStructureRecord {
            structure_id: id,
            type_id: tid,
            owner_corp_id: owner,
            solar_system_id: sid,
            name: "Dock Test Structure".into(),
            position: [0.0, 0.0, 0.0],
            state: state.into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        };
        snap.structures.insert(med_struct_id, mk_struct(med_struct_id, 35832, sys_id, "Anchoring", corp_id));
        snap.structures.insert(large_struct_id, mk_struct(large_struct_id, 35833, sys_id, "ShieldVulnerable", corp_id));
        snap.structures.insert(other_sys_struct_id, mk_struct(other_sys_struct_id, 35832, 30_000_144, "ShieldVulnerable", corp_id));
    });

    // 1. Pochven Clade Home standing check (< 6.0 then >= 6.0)
    if let Some(poch_gate) = ctx.sde.jumps.values().find(|j| {
        ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
            s.region_id == 10_000_070 && matches!(s.name.as_str(), "Kino" | "Niarja" | "Archee")
        })
    }) {
        let from_h = ctx.systems.get_or_spawn(poch_gate.from_solar_system_id);
        let _ = from_h.spawn_ball(Ball {
            id: ship_id,
            type_id: 593,
            position: poch_gate.position,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;
        let mut p_sess = SessionState::new(96_201);
        p_sess.user_id = Some(1);
        p_sess.character_id = Some(char_id);
        p_sess.corporation_id = Some(corp_id);
        p_sess.solar_system_id = Some(poch_gate.from_solar_system_id);
        p_sess.ship_id = Some(ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            let c = snap.characters.get_mut(&char_id).unwrap();
            c.solar_system_id = poch_gate.from_solar_system_id;
            c.station_id = None;
        });
        assert!(call_beyonce(&ctx, &mut p_sess, "CmdStargateJump", &[PyRep::Integer(poch_gate.stargate_id)]).await.is_err());
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().standings.insert(500_026, 6.5);
        });
        let _ = call_beyonce(&ctx, &mut p_sess, "CmdStargateJump", &[PyRep::Integer(poch_gate.stargate_id)]).await;
        ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);
    }

    // 2. Zarzakh gate lock check
    let zar_gates: Vec<_> = ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == 30_100_000).take(2).collect();
    if zar_gates.len() == 2 {
        let zh = ctx.systems.get_or_spawn(30_100_000);
        let _ = zh.spawn_ball(Ball {
            id: ship_id,
            type_id: 593,
            position: zar_gates[1].position,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks.insert(char_id, (zar_gates[0].stargate_id, ctx.now_unix() + 3_600));
            let c = snap.characters.get_mut(&char_id).unwrap();
            c.solar_system_id = 30_100_000;
            c.station_id = None;
        });
        let mut z_sess = SessionState::new(96_202);
        z_sess.user_id = Some(1);
        z_sess.character_id = Some(char_id);
        z_sess.solar_system_id = Some(30_100_000);
        z_sess.ship_id = Some(ship_id);
        assert_eq!(
            call_beyonce(&ctx, &mut z_sess, "CmdStargateJump", &[PyRep::Integer(zar_gates[1].stargate_id)]).await.unwrap_err().code,
            "ZarzakhGateLocked"
        );
    }

    // 3. CmdDock comprehensive error and Upwell structure paths
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.characters.get_mut(&char_id).unwrap();
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.structure_id = None;
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut sess = SessionState::new(96_203);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // ShipNotInSpace before spawning ball
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "ShipNotInSpace");
    // InvalidShipId when second arg doesn't match
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id), PyRep::Integer(ship_id + 1)]).await.unwrap_err().code, "InvalidShipId");

    let _ = sys_h.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );

    // Crimewatch WeaponsTimerActive & CantDockGlobalCriminalFlag
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.states.entry(char_id).or_default();
        st.weapon_timer_until_unix = ctx.now_unix() + 60;
    }
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "WeaponsTimerActive");
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.states.entry(char_id).or_default();
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = ctx.now_unix() + 900;
    }
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "CantDockGlobalCriminalFlag");
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.states.entry(char_id).or_default().criminal_flag_until_unix = 0;
    }

    // Structure checks: missing, wrong sys, anchoring, destroyed, ACL denied, ship too large, warp scrambled, success
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(missing_struct_id)]).await.unwrap_err().code, "StructureNotFound");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(other_sys_struct_id)]).await.unwrap_err().code, "DockTargetWrongSystem");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "StructureNotDockable");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&med_struct_id).unwrap().state = "Destroyed".into());
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "StructureDestroyed");
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.structures.get_mut(&med_struct_id).unwrap();
        s.state = "ShieldVulnerable".into();
        s.owner_corp_id = corp_id + 999; // denies ACL
    });
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "StructureDockingAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&med_struct_id).unwrap().owner_corp_id = corp_id;
        snap.items.get_mut(&ship_id).unwrap().type_id = dread_type;
    });
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(med_struct_id)]).await.unwrap_err().code, "ShipTooLargeForStructure");
    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&ship_id).unwrap().type_id = titan_type);
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(large_struct_id)]).await.unwrap_err().code, "ShipTooLargeForStructure");
    ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&ship_id).unwrap().type_id = 593);

    let _ = sys_h.set_warp_scramble_strength(ship_id, 2).await;
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(large_struct_id)]).await.unwrap_err().code, "ShipWarpScrambled");
    let _ = sys_h.set_warp_scramble_strength(ship_id, 0).await;

    let ok_struct_dock = call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(large_struct_id)]).await;
    assert!(ok_struct_dock.is_ok(), "structure dock failed: {ok_struct_dock:?}");
    assert_eq!(sess.structure_id, Some(large_struct_id));
}

#[tokio::test]
async fn warp_stargate_and_dock_full_error_guard_zarzakh_and_outlaw_matrix() {
    use eve_db::{
        ActiveMissionRecord, BookmarkRecord, CosmicAnomalyRecord, FleetRecord, InventoryFlag,
        InventoryItemRecord, ServerClock, VirtualServerClock,
    };
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_880_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    let sys_id: u32 = 30_000_142;
    let other_sys_id: u32 = 30_000_144;
    let char_id = 96_500_001i64;
    let mate_id = 96_500_002i64;
    let corp_id = 98_500_001i64;
    let ship_id = 1_965_000_010i64;
    let mate_ship_id = 1_965_000_011i64;
    let bm_wrong_sys = 700_500_001i64;
    let jita_station = 60_003_760i64;
    let amarr_station = 60_008_494i64;

    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .expect("Jita stargate")
        .clone();
    let other_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == other_sys_id)
        .expect("Other stargate")
        .clone();

    ctx.db.with_mut_snapshot(|snap| {
        let base_c = snap.characters.values().next().cloned().expect("demo char");
        let mut c1 = base_c.clone();
        c1.character_id = char_id;
        c1.corporation_id = corp_id;
        c1.solar_system_id = sys_id;
        c1.station_id = None;
        c1.active_ship_id = ship_id;
        snap.characters.insert(char_id, c1);

        let mut c2 = base_c;
        c2.character_id = mate_id;
        c2.corporation_id = corp_id;
        c2.solar_system_id = sys_id;
        c2.station_id = None;
        c2.active_ship_id = mate_ship_id;
        snap.characters.insert(mate_id, c2);

        let mk_ship = |id, owner| InventoryItemRecord {
            item_id: id,
            type_id: 593,
            owner_id: owner,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk_ship(ship_id, char_id));
        snap.items.insert(mate_ship_id, mk_ship(mate_ship_id, mate_id));

        snap.bookmarks.insert(
            bm_wrong_sys,
            BookmarkRecord {
                bookmark_id: bm_wrong_sys,
                owner_id: char_id,
                label: "Remote Safe".into(),
                solar_system_id: other_sys_id,
                x: 100_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "COV-CLEARED-001".into(),
            solar_system_id: sys_id,
            name: "Cleared Site".into(),
            anomaly_type: "Combat".into(),
            position: [120_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: 1_860_000_000,
            lifetime_seconds: 86_400,
            cleared: true,
        });
        snap.active_missions.insert(
            (char_id, 55_900),
            ActiveMissionRecord {
                mission_id: 55_900,
                character_id: char_id,
                agent_id: 3_010_001,
                solar_system_id: other_sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 1,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        snap.fleets.insert(
            99_501,
            FleetRecord {
                fleet_id: 99_501,
                boss_id: char_id,
                members: vec![char_id, mate_id],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: true,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details: HashMap::new(),
                invites: Default::default(),
                motd: String::new(),
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut sess = SessionState::new(96_500);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.fleet_id = Some(99_501);

    // 1. Pre-spawn ShipNotInSpace checks on Warp & StargateJump + CmdDock MissingLocationId / NoSolarSystem / AlreadyDocked
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(jita_station)]).await.unwrap_err().code, "ShipNotInSpace");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "ShipNotInSpace");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[]).await.unwrap_err().code, "MissingLocationId");

    sess.solar_system_id = None;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = 0);
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(jita_station)]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id);

    sess.station_id = Some(jita_station);
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "ShipDocked");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(jita_station)]).await.unwrap_err().code, "AlreadyDocked");
    sess.station_id = None;

    sess.ship_id = None;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().active_ship_id = 0);
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "NoActiveShip");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(jita_station)]).await.unwrap_err().code, "NoActiveShip");
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id);

    // Spawn ship & nearby fleetmate (within 150 km for fleet grid warp)
    let _ = sys_h.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::new(-500_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState { capacitor: 250.0, max_capacitor: 250.0, ..Default::default() },
    );
    let _ = sys_h.try_spawn_ball_with_combat(
        Ball {
            id: mate_ship_id,
            type_id: 593,
            position: DVec3::new(-499_999_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState { capacitor: 250.0, max_capacitor: 250.0, ..Default::default() },
    );

    // 2. CmdWarpToStuff negative targets + clamping + 0 cap + active fleet grid warp
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_wrong_sys)]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("anomaly".into()), PyRep::String("COV-CLEARED-001".into())]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("anomaly".into()), PyRep::String("MISSING-ANOM".into())]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(55_900)]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("fleet".into()), PyRep::Integer(char_id)]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("fleet".into()), PyRep::Integer(99_999_777)]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("char".into()), PyRep::Integer(99_999_777)]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(999_888_777)]).await.is_err());
    assert!(call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("unknownKind".into()), PyRep::Integer(999_888_777)]).await.is_err());

    let _ = sys_h.set_capacitor(ship_id, 0.0).await;
    let zero_cap_res = call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(jita_station)]).await;
    assert_eq!(zero_cap_res.unwrap_err().code, "NotEnoughCapacitor");
    let _ = sys_h.set_capacitor(ship_id, 250.0).await;

    let mut kw_fleet = HashMap::new();
    kw_fleet.insert("fleet".into(), PyRep::Bool(true));
    kw_fleet.insert("minRange".into(), PyRep::Real(500_000.0));
    let _ = super::super::handle(&ctx, &mut sess, None, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(jita_station)], &kw_fleet).await;

    // Warp while already in warp + Stop
    let _ = call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(jita_gate.stargate_id)]).await;
    let _ = sys_h.set_movement_mode(ship_id, MovementMode::Stop).await;

    // 3. CmdStargateJump guards + clean entry into Zarzakh + ZarzakhGateLocked + ShipDocked + NoActiveShip
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(999_999_888)]).await.unwrap_err().code, "UnknownStargate");
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(other_gate.stargate_id)]).await.unwrap_err().code, "StargateWrongSystem");

    // PVE-ANOM-* prefix seed check in CmdWarpToStuff (L231-239)
    let _ = call_beyonce(&ctx, &mut sess, "CmdWarpToStuff", &[PyRep::String("other".into()), PyRep::String("PVE-ANOM-001".into())]).await;

    // SessionChangeInProgress on CmdStargateJump (must run while still in Jita before jumping!)
    let _ = sys_h.set_ball_position(ship_id, jita_gate.position).await;
    ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).insert(
        char_id,
        crate::services::space_destiny::SessionChangeTimerState {
            expires_unix: ctx.now_unix() + 20,
            system_id: sys_id,
            started_tick: 0,
            reason: "test".to_string(),
            location_change_invul_until: 0,
        },
    );
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "SessionChangeInProgress");
    ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);

    // Jump through Jita gate, then jump BACK through destination gate -> restores Jita cleanly
    let _ = sys_h.set_warp_scramble_strength(ship_id, 2).await;
    let _ = call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await;
    let _ = sys_h.set_warp_scramble_strength(ship_id, 0).await;
    ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);

    // Jump into Zarzakh if SDE has incoming gate, then verify ZarzakhGateLocked on another Zarzakh exit gate
    let zar_sys = 30_100_000u32;
    let zar_gates: Vec<_> = ctx.sde.jumps.values().filter(|j| j.to_solar_system_id == zar_sys).cloned().collect();
    if let Some(to_zar) = zar_gates.first() {
        let fh = ctx.systems.get_or_spawn(to_zar.from_solar_system_id);
        let _ = fh.spawn_ball(Ball {
            id: ship_id,
            type_id: 593,
            position: to_zar.position,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;
        ctx.db.with_mut_snapshot(|snap| {
            let c = snap.characters.get_mut(&char_id).unwrap();
            c.solar_system_id = to_zar.from_solar_system_id;
            c.station_id = None;
        });
        let mut z_sess = sess.clone();
        z_sess.solar_system_id = Some(to_zar.from_solar_system_id);
        let z_ok = call_beyonce(&ctx, &mut z_sess, "CmdStargateJump", &[PyRep::Integer(to_zar.stargate_id)]).await;
        assert!(z_ok.is_ok(), "entry into Zarzakh failed: {z_ok:?}");
        ctx.session_change_timers.lock().unwrap_or_else(|e| e.into_inner()).remove(&char_id);

        if let Some(other_exit) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == zar_sys && j.stargate_id != to_zar.destination_stargate_id) {
            let zh = ctx.systems.get_or_spawn(zar_sys);
            let _ = zh.set_ball_position(ship_id, other_exit.position).await;
            let locked_err = call_beyonce(&ctx, &mut z_sess, "CmdStargateJump", &[PyRep::Integer(other_exit.stargate_id)]).await;
            assert_eq!(locked_err.unwrap_err().code, "ZarzakhGateLocked");
        }
    }

    // 4. Restore sess+DB to Jita undocked, test CmdDock wrong-system, Outlaw highsec refusal, and db_ship_id fallback (L1128-1139)
    let _ = sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 593,
        position: jita_gate.position,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.characters.get_mut(&char_id).unwrap();
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        c.security_status = 0.0;
    });
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(amarr_station)]).await.unwrap_err().code, "DockTargetWrongSystem");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().security_status = -5.5);
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(jita_station)]).await.unwrap_err().code, "FactionPoliceRefusedDocking");

    // db_ship_id fallback in CmdDock (sess.ship_id != active_ship_id and missing from space)
    sess.ship_id = Some(999_111_222);
    let _ = call_beyonce(&ctx, &mut sess, "CmdDock", &[PyRep::Integer(jita_station)]).await;
    sess.ship_id = Some(ship_id);

    // ShipDocked & NoActiveShip on CmdStargateJump
    let mut docked_sess = sess.clone();
    docked_sess.station_id = Some(jita_station);
    assert_eq!(call_beyonce(&ctx, &mut docked_sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "ShipDocked");
    let mut noship_sess = sess.clone();
    noship_sess.ship_id = None;
    assert_eq!(call_beyonce(&ctx, &mut noship_sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "NoActiveShip");

    // 5. CmdStargateJump ShipNotInSpace + DB/session desync (`entry_db_undocked == false` L884-922) + CharacterNotFound
    let _ = sys_h.remove_ball(ship_id).await;
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "ShipNotInSpace");
    let _ = sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 593,
        position: jita_gate.position,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    let saved_char = ctx.db.with_mut_snapshot(|snap| snap.characters.remove(&char_id).unwrap());
    assert_eq!(call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "CharacterNotFound");
    ctx.db.with_mut_snapshot(|snap| {
        let mut c = saved_char.clone();
        c.solar_system_id = sys_id;
        c.station_id = Some(jita_station);
        c.active_ship_id = mate_ship_id;
        c.security_status = 0.0;
        snap.characters.insert(char_id, c);
    });
    let _ = call_beyonce(&ctx, &mut sess, "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await;
}


