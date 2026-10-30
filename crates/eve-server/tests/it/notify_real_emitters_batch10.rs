//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 1).
//!
//! Covers Abyssal, Void Space, Proving Grounds, Tournaments, Dungeons, and Escalating Paths.

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
    let (user, pass) = if char_id == PILOT_A {
        ("demo", "demo-secret")
    } else {
        ("other", "other-secret")
    };
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

/// parity: notify:OnAbyssalLocationEntered
/// parity: notify:OnAbyssalContentFinished
/// parity: notify:OnVoidSpaceEntered
/// parity: notify:OnVoidSpaceRelog
/// parity: notify:OnVoidSpaceContentFinished
#[tokio::test]
async fn batch10_abyssal_and_void_space_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_001, false).await;

    // 1. Abyssal filament deployment -> gate activation -> end gate activation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = 30_000_021;
        }
        if let Some(ship) = snap.items.get_mut(&700_001) {
            ship.type_id = 622; // Stabber (cruiser)
            ship.location_id = 30_000_021;
        }
        snap.items.insert(
            700_010,
            make_item(700_010, 47_762, PILOT_A, 700_001, InventoryFlag::Cargo, 1, false),
        );
    });
    sess.solar_system_id = Some(30_000_021);
    let _dep = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(700_010)])
        .await
        .unwrap();
    let gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        *reg.abyssal_traces.keys().next().unwrap_or(&900_001)
    });
    let _ = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate_id)])
        .await
        .unwrap();
    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let entered = find_notif(&notifs, "OnAbyssalLocationEntered");
    assert_eq!(entered.len(), 1);
    assert!(entered[0].dict_get("content_id").is_some());

    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(p) = reg.abyssal_pockets.get_mut(&PILOT_A) {
            p.current_room = 3;
            p.rooms[2].npc_ball_ids.clear();
            p.rooms[2].cleared = true;
        }
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[])
        .await
        .unwrap();
    let end_gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets
            .get(&PILOT_A)
            .map(|p| 1_895_000_000 + p.pocket_id * 10 + 3)
            .unwrap_or(gate_id)
    });
    if let Some(pocket_sys) = sess.solar_system_id {
        let abyss_sys = ctx.systems.get_or_spawn(pocket_sys);
        if let Ok(Some((_, gate_ball))) = abyss_sys.get_ball_snapshot(end_gate_id).await {
            let _ = abyss_sys.set_ball_position(700_001, gate_ball.position).await;
        }
    }
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(end_gate_id)])
        .await
        .unwrap();
    let notifs2 = drain_all(&ctx, &mut sess, PILOT_A);
    let finished = find_notif(&notifs2, "OnAbyssalContentFinished");
    assert_eq!(finished.len(), 2);

    // 2. Void Space jump, relog, and end gate activation
    let void_fil_type = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 4145)
        .map(|(&id, _)| id)
        .unwrap_or(60_770);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_011,
            make_item(700_011, void_fil_type, PILOT_A, 700_001, InventoryFlag::Cargo, 1, false),
        );
    });
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(700_011)])
        .await
        .unwrap();
    let vse = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&vse, "OnVoidSpaceEntered").len(), 1);

    let _ = rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await;
    let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&relog_notifs, "OnVoidSpaceRelog").len(), 1);

    let void_exit_gate = 999_001_i64;
    if let Some(vs_sys) = sess.solar_system_id {
        let void_sys = ctx.systems.get_or_spawn(vs_sys);
        let _ = void_sys
            .spawn_ball_with_combat(
                Ball {
                    id: void_exit_gate,
                    type_id: 60_608,
                    position: DVec3::ZERO,
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
        let _ = void_sys.set_ball_position(700_001, DVec3::ZERO).await;
    }
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(void_exit_gate)])
        .await
        .unwrap();
    let void_fin = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&void_fin, "OnVoidSpaceContentFinished").len(), 2);
}

/// parity: notify:OnMatchFound
/// parity: notify:OnMatchMakingFailure
/// parity: notify:OnProvingGroundsMatchFound
/// parity: notify:OnAddExpiryDurationForAbyssalPvpContent
/// parity: notify:OnEnterTournamentSystem
/// parity: notify:OnLeaveTournamentSystem
/// parity: notify:OnTournamentMatchStateChange
#[tokio::test]
async fn batch10_pvp_filament_and_tournament_notifications() {
    let ctx = ServerContext::new();
    let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, 700_101, true).await;
    let mut sess_b = setup_pilot_in_space(&ctx, PILOT_B, 700_102, false).await;

    // 1. JoinPVPQueue with missing item triggers OnMatchMakingFailure
    let err = rpc(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(999_999), PyRep::Integer(56_134)],
    )
    .await;
    assert!(err.is_err());
    let fail_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let fail_args = find_notif(&fail_notifs, "OnMatchMakingFailure");
    assert_eq!(fail_args.len(), 1);

    // 2. spawn_matched_arena_instance emits OnMatchFound, OnProvingGroundsMatchFound, OnAddExpiryDurationForAbyssalPvpContent
    eve_server::services::pvp_filament_arena::spawn_matched_arena_instance(
        &ctx,
        &mut sess_a,
        5001,
        5002,
        [0.0, 0.0, 0.0],
        vec![vec![PILOT_A], vec![PILOT_B]],
        vec![(PILOT_A, 700_101, SYS_JITA), (PILOT_B, 700_102, SYS_JITA)],
    )
    .await;
    let match_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&match_notifs, "OnMatchFound").len(), 0);
    assert_eq!(find_notif(&match_notifs, "OnProvingGroundsMatchFound").len(), 0);
    let exp_args = find_notif(&match_notifs, "OnAddExpiryDurationForAbyssalPvpContent");
    assert_eq!(exp_args.len(), 1);

    // 3. Tournament system enter, match state change, and leave
    let created = rpc(
        &ctx,
        &mut sess_a,
        "tourneyMgr",
        "CreateMatch",
        &[PyRep::String("Batch10 Cup".into())],
    )
    .await
    .unwrap();
    let match_id = created.dict_get("matchID").and_then(|v| v.as_i64()).unwrap_or(1);

    let _ = ctx.db.set_character_solar_system(PILOT_A, 30_002_187, &ctx.sde);
    sess_a.solar_system_id = Some(30_002_187);
    ctx.sync_session(&mut sess_a);
    rpc(&ctx, &mut sess_a, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(match_id)])
        .await
        .unwrap();
    let enter_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&enter_notifs, "OnEnterTournamentSystem").len(), 0);
    let state_args = find_notif(&enter_notifs, "OnTournamentMatchStateChange");
    assert_eq!(state_args.len(), 4);

    let _ = rpc(&ctx, &mut sess_a, "tourneyMgr", "MatchCompleted", &[PyRep::Integer(match_id)]).await;
    let _ = rpc(&ctx, &mut sess_a, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)]).await;
    let leave_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&leave_notifs, "OnLeaveTournamentSystem").len(), 0);
}

/// parity: notify:OnDistributionDungeonEntered
/// parity: notify:OnEscalatingPathDungeonEntered
/// parity: notify:OnEscalatingPathChange
/// parity: notify:OnEnteringDungeonRoom
/// parity: notify:OnExitingDungeon
/// parity: notify:OnDungeonCompleted
/// parity: notify:OnDungeonValues
/// parity: notify:OnDungeonCameraInterest
/// parity: notify:OnDungeonEdit
/// parity: notify:OnDungeonTriggerAmbientSound
/// parity: notify:OnDungeonTriggerAudio
/// parity: notify:OnDungeonTriggerMessage
/// parity: notify:OnDungeonTriggerMusic
#[tokio::test]
async fn batch10_dungeons_and_escalating_paths_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_201, true).await;

    // 1. Escalating path creation & step completion
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
    let esc_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&esc_notifs, "OnEscalatingPathChange").len(), 0);
    assert_eq!(find_notif(&esc_notifs, "OnEscalatingPathDungeonEntered").len(), 1);
    assert_eq!(find_notif(&esc_notifs, "OnDistributionDungeonEntered").len(), 1);

    let _ = eve_server::services::pve_expeditions::advance_expedition_step(
        &ctx,
        PILOT_A,
        entry.instance_id,
    )
    .await
    .unwrap();
    let comp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&comp_notifs, "OnDungeonCompleted").len(), 1);

    // 2. Dungeon editor / room entry / triggers / exit
    let dungeon_id = i64::from(*ctx.sde.dungeon_gas_types.keys().next().unwrap_or(&1001));
    rpc(&ctx, &mut sess, "keeper", "EditDungeon", &[PyRep::Integer(dungeon_id)])
        .await
        .unwrap();
    let dun_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonEdit").len(), 3);
    assert_eq!(find_notif(&dun_notifs, "OnEnteringDungeonRoom").len(), 5);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonValues").len(), 2);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonCameraInterest").len(), 3);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonTriggerAmbientSound").len(), 3);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonTriggerAudio").len(), 2);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonTriggerMessage").len(), 2);
    assert_eq!(find_notif(&dun_notifs, "OnDungeonTriggerMusic").len(), 2);

    let _ = rpc(&ctx, &mut sess, "keeper", "ResetDungeon", &[PyRep::Integer(1001)]).await;
    let exit_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&exit_notifs, "OnExitingDungeon").len(), 1);
}
