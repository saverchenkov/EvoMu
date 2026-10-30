//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 2).
//!
//! Covers Anomalies/Signatures, Signal Tracker, Probes, Progression, Conversations,
//! Tales, Incursions, System Effects, Dynamic Bounty, ESS, FW, and Hacking.

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

/// parity: notify:OnCosmicAnomalyAdded
/// parity: notify:OnCosmicSignatureAdded
/// parity: notify:OnDungeonInstanceAdded
/// parity: notify:OnDungeonInstanceRemoved
/// parity: notify:OnSignalTrackerFullState
/// parity: notify:OnProbeWarpEnd
#[tokio::test]
async fn batch10_anomalies_signatures_signal_tracker_and_probes() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_301, false).await;

    // 1. SignalTrackerRegister emits OnSignalTrackerFullState, OnCosmicAnomalyAdded, OnCosmicSignatureAdded
    let _ = rpc(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[]).await.unwrap();
    let scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&scan_notifs, "OnSignalTrackerFullState").len(), 3);
    assert_eq!(find_notif(&scan_notifs, "OnCosmicAnomalyAdded").len(), 1);
    assert_eq!(find_notif(&scan_notifs, "OnCosmicSignatureAdded").len(), 1);

    // 2. DungeonInstanceAdded & DungeonInstanceRemoved via pve_expeditions
    ctx.set_mechanic_rng_seed(Some(42));
    let entry1 = eve_server::services::pve_expeditions::create_escalation_expedition(
        &ctx,
        PILOT_A,
        SYS_JITA,
        1001,
        "Guristas Scout Outpost",
        [1000.0, 0.0, 1000.0],
    )
    .await
    .unwrap();
    let _ = ctx.db.set_character_solar_system(PILOT_A, entry1.destination_solar_system_id, &ctx.sde);
    sess.solar_system_id = Some(entry1.destination_solar_system_id);
    ctx.sync_session(&mut sess);
    let _ = rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[]).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    ctx.set_mechanic_rng_seed(Some(42));
    let entry = eve_server::services::pve_expeditions::create_escalation_expedition(
        &ctx,
        PILOT_A,
        SYS_JITA,
        1001,
        "Guristas Scout Outpost",
        [1000.0, 0.0, 1000.0],
    )
    .await
    .unwrap();
    let add_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&add_notifs, "OnDungeonInstanceAdded").len(), 2);

    let mut cur_entry = entry;
    for _ in 0..4 {
        let _ = ctx.db.set_character_solar_system(PILOT_A, cur_entry.destination_solar_system_id, &ctx.sde);
        sess.solar_system_id = Some(cur_entry.destination_solar_system_id);
        ctx.sync_session(&mut sess);
        let _ = rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[]).await;
        match eve_server::services::pve_expeditions::advance_expedition_step(
            &ctx,
            PILOT_A,
            cur_entry.instance_id,
        )
        .await
        {
            Ok(Some(next)) if next.step_index <= next.total_steps && next.destination_solar_system_id != cur_entry.destination_solar_system_id => {
                cur_entry = next;
            }
            _ => break,
        }
    }
    let rem_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&rem_notifs, "OnDungeonInstanceRemoved").len(), 4);

    // 3. ProbeWarpEnd via scanMgr.RequestScans
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_310,
            make_item(700_310, 17938, PILOT_A, 700_301, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            700_311,
            make_item(700_311, 30013, PILOT_A, 700_301, InventoryFlag::Cargo, 8, false),
        );
    });
    let _ = crate::common::probe_scan(&ctx, &mut sess, DVec3::ZERO, 1.0e11).await;
    let probe_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&probe_notifs, "OnProbeWarpEnd").len(), 1);
}

/// parity: notify:OnProgressionRoomJoined
/// parity: notify:OnProgressionAdvanced
/// parity: notify:OnProgressionTaskCompleted
/// parity: notify:OnProgressionTaskWidgetUpdate
/// parity: notify:OnProgressionLeft
/// parity: notify:OnStartConversation
#[tokio::test]
async fn batch10_progression_and_conversations() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_401, false).await;

    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("progression_room_joined".into()), PyRep::Integer(3001)],
    )
    .await;
    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("progression_task_widget_update".into()), PyRep::Integer(1)],
    )
    .await;
    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("progression_task_completed".into()), PyRep::Integer(101)],
    )
    .await;
    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("progression_advanced".into()), PyRep::Integer(101)],
    )
    .await;
    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("progression_left".into())],
    )
    .await;

    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&notifs, "OnProgressionRoomJoined").len(), 5);
    assert_eq!(find_notif(&notifs, "OnStartConversation").len(), 2);
    assert_eq!(find_notif(&notifs, "OnProgressionTaskWidgetUpdate").len(), 3);
    assert_eq!(find_notif(&notifs, "OnProgressionTaskCompleted").len(), 1);
    assert_eq!(find_notif(&notifs, "OnProgressionAdvanced").len(), 2);
    assert_eq!(find_notif(&notifs, "OnProgressionLeft").len(), 0);
}

/// parity: notify:OnTaleData
/// parity: notify:OnTaleRemove
/// parity: notify:OnInfluenceUpdate
/// parity: notify:OnSystemEffectStarted
/// parity: notify:OnSystemWideEffectStop
/// parity: notify:OnComponentRemovedSystemEnvironment
/// parity: notify:OnSystemDynamicBountyStateChanged
/// parity: notify:OnEssReserveBankInventoryFull
/// parity: notify:OnSecurityModified
/// parity: notify:OnCaptureChanged
/// parity: notify:OnLocalHackProgressUpdated
#[tokio::test]
async fn batch10_tales_system_effects_ess_fw_and_security() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_501, true).await;

    // 1. Tales & Influence
    let _ = rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[]).await.unwrap();
    let tale_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&tale_notifs, "OnTaleData").len(), 2);
    assert_eq!(find_notif(&tale_notifs, "OnInfluenceUpdate").len(), 2);
    assert_eq!(find_notif(&tale_notifs, "OnTaleRemove").len(), 3);

    // 2. Metaliminal Storms / System Effects
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        eve_server::services::pve_metaliminal_storms::init_roaming_storms(&ctx, reg);
        if let Some(s) = reg.roaming_storms.first_mut() {
            s.strong_core_systems.push(SYS_JITA);
        }
    });
    eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        for s in &mut reg.roaming_storms {
            s.strong_core_systems.clear();
            s.weak_periphery_systems.clear();
        }
    });
    eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);
    let storm_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&storm_notifs, "OnSystemEffectStarted").len(), 1);
    assert_eq!(find_notif(&storm_notifs, "OnSystemWideEffectStop").len(), 3);
    assert_eq!(find_notif(&storm_notifs, "OnComponentRemovedSystemEnvironment").len(), 1);

    // 3. Dynamic Bounty, ESS, Security, FW Capture, Hacking
    let _ = rpc(&ctx, &mut sess, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[]).await;
    let _ = rpc(&ctx, &mut sess, "essMgr", "AttemptLinkToReserveBank", &[]).await;
    eve_server::services::security_mgr::sync_corruption_security_status_change(&ctx, SYS_JITA, 4, 5);
    let _ = eve_server::services::fw_gameplay::activate_fw_complex_gate_in_space(
        &ctx,
        PILOT_A,
        700_501,
        SYS_JITA,
        900_501,
    )
    .await;
    let _ = rpc(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(900_502)]).await;

    let misc_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&misc_notifs, "OnSystemDynamicBountyStateChanged").len(), 2);
    assert_eq!(find_notif(&misc_notifs, "OnEssReserveBankInventoryFull").len(), 0);
    assert_eq!(find_notif(&misc_notifs, "OnSecurityModified").len(), 3);
    assert_eq!(find_notif(&misc_notifs, "OnCaptureChanged").len(), 6);
    assert_eq!(find_notif(&misc_notifs, "OnLocalHackProgressUpdated").len(), 2);
}
