//! Real production server-to-client `notify:On*` emission integration tests (Batch 2).
//
// Exercises 32 V24.01 server-to-client notifications end-to-end through `ctx.dispatch_rpc`,
// asserting exact V24 positional/dict payload arguments and persisted world-state transitions.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::season_manager::SeasonalChallengeDef;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const EXPERT_SYSTEM_TYPE_ID: u32 = 57_203;
const FILAMENT_TYPE_ID: u32 = 56_134; // Tranquil Firestorm Filament (T0, group 1979)

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

#[allow(clippy::too_many_arguments)]
async fn login_in_space(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut ship = make_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        ship.dynamic_attributes.insert(48, 25_000.0);
        ship.dynamic_attributes.insert(11, 25_000.0);
        snap.items.insert(ship_id, ship);
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.capacitor = 50_000.0;
    cs.max_capacitor = 50_000.0;
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}


#[path = "notify_events/abyssal_operations_and_node_graph_notices.rs"]
mod abyssal_operations_and_node_graph_notices;
#[path = "notify_events/clock_kredits_bans_and_capture_notices.rs"]
mod clock_kredits_bans_and_capture_notices;
#[path = "notify_events/combat_and_missile_message_notices.rs"]
mod combat_and_missile_message_notices;
#[path = "notify_events/crimewatch_insurgency_and_tale_notices.rs"]
mod crimewatch_insurgency_and_tale_notices;
#[path = "notify_events/criminal_timer_and_fw_capture_notices.rs"]
mod criminal_timer_and_fw_capture_notices;
#[path = "notify_events/discovery_campaigns_and_cluster_notices.rs"]
mod discovery_campaigns_and_cluster_notices;
#[path = "notify_events/dungeon_escalation_and_fw_bunker_notices.rs"]
mod dungeon_escalation_and_fw_bunker_notices;
#[path = "notify_events/dungeon_progression_notices.rs"]
mod dungeon_progression_notices;
#[path = "notify_events/dungeon_triggers_and_decloak_notices.rs"]
mod dungeon_triggers_and_decloak_notices;
#[path = "notify_events/fighters_scanning_abyssal_and_seasons_notices.rs"]
mod fighters_scanning_abyssal_and_seasons_notices;
#[path = "notify_events/fw_insurgency_and_safelogoff_notices.rs"]
mod fw_insurgency_and_safelogoff_notices;
#[path = "notify_events/gm_admin_and_attribute_notices.rs"]
mod gm_admin_and_attribute_notices;
#[path = "notify_events/operation_replay_and_container_password_notices.rs"]
mod operation_replay_and_container_password_notices;
#[path = "notify_events/raffle_and_operation_notices.rs"]
mod raffle_and_operation_notices;
#[path = "notify_events/seasons_plex_and_dogma_notices.rs"]
mod seasons_plex_and_dogma_notices;
#[path = "notify_events/session_localization_and_load_rpcs.rs"]
mod session_localization_and_load_rpcs;
#[path = "notify_events/sim_clock_disapproval_and_cache_notices.rs"]
mod sim_clock_disapproval_and_cache_notices;
#[path = "notify_events/station_arena_and_space_notices.rs"]
mod station_arena_and_space_notices;
#[path = "notify_events/station_pod_and_ess_notices.rs"]
mod station_pod_and_ess_notices;
#[path = "notify_events/target_lock_warzone_and_crimewatch_notices.rs"]
mod target_lock_warzone_and_crimewatch_notices;
