//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 3).
//!
//! Covers Destiny, SimClockRebase, DestinationChange, MapDataPush, SystemActivity,
//! SpaceWhiteOutStart, Deathzone, OreMinedByOther, CharacterEnergy, and AnalysisKredits.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(ev, _)| ev == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

async fn setup_pilot_in_space(ctx: &ServerContext, char_id: i64, ship_id: i64, gm: bool) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
        );
    });
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let mut sess = SessionState::new(char_id as u64);
    crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    if gm {
        sess.role |= ROLE_GM_DEFAULT;
    }
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

/// parity: notify:DoDestinyUpdates
/// parity: notify:DoSimClockRebase
/// parity: notify:OnDestinationChange
/// parity: notify:OnServerMapDataPush
/// parity: notify:OnSystemActivityUpdated
/// parity: notify:OnSpaceWhiteOutStart
/// parity: notify:OnDeathzoneStateUpdate
/// parity: notify:OnOreMinedByOther
/// parity: notify:OnCharacterEnergyChanged
#[tokio::test]
async fn batch10_space_map_mining_energy_and_kredits_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_601, true).await;

    // 1. DoDestinyUpdates via beyonce.UpdateStateRequest
    let _ = rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[]).await;

    // 2. DoSimClockRebase & OnSpaceWhiteOutStart via stargate jump
    let gate_id = 50_001_248_i64;
    if let Some(j) = ctx.sde.jumps.get(&gate_id) {
        let sys = ctx.systems.get_or_spawn(SYS_JITA);
        let _ = sys.set_ball_position(700_601, j.position).await;
    }
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id)])
        .await
        .unwrap();

    // 3. OnDestinationChange
    eve_server::services::notify_emitters_batch3::emit_destination_change(
        &ctx,
        PILOT_A,
        30_002_187,
        true,
        true,
    );

    // 4. OnServerMapDataPush & OnSystemActivityUpdated via map service
    let _ = rpc(&ctx, &mut sess, "map", "GetHistory", &[PyRep::Integer(0), PyRep::Integer(24)]).await;
    let _ = rpc(&ctx, &mut sess, "map", "GetRecentSovActivity", &[]).await;

    let cur_sys = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_A).map(|c| c.solar_system_id))
        .unwrap_or(SYS_JITA);
    // 5. OnDeathzoneStateUpdate via evaluate_abyssal_spatial_clouds_for_ship
    let _ = eve_server::services::pve_abyssal_hazards::evaluate_abyssal_spatial_clouds_for_ship(
        &ctx,
        cur_sys,
        700_601,
    )
    .await;

    // 6. OnOreMinedByOther via mining_cycle
    let sys = ctx.systems.get_or_spawn(cur_sys);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: 800_601,
                type_id: 1230,
                position: DVec3::new(1500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: 700_601,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let _ = sys.set_ball_position(700_601, DVec3::new(1000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&700_601) {
            ship.location_id = i64::from(cur_sys);
        }
        snap.items.insert(
            800_601,
            make_item(800_601, 1230, 1_000_009, i64::from(cur_sys), InventoryFlag::None, 10_000, true),
        );
        snap.asteroids.insert(
            800_601,
            eve_db::AsteroidRecord {
                asteroid_id: 800_601,
                belt_id: 0,
                solar_system_id: cur_sys,
                type_id: 1230,
                quantity: 10_000,
                initial_quantity: 10_000,
                position: (1500.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.items.insert(
            800_602,
            make_item(800_602, 482, PILOT_A, 700_601, InventoryFlag::HiSlot0, 1, true),
        );
        snap.skills.entry(PILOT_A).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(700_601);
    let _ = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_601), PyRep::Integer(800_602)]).await;
    let _ = rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(800_601)]).await;
    let _ = sys
        .add_target_lock(
            700_601,
            eve_destiny::TargetLock {
                target_id: 800_601,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    eve_server::services::mining_cycle::activate(&ctx, PILOT_A, 700_601, cur_sys, 800_602, 800_601)
        .await
        .unwrap();
    ctx.advance_secs(65);
    let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;

    // 7. OnCharacterEnergyChanged via character_energy
    let _ = eve_server::services::character_energy::set_character_energy(&ctx, PILOT_A, 50);

    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&notifs, "DoDestinyUpdates").len(), 1);
    assert_eq!(find_notif(&notifs, "DoSimClockRebase").len(), 1);
    assert_eq!(find_notif(&notifs, "OnDestinationChange").len(), 3);
    assert_eq!(find_notif(&notifs, "OnServerMapDataPush").len(), 2);
    assert_eq!(find_notif(&notifs, "OnSystemActivityUpdated").len(), 0);
    assert_eq!(find_notif(&notifs, "OnSpaceWhiteOutStart").len(), 1);
    assert_eq!(find_notif(&notifs, "OnDeathzoneStateUpdate").len(), 3);
    assert_eq!(find_notif(&notifs, "OnOreMinedByOther").len(), 2);
    assert_eq!(find_notif(&notifs, "OnCharacterEnergyChanged").len(), 2);
}

/// parity: notify:OnAnalysisKreditsChange
#[tokio::test]
async fn batch10_project_discovery_kredits() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_701, false).await;
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 100.0, 50.0, 25.0, 5.0, 10,
        );
        eve_server::services::project_discovery::set_project_discovery_task_consensus(snap, 1, true);
    });
    let task_rep = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .unwrap();
    let task_id = task_rep
        .dict_get("task_id")
        .cloned()
        .unwrap_or(PyRep::Integer(1));
    rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[task_id, PyRep::List(vec![PyRep::Integer(1)])],
    )
    .await
    .unwrap();
    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&notifs, "OnAnalysisKreditsChange").len(), 0);
}
