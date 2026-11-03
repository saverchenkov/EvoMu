//! Runtime integration tests for open `notify:*` parity rows (Part 1).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::{
    notify_emitters_b23, notify_emitters_batch8_ops,
    pve_expeditions::EscalatingPathEntry,
    pve_exploration::notify_pve_destruction_events,
};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

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

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
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

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type: u32,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
        );
    });
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    let _ = ctx
        .systems
        .get_or_spawn(SYS_JITA)
        .spawn_ball_with_combat(make_ball(ship_id, ship_type, DVec3::ZERO), Default::default())
        .await;
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

#[tokio::test]
async fn test_parity_notifies_abyssal_and_void_space() {
    // parity: notify:OnAbyssalContentFinished, notify:OnAbyssalLocationEntered
    // parity: notify:OnVoidSpaceContentFinished, notify:OnVoidSpaceEntered, notify:OnVoidSpaceRelog
    eve_testkit::attest(&[
        "notify:OnAbyssalContentFinished",
        "notify:OnAbyssalLocationEntered",
        "notify:OnVoidSpaceContentFinished",
        "notify:OnVoidSpaceEntered",
        "notify:OnVoidSpaceRelog",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 1101, "demo", "demo-secret", PILOT_A, 710_001, 622).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = 30_000_021;
        }
        if let Some(ship) = snap.items.get_mut(&710_001) {
            ship.location_id = 30_000_021;
        }
        snap.items.insert(
            710_010,
            make_item(710_010, 47_762, PILOT_A, 710_001, InventoryFlag::Cargo, 1, false),
        );
    });
    sess.solar_system_id = Some(30_000_021);
    let _ = ctx
        .systems
        .get_or_spawn(30_000_021)
        .spawn_ball_with_combat(make_ball(710_001, 622, DVec3::ZERO), Default::default())
        .await;

    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(710_010)])
        .await
        .expect("AbyssalEntranceDeployment");
    let gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        *reg.abyssal_traces.keys().next().expect("trace gate")
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate_id)])
        .await
        .expect("AbyssalEntranceGateActivation");
    let abyss_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let entered = find_notif(&abyss_notifs, "OnAbyssalLocationEntered");
    let content_id = entered[0]
        .dict_get("content_id")
        .and_then(|v| v.as_i64())
        .expect("content_id");
    assert!(content_id > 0);

    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(p) = reg.abyssal_pockets.get_mut(&PILOT_A) {
            p.current_room = 3;
            p.rooms[2].npc_ball_ids.clear();
            p.rooms[2].cleared = true;
        }
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.expect("ClientIsReady");
    let end_gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets
            .get(&PILOT_A)
            .map(|p| 1_895_000_000 + p.pocket_id * 10 + 3)
            .expect("end gate")
    });
    if let Some(pocket_sys) = sess.solar_system_id {
        let abyss_sys = ctx.systems.get_or_spawn(pocket_sys);
        if let Ok(Some((_, gate_ball))) = abyss_sys.get_ball_snapshot(end_gate_id).await {
            let _ = abyss_sys.set_ball_position(710_001, gate_ball.position).await;
        }
    }
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(end_gate_id)])
        .await
        .expect("AbyssalEndGateActivation");
    let abyss_fin = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&abyss_fin, "OnAbyssalContentFinished"),
        &[PyRep::Integer(content_id), PyRep::Integer(1)]
    );

    // Void Space: enter -> relog -> exit
    let mut sess_b = setup_pilot_in_space(&ctx, 1102, "other", "other-secret", PILOT_B, 710_002, 622).await;
    let void_fil_type = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 4145)
        .map(|(&id, _)| id)
        .unwrap_or(60_770);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            710_011,
            make_item(710_011, void_fil_type, PILOT_B, 710_002, InventoryFlag::Cargo, 1, false),
        );
    });
    ctx.advance_secs(12);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(710_011)])
        .await
        .expect("VoidSpacePlayerJump");
    let vse = drain_all(&ctx, &mut sess_b, PILOT_B);
    let vse_args = find_notif(&vse, "OnVoidSpaceEntered");
    let vs_cid = vse_args[0]
        .dict_get("content_id")
        .and_then(|v| v.as_i64())
        .expect("void content_id");
    assert!(vs_cid > 0);

    let _ = rpc(&ctx, &mut sess_b, "voidSpaceMgr", "ClientIsReady", &[]).await;
    let relog = drain_all(&ctx, &mut sess_b, PILOT_B);
    let relog_args = find_notif(&relog, "OnVoidSpaceRelog");
    assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(vs_cid));

    let void_exit_gate = 999_001_i64;
    if let Some(vs_sys) = sess_b.solar_system_id {
        let void_sys = ctx.systems.get_or_spawn(vs_sys);
        let _ = void_sys
            .spawn_ball_with_combat(make_ball(void_exit_gate, 60_608, DVec3::ZERO), Default::default())
            .await;
        let _ = void_sys.set_ball_position(710_002, DVec3::ZERO).await;
    }
    ctx.advance_secs(12);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(void_exit_gate)])
        .await
        .expect("VoidSpaceEndGateActivation");
    let void_fin = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&void_fin, "OnVoidSpaceContentFinished"),
        &[PyRep::Integer(vs_cid), PyRep::Integer(1)]
    );
}

#[tokio::test]
async fn test_parity_notifies_operations_and_node_graphs() {
    // parity: notify:OnOperationSkipped, notify:OnOperationsProgressReset, notify:OnOperationsServerFSDReload
    // parity: notify:OnStartClientNodeGraph, notify:OnClientNodeGraphUpdate, notify:OnClientNodeGraphMessage
    // parity: notify:OnStopClientNodeGraph, notify:OnStopClientNodeGraphById, notify:OnStopAllClientNodeGraphs
    eve_testkit::attest(&[
        "notify:OnOperationSkipped",
        "notify:OnOperationsProgressReset",
        "notify:OnOperationsServerFSDReload",
        "notify:OnStartClientNodeGraph",
        "notify:OnClientNodeGraphUpdate",
        "notify:OnClientNodeGraphMessage",
        "notify:OnStopClientNodeGraph",
        "notify:OnStopClientNodeGraphById",
        "notify:OnStopAllClientNodeGraphs",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 1103, "demo", "demo-secret", PILOT_A, 710_003, 622).await;
    sess.role = ROLE_GM_DEFAULT;

    let op_id = *eve_server::services::operations_manager_helpers::get_operation_defs(&ctx.sde)
        .op_tasks
        .keys()
        .next()
        .expect("operation in SDE");
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/operation skip 1 {op_id}"))])
        .await
        .expect("/operation skip");
    let skip_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&skip_n, "OnOperationSkipped"), &[PyRep::Integer(1), PyRep::Integer(op_id)]);

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/operation reset_progress".into())])
        .await
        .expect("/operation reset_progress");
    let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reset_n, "OnOperationsProgressReset").len(), 3);

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/operation reload_fsd".into())])
        .await
        .expect("/operation reload_fsd");
    let reload_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reload_n, "OnOperationsServerFSDReload").len(), 0);

    let graph_ids: Vec<i64> = ctx.sde.node_graphs.keys().take(3).map(|&id| i64::from(id)).collect();
    let (g1, g2, g3) = (graph_ids[0], graph_ids[1], graph_ids[2]);
    let _inst_a = notify_emitters_batch8_ops::start_client_node_graph_instance(
        &ctx,
        PILOT_A,
        g1,
        1,
        101,
        BTreeMap::new(),
    )
    .expect("start graph 1");
    let _inst_b = notify_emitters_batch8_ops::start_client_node_graph_instance(
        &ctx,
        PILOT_A,
        g2,
        2,
        102,
        BTreeMap::new(),
    )
    .expect("start graph 2");
    let _inst_c = notify_emitters_batch8_ops::start_client_node_graph_instance(
        &ctx,
        PILOT_A,
        g3,
        3,
        103,
        BTreeMap::new(),
    )
    .expect("start graph 3");
    let start_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&start_n, "OnStartClientNodeGraph")[0], PyRep::Integer(g1));

    notify_emitters_batch8_ops::stop_active_operation_node_graph_for_category(&ctx, PILOT_A, 1);
    let stop_cat_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&stop_cat_n, "OnStopClientNodeGraph"), &[PyRep::Integer(_inst_a)]);

    notify_emitters_batch8_ops::stop_client_node_graph_instances_by_id(&ctx, PILOT_A, g2);
    let stop_id_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&stop_id_n, "OnStopClientNodeGraphById"), &[PyRep::Integer(g2)]);

    notify_emitters_batch8_ops::stop_all_client_node_graphs_for_char(&ctx, PILOT_A);
    let stop_all_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&stop_all_n, "OnStopAllClientNodeGraphs").len(), 0);

    // Drive real AIR NPE node-graph runner RPCs -> emits OnClientNodeGraphUpdate and OnClientNodeGraphMessage
    let mut npe_sess = SessionState::new(1105);
    common::login_pilot(&ctx, &mut npe_sess, "demo", "demo-secret").await;
    let doll = PyRep::keyval([("genderID", PyRep::Integer(1)), ("bloodlineID", PyRep::Integer(1))]);
    let portrait = PyRep::keyval([("backgroundID", PyRep::Integer(1))]);
    let npe_cid = rpc(
        &ctx,
        &mut npe_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("NPE Notify Pilot".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            doll,
            portrait,
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll")
    .as_i64()
    .expect("npe_cid");
    rpc(&ctx, &mut npe_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(npe_cid)])
        .await
        .expect("SelectCharacterID npe");
    rpc(
        &ctx,
        &mut npe_sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .await
    .expect("start_tutorial");
    let _ = drain_all(&ctx, &mut npe_sess, npe_cid);
    rpc(
        &ctx,
        &mut npe_sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("new_eden_cinematic_ended".into())],
    )
    .await
    .expect("new_eden_cinematic_ended");
    let msg_n = drain_all(&ctx, &mut npe_sess, npe_cid);
    assert!(!find_notif(&msg_n, "OnClientNodeGraphUpdate").is_empty());
    assert_eq!(
        find_notif(&msg_n, "OnClientNodeGraphMessage")
            .get(2)
            .and_then(|v| v.as_str()),
        Some("pocket_entered")
    );
}

#[tokio::test]
async fn test_parity_notifies_dungeons_cloak_and_space() {
    // parity: notify:OnCloakModuleForceDeactivated, notify:OnDungeonCameraInterest
    // parity: notify:OnDungeonTriggerAmbientSound, notify:OnDungeonTriggerAudio
    // parity: notify:OnDungeonTriggerMessage, notify:OnDungeonTriggerMusic
    // parity: notify:OnFleetEscalatingPathMessage, notify:OnProgressionRoomJoined
    eve_testkit::attest(&[
        "notify:OnCloakModuleForceDeactivated",
        "notify:OnDungeonCameraInterest",
        "notify:OnDungeonTriggerAmbientSound",
        "notify:OnDungeonTriggerAudio",
        "notify:OnDungeonTriggerMessage",
        "notify:OnDungeonTriggerMusic",
        "notify:OnFleetEscalatingPathMessage",
        "notify:OnProgressionRoomJoined",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 1104, "demo", "demo-secret", PILOT_A, 710_004, 622).await;

    notify_emitters_b23::emit_cloak_module_force_deactivated(&ctx, PILOT_A, "TooCloseToObject", 1850.0);
    let cloak_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&cloak_n, "OnCloakModuleForceDeactivated")[0], PyRep::String("TooCloseToObject".into()));

    notify_emitters_b23::emit_dungeon_camera_interest_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        None,
        PILOT_A,
        81_001,
        1.5,
        4.0,
    );
    notify_emitters_b23::emit_dungeon_trigger_ambient_sound_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        None,
        PILOT_A,
        4001,
        202,
        81_001,
        1,
    );
    notify_emitters_b23::emit_dungeon_trigger_audio_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        None,
        PILOT_A,
        4001,
        "res:/audio/dungeon.wav",
    );
    notify_emitters_b23::emit_dungeon_trigger_message_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        None,
        PILOT_A,
        1,
        9901,
    );
    notify_emitters_b23::emit_dungeon_trigger_music_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        None,
        PILOT_A,
        4001,
        303,
    );
    let dng_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&dng_n, "OnDungeonCameraInterest")[0], PyRep::Integer(81_001));
    assert_eq!(find_notif(&dng_n, "OnDungeonTriggerAmbientSound")[0], PyRep::Integer(4001));
    assert_eq!(find_notif(&dng_n, "OnDungeonTriggerAudio")[0], PyRep::Integer(4001));
    assert_eq!(find_notif(&dng_n, "OnDungeonTriggerMessage"), &[PyRep::Integer(1), PyRep::Integer(9901)]);
    assert_eq!(find_notif(&dng_n, "OnDungeonTriggerMusic")[0], PyRep::Integer(4001));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.fleet_id = Some(77_001);
        }
        let mut fl = eve_db::FleetRecord::default();
        fl.fleet_id = 77_001;
        fl.boss_id = PILOT_A;
        fl.members = vec![PILOT_A, PILOT_B];
        snap.fleets.insert(77_001, fl);
    });
    let esc = EscalatingPathEntry {
        instance_id: 500_001,
        character_id: PILOT_A,
        source_dungeon_id: 1001,
        expedition_dungeon_id: 4002,
        expedition_name: "Guristas Scout Outpost".to_string(),
        destination_solar_system_id: SYS_JITA,
        step_index: 2,
        total_steps: 4,
        journal_text: "Escalation".to_string(),
        created_at_sec: ctx.now_unix_secs(),
        expires_at_sec: ctx.now_unix_secs() + 86_400,
        dungeons_entered: vec![],
        journal_entry_template_id: 280_001,
    };
    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        PILOT_A,
        None,
        Some(esc),
        None,
    );
    let esc_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&esc_n, "OnFleetEscalatingPathMessage"), &[PyRep::Integer(PILOT_A), PyRep::Integer(280_001)]);

    let inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        &ctx,
        PILOT_A,
        SYS_JITA,
        2001,
        "Deadspace",
        [120_000_000.0, 0.0, 80_000_000.0],
        true,
    )
    .await
    .expect("spawn_sde_dungeon_runtime_instance");
    let dng_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id)
        .expect("get_dungeon_instance_state");
    let entry_gate_id = dng_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
    let entry_gate_pos = sys_h.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(710_004, entry_gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)])
        .await
        .expect("ActivateAccelerationGate");
    let pr_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&pr_n, "OnProgressionRoomJoined").len(), 5);
}
