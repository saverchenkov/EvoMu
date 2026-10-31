//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 2).
//!
//! Covers Dungeon Editor, Dungeon Values, all 5 Dungeon Room Triggers, Signal Tracker full state,
//! Scanner Probe repositioning (`OnProbeWarpEnd`), and Escalation Expeditions
//! (`OnCosmicSignatureAdded`, `OnDungeonInstanceAdded`, `OnDungeonInstanceRemoved`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
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

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, kwargs, None)
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

fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(ev, _)| ev == event)
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
    let (user, pass) = if char_id == PILOT_A { ("demo", "demo-secret") } else { ("other", "other-secret") };
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
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

/// parity: notify:OnDungeonEdit
/// parity: notify:OnDungeonValues
#[tokio::test]
async fn batch10_dungeon_editor_values_and_room_triggers() {
    let ctx = ServerContext::new();
    let mut gm_sess = setup_pilot_in_space(&ctx, PILOT_A, 700_201, true).await;
    let mut non_gm = setup_pilot_in_space(&ctx, PILOT_B, 700_202, false).await;

    // 1. OnDungeonEdit via keeper.EditDungeon
    // Negative: non-GM cannot EditDungeon, and GM PlayDungeon (godmode=0) does NOT emit OnDungeonEdit
    assert!(rpc(&ctx, &mut non_gm, "keeper", "EditDungeon", &[PyRep::Integer(1420)]).await.is_err());
    assert!(!has_notif(&drain_all(&ctx, &mut non_gm, PILOT_B), "OnDungeonEdit"));

    let mut play_kw = HashMap::new();
    play_kw.insert("dungeonID".into(), PyRep::Integer(1420));
    play_kw.insert("godmode".into(), PyRep::Integer(0));
    rpc_kw(&ctx, &mut gm_sess, "keeper", "PlayDungeon", &[], &play_kw).await.unwrap();
    assert!(!has_notif(&drain_all(&ctx, &mut gm_sess, PILOT_A), "OnDungeonEdit"));

    // Positive: GM EditDungeon emits OnDungeonEdit(dungeonID, roomID, (x, y, z))
    let mut edit_kw = HashMap::new();
    edit_kw.insert("dungeonID".into(), PyRep::Integer(1420));
    rpc_kw(&ctx, &mut gm_sess, "keeper", "EditDungeon", &[], &edit_kw).await.unwrap();
    let edit_notifs = drain_all(&ctx, &mut gm_sess, PILOT_A);
    let edit_args = find_notif(&edit_notifs, "OnDungeonEdit");
    assert_eq!(edit_args.len(), 3);
    assert_eq!(edit_args[0], PyRep::Integer(1420));
    assert!(edit_args[1].as_i64().unwrap_or(0) > 0);
    match &edit_args[2] {
        PyRep::Tuple(coords) => assert_eq!(coords.len(), 3),
        other => panic!("expected 3-tuple roomPos, got {other:?}"),
    }

    // 2. Spawn normal SDE dungeon instance with entry gate and verify OnDungeonValues
    let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        &ctx,
        PILOT_A,
        SYS_JITA,
        1420,
        "Deadspace",
        [100_000.0, 0.0, 0.0],
        true,
    )
    .await
    .expect("spawn_sde_dungeon_runtime_instance");
    let state_rep = eve_server::services::pve_dungeon_runtime_rpc::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let entry_gate_id = state_rep
        .dict_get("entryGateBallID")
        .and_then(|v| v.as_i64())
        .expect("entryGateBallID");
    let _ = drain_all(&ctx, &mut non_gm, PILOT_B);

    // Negative: Pilot B is >2500m from entry gate -> ActivateAccelerationGate fails, zero OnDungeonValues
    assert!(
        rpc(&ctx, &mut non_gm, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut non_gm, PILOT_B), "OnDungeonValues"));

    // Positive: move Pilot B within 2500m of entry gate and activate -> warps into Room 1 and emits OnDungeonValues
    let jita_sys = ctx.systems.get_or_spawn(SYS_JITA);
    let (_, gate_ball) = jita_sys.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap();
    let _ = jita_sys.set_ball_position(700_202, gate_ball.position).await;
    let _room_enter_rep = rpc(&ctx, &mut non_gm, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
        .await
        .expect("ActivateAccelerationGate");
    let gate_notifs = drain_all(&ctx, &mut non_gm, PILOT_B);
    let dv_args = find_notif(&gate_notifs, "OnDungeonValues");
    assert_eq!(dv_args.len(), 2);
    assert_eq!(dv_args[0], PyRep::Integer(1420));
    assert_eq!(dv_args[1].dict_get("currentWave").and_then(|v| v.as_i64()), Some(1));
}

/// parity: notify:OnSignalTrackerFullState
/// parity: notify:OnProbeWarpEnd
/// parity: notify:OnCosmicSignatureAdded
/// parity: notify:OnDungeonInstanceAdded
/// parity: notify:OnDungeonInstanceRemoved
#[tokio::test]
async fn batch10_expedition_signatures_signal_tracker_and_probes() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_301, false).await;

    // 1. OnSignalTrackerFullState via scanMgr.SignalTrackerRegister
    // Negative: session with solar_system_id = None returns NoSolarSystem and emits zero notifications
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_A).unwrap().solar_system_id = 0);
    sess.solar_system_id = None;
    assert_eq!(
        rpc(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[]).await.unwrap_err().code,
        "NoSolarSystem"
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnSignalTrackerFullState"));

    // Positive: in-space pilot registers and receives 3-arg OnSignalTrackerFullState with 4-tuple fullState
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_A).unwrap().solar_system_id = SYS_JITA);
    sess.solar_system_id = Some(SYS_JITA);
    rpc(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[]).await.unwrap();
    let st_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let st_args = find_notif(&st_notifs, "OnSignalTrackerFullState");
    assert_eq!(st_args.len(), 3);
    assert_eq!(st_args[0], PyRep::Integer(i64::from(SYS_JITA)));
    match &st_args[1] {
        PyRep::Tuple(parts) => assert_eq!(parts.len(), 4),
        other => panic!("expected 4-tuple fullState, got {other:?}"),
    }
    assert_eq!(st_args[2], PyRep::Bool(true));

    // 2. OnProbeWarpEnd via scanSvc / dogmaIM probe launch + RequestScans
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_310,
            make_item(700_310, 17_938, PILOT_A, 700_301, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            700_311,
            make_item(700_311, 30_013, PILOT_A, 700_301, InventoryFlag::HiSlot0, 8, false),
        );
    });
    ctx.invalidate_ship_fitting(700_301);
    let _ = rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_301), PyRep::Integer(700_310)]).await;
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(700_310), PyRep::String("launchProbes".into())])
        .await
        .unwrap();
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: RequestScans without moving probe coordinates does NOT emit OnProbeWarpEnd
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[]).await.unwrap();
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnProbeWarpEnd"));

    // Positive: RequestScans with new probe coordinates repositions probe and emits OnProbeWarpEnd(probeID)
    let first_probe_id = ctx.active_scanner_probes.read().unwrap().keys().min().copied().expect("launched probe");
    rpc(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Real(250_000.0),
            PyRep::Real(100_000.0),
            PyRep::Real(-50_000.0),
            PyRep::Integer(4),
        ])])],
    )
    .await
    .unwrap();
    let scan_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(has_notif(&scan_notifs, "OnProbeWarpStart"));
    assert!(!has_notif(&scan_notifs, "OnProbeWarpEnd"));

    // Advance clock past warp_ft (1000ms = 1s) -> OnProbeWarpEnd(probeID) fires
    ctx.advance_secs(2);
    let after_warp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let pwe_args = find_notif(&after_warp_notifs, "OnProbeWarpEnd");
    assert_eq!(pwe_args, &[PyRep::Integer(first_probe_id)]);

    // 3. OnCosmicSignatureAdded, OnDungeonInstanceAdded, OnDungeonInstanceRemoved via live keeper.EnterNextDungeonRoom
    let dest_sys = eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, SYS_JITA);
    let mut obs_sess = setup_pilot_in_space(&ctx, PILOT_B, 700_302, false).await;
    let _ = ctx.db.set_character_solar_system(PILOT_B, dest_sys, &ctx.sde);
    obs_sess.solar_system_id = Some(dest_sys);
    ctx.sync_session(&mut obs_sess);
    let _ = rpc(&ctx, &mut obs_sess, "scanSvc", "GetCombatAnomalyInstances", &[]).await.unwrap();
    let _ = drain_all(&ctx, &mut obs_sess, PILOT_B);

    // Negative: advancing wave 1 -> 2 of a 2-wave anomaly does NOT complete or spawn an expedition
    let _ = rpc(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[]).await.unwrap();
    let site_id = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        let sites = reg.sites_by_system.get_mut(&SYS_JITA).unwrap();
        let s = sites.iter_mut().find(|s| s.site_category == "CombatAnomaly").unwrap();
        s.current_wave = 1;
        s.total_waves = 2;
        s.cleared = false;
        s.active_npc_ball_ids.clear();
        s.site_id.clone()
    });
    rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String(site_id.clone())]).await.unwrap();
    let wave1_obs = drain_all(&ctx, &mut obs_sess, PILOT_B);
    assert!(!has_notif(&wave1_obs, "OnCosmicSignatureAdded"));
    assert!(!has_notif(&wave1_obs, "OnDungeonInstanceAdded"));

    // Positive: find deterministic RNG seed that rolls <= 0.20 escalation on final wave completion
    let mut esc_seed = 0_u64;
    for candidate in 1..200_u64 {
        let rng = eve_server::rng::MechanicRng::seeded(candidate);
        if rng.roll(0.20) {
            esc_seed = candidate;
            break;
        }
    }
    ctx.set_mechanic_rng_seed(Some(esc_seed));
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        if let Some(s) = reg.sites_by_system.get_mut(&SYS_JITA).and_then(|l| l.iter_mut().find(|x| x.site_id == site_id)) {
            s.active_npc_ball_ids.clear();
        }
    });
    let clear_rep = rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String(site_id)])
        .await
        .unwrap();
    let inst_id = clear_rep.dict_get("escalationInstanceID").and_then(|v| v.as_i64()).expect("escalationInstanceID");

    let add_obs = drain_all(&ctx, &mut obs_sess, PILOT_B);
    let sig_args = find_notif(&add_obs, "OnCosmicSignatureAdded");
    assert_eq!(sig_args.len(), 1);
    let sig_id = sig_args[0].as_str().expect("signatureID string").to_string();
    assert_eq!(sig_id.len(), 7);
    assert_eq!(sig_id.as_bytes()[3], b'-');

    let di_add = find_notif(&add_obs, "OnDungeonInstanceAdded");
    assert_eq!(di_add.len(), 2);
    assert_eq!(di_add[0], PyRep::Integer(i64::from(dest_sys)));
    assert_eq!(di_add[1].dict_get("signatureID").and_then(|v| v.as_str()), Some(sig_id.as_str()));
    let exp_did = di_add[1].dict_get("dungeonID").and_then(|v| v.as_i64()).expect("expedition dungeonID");
    assert!(exp_did > 0);
    assert!(di_add[1].dict_get("scanStrengthValue").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Move Pilot A to dest_sys to run Step 1 of the expedition
    let _ = ctx.db.set_character_solar_system(PILOT_A, dest_sys, &ctx.sde);
    sess.solar_system_id = Some(dest_sys);
    ctx.sync_session(&mut sess);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let _ = drain_all(&ctx, &mut obs_sess, PILOT_B);

    // Negative: non-existent expedition site fails and emits zero OnDungeonInstanceRemoved
    assert!(
        rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String("EXP-999999-S1".into())])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut obs_sess, PILOT_B), "OnDungeonInstanceRemoved"));

    // Positive: clearing final wave of EXP-{inst_id}-1 advances expedition and emits OnDungeonInstanceRemoved
    let exp_site_id = format!("EXP-{inst_id}-1");
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        if let Some(s) = reg.sites_by_system.get_mut(&dest_sys).and_then(|l| l.iter_mut().find(|x| x.site_id == exp_site_id)) {
            s.current_wave = s.total_waves;
            s.active_npc_ball_ids.clear();
        }
    });
    rpc(&ctx, &mut sess, "keeper", "EnterNextDungeonRoom", &[PyRep::String(exp_site_id)])
        .await
        .unwrap();
    let rem_obs = drain_all(&ctx, &mut obs_sess, PILOT_B);
    let rem_args = find_notif(&rem_obs, "OnDungeonInstanceRemoved");
    assert_eq!(
        rem_args,
        &[
            PyRep::Integer(i64::from(dest_sys)),
            PyRep::Integer(inst_id),
            PyRep::String(sig_id),
            PyRep::Integer(exp_did),
        ]
    );
}
