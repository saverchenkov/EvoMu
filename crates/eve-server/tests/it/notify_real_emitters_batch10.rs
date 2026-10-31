//! Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 1).
//!
//! Covers Abyssal Deadspace, Void Space, Space White-Out, Zarzakh Deathzone, Proving Grounds
//! matchmaking, and Tournament arena/match state transitions via live RPCs and cluster ticks.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

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

fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(ev, _)| ev == event)
}

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    gm: bool,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type_id, char_id, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
        );
    });
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys
        .spawn_ball_with_combat(make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)), Default::default())
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

/// parity: notify:OnAbyssalLocationEntered
/// parity: notify:OnAbyssalContentFinished
/// parity: notify:OnSpaceWhiteOutStart
/// parity: notify:OnDeathzoneStateUpdate
/// parity: notify:OnVoidSpaceEntered
/// parity: notify:OnVoidSpaceRelog
/// parity: notify:OnVoidSpaceContentFinished
#[tokio::test]
async fn batch10_abyssal_void_whiteout_and_deathzone_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_001, 622, false).await;

    // 1. Abyssal filament deployment -> gate activation -> end gate activation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = 30_000_021;
        }
        if let Some(ship) = snap.items.get_mut(&700_001) {
            ship.location_id = 30_000_021;
        }
        snap.items.insert(
            700_010,
            make_item(700_010, 47_762, PILOT_A, 700_001, InventoryFlag::Cargo, 1, false),
        );
    });
    sess.solar_system_id = Some(30_000_021);
    let _ = ctx
        .systems
        .get_or_spawn(30_000_021)
        .spawn_ball_with_combat(make_ball(700_001, 622, DVec3::ZERO), Default::default())
        .await;

    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(700_010)])
        .await
        .unwrap();
    let gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        *reg.abyssal_traces.keys().next().unwrap()
    });

    // Negative: invalid gate does not emit OnAbyssalLocationEntered
    assert!(
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(999_999)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnAbyssalLocationEntered"));

    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate_id)])
        .await
        .unwrap();
    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let entered = find_notif(&notifs, "OnAbyssalLocationEntered");
    assert_eq!(entered.len(), 1);
    let content_id = entered[0].dict_get("content_id").and_then(|v| v.as_i64()).expect("content_id");
    assert!(content_id > 0);
    assert_eq!(entered[0].dict_get("difficulty_tier").and_then(|v| v.as_i64()), Some(1));
    assert!(entered[0].dict_get("location_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(entered[0].dict_get("nebula_graphic_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // Negative: invalid end gate does not emit OnAbyssalContentFinished
    assert!(
        rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(999_999)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnAbyssalContentFinished"));

    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(p) = reg.abyssal_pockets.get_mut(&PILOT_A) {
            p.current_room = 3;
            p.rooms[2].npc_ball_ids.clear();
            p.rooms[2].cleared = true;
        }
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.unwrap();
    let end_gate_id = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.get(&PILOT_A).map(|p| 1_895_000_000 + p.pocket_id * 10 + 3).unwrap()
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
    assert_eq!(finished, &[PyRep::Integer(content_id), PyRep::Integer(1)]);

    // 2. OnSpaceWhiteOutStart on 20-minute Abyssal pocket collapse
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_012,
            make_item(700_012, 47_762, PILOT_A, 700_001, InventoryFlag::Cargo, 1, false),
        );
    });
    ctx.advance_secs(15);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(700_012)])
        .await
        .unwrap();
    let gate2 = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        *reg.abyssal_traces.keys().max().unwrap()
    });
    rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(gate2)])
        .await
        .unwrap();
    let pocket_sys_2 = sess.solar_system_id.expect("in second abyssal pocket");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // 2a. OnDeathzoneStateUpdate via live cluster tick in Abyssal pocket (32000001)
    // Negative: inside ABYSSAL_ROOM_BOUNDARY_RADIUS_M (50km <= 75km from grid_offset) -> no emission
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnDeathzoneStateUpdate"));

    // Positive: move outside 75km room boundary relative to grid_offset (80km) -> emits danger trackingData
    let pocket_center = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets
            .get(&PILOT_A)
            .map(|p| DVec3::from_array(p.grid_offset))
            .unwrap()
    });
    let abyss_sys_2 = ctx.systems.get_or_spawn(pocket_sys_2);
    let _ = abyss_sys_2
        .set_ball_position(700_001, pocket_center + DVec3::new(80_000.0, 0.0, 0.0))
        .await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dz_enter = drain_all(&ctx, &mut sess, PILOT_A);
    let dz_args = find_notif(&dz_enter, "OnDeathzoneStateUpdate");
    assert_eq!(dz_args.len(), 3);
    assert_eq!(dz_args[0], PyRep::Integer(i64::from(pocket_sys_2)));
    assert_eq!(dz_args[1], PyRep::Integer(700_001));
    assert_eq!(dz_args[2].dict_get("dangerLevel").and_then(|v| v.as_f64()), Some(1.0));

    // Move back inside 75km room boundary (10km from grid_offset) -> emits trackingData = None
    let _ = abyss_sys_2
        .set_ball_position(700_001, pocket_center + DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dz_exit = drain_all(&ctx, &mut sess, PILOT_A);
    let dz_exit_args = find_notif(&dz_exit, "OnDeathzoneStateUpdate");
    assert_eq!(
        dz_exit_args,
        &[
            PyRep::Integer(i64::from(pocket_sys_2)),
            PyRep::Integer(700_001),
            PyRep::None,
        ]
    );

    // Negative: advancing 10s (well before 20-min expiry) does NOT emit OnSpaceWhiteOutStart
    ctx.advance_secs(10);
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnSpaceWhiteOutStart"));

    // Positive: advancing past 20-min timer collapses pocket and emits OnSpaceWhiteOutStart(pocket_sys)
    ctx.advance_secs(1_250);
    let collapse_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let wo_args = find_notif(&collapse_notifs, "OnSpaceWhiteOutStart");
    assert_eq!(wo_args, &[PyRep::Integer(i64::from(pocket_sys_2))]);

    // 3. Void Space jump, relog, and end gate activation (restore clean ship in k-space 30000021)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = 30_000_021;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = 700_002;
        }
        snap.items.insert(
            700_002,
            make_item(700_002, 622, PILOT_A, 30_000_021, InventoryFlag::None, 1, true),
        );
    });
    sess.solar_system_id = Some(30_000_021);
    sess.station_id = None;
    sess.ship_id = Some(700_002);
    let _ = ctx
        .systems
        .get_or_spawn(30_000_021)
        .spawn_ball_with_combat(make_ball(700_002, 622, DVec3::ZERO), Default::default())
        .await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: ClientIsReady outside void space does not emit OnVoidSpaceRelog
    let _ = rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnVoidSpaceRelog"));

    // Negative: invalid filament does not emit OnVoidSpaceEntered
    assert!(
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(999_999)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnVoidSpaceEntered"));

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
            make_item(700_011, void_fil_type, PILOT_A, 700_002, InventoryFlag::Cargo, 1, false),
        );
    });
    ctx.advance_secs(12);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[PyRep::Integer(700_011)])
        .await
        .unwrap();
    let vse = drain_all(&ctx, &mut sess, PILOT_A);
    let vse_args = find_notif(&vse, "OnVoidSpaceEntered");
    assert_eq!(vse_args.len(), 1);
    let vs_cid = vse_args[0].dict_get("content_id").and_then(|v| v.as_i64()).expect("void content_id");
    assert!(vs_cid > 0);
    assert!(vse_args[0].dict_get("nebula_graphic_id").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let _ = rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await;
    let relog_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let relog_args = find_notif(&relog_notifs, "OnVoidSpaceRelog");
    assert_eq!(relog_args.len(), 1);
    assert_eq!(relog_args[0].dict_get("content_id").and_then(|v| v.as_i64()), Some(vs_cid));
    assert!(
        relog_args[0].dict_get("end_time").and_then(|v| v.as_i64()).unwrap_or(0)
            > ctx.now_blueos_filetime_ticks()
    );

    // Negative: invalid exit gate does not emit OnVoidSpaceContentFinished
    assert!(
        rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(888_888)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnVoidSpaceContentFinished"));

    let void_exit_gate = 999_001_i64;
    if let Some(vs_sys) = sess.solar_system_id {
        let void_sys = ctx.systems.get_or_spawn(vs_sys);
        let _ = void_sys
            .spawn_ball_with_combat(make_ball(void_exit_gate, 60_608, DVec3::ZERO), Default::default())
            .await;
        let _ = void_sys.set_ball_position(700_002, DVec3::ZERO).await;
    }
    ctx.advance_secs(12);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[PyRep::Integer(void_exit_gate)])
        .await
        .unwrap();
    let void_fin = drain_all(&ctx, &mut sess, PILOT_A);
    let void_fin_args = find_notif(&void_fin, "OnVoidSpaceContentFinished");
    assert_eq!(void_fin_args, &[PyRep::Integer(vs_cid), PyRep::Integer(1)]);
}

/// parity: notify:OnMatchMakingFailure
/// parity: notify:OnMatchFound
/// parity: notify:OnProvingGroundsMatchFound
/// parity: notify:OnAddExpiryDurationForAbyssalPvpContent
/// parity: notify:OnEnterTournamentSystem
/// parity: notify:OnTournamentMatchStateChange
/// parity: notify:OnLeaveTournamentSystem
#[tokio::test]
async fn batch10_proving_grounds_and_tournament_notifications() {
    let ctx = ServerContext::new();
    // Set clock inside SDE schedule 170 (2023-01-07T12:00:00Z, 2v2 Battleships Proving Filament 72984)
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_673_092_800)));

    let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, 700_101, 638, true).await;
    let mut sess_b = setup_pilot_in_space(&ctx, PILOT_B, 700_102, 638, false).await;

    // 1. JoinPVPQueue with missing filament emits OnMatchMakingFailure (and NOT OnMatchFound)
    let err = rpc(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(999_999), PyRep::Integer(72_984)],
    )
    .await
    .unwrap_err();
    let fail_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!has_notif(&fail_notifs, "OnMatchFound"));
    assert!(!has_notif(&fail_notifs, "OnProvingGroundsMatchFound"));
    assert!(!has_notif(&fail_notifs, "OnAddExpiryDurationForAbyssalPvpContent"));
    let fail_args = find_notif(&fail_notifs, "OnMatchMakingFailure");
    assert_eq!(fail_args, &[PyRep::List(vec![PyRep::String(err.code)])]);

    // 2. Live 4-pilot 2v2 JoinPVPQueue matchmaking
    let (p3_id, p4_id) = (90_000_003_i64, 90_000_004_i64);
    let (ship3, ship4) = (700_103_i64, 700_104_i64);
    let (f1, f2, f3, f4) = (700_111_i64, 700_112_i64, 700_113_i64, 700_114_i64);
    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.get(&PILOT_B).unwrap().clone();
        for (cid, sid) in [(p3_id, ship3), (p4_id, ship4)] {
            let mut rec = tmpl.clone();
            rec.character_id = cid;
            rec.solar_system_id = SYS_JITA;
            rec.station_id = None;
            rec.structure_id = None;
            rec.active_ship_id = sid;
            snap.characters.insert(cid, rec);
            snap.items.insert(
                sid,
                make_item(sid, 638, cid, i64::from(SYS_JITA), InventoryFlag::None, 1, true),
            );
        }
        for (fid, cid, sid) in [
            (f1, PILOT_A, 700_101),
            (f2, PILOT_B, 700_102),
            (f3, p3_id, ship3),
            (f4, p4_id, ship4),
        ] {
            snap.items.insert(
                fid,
                make_item(fid, 72_984, cid, sid, InventoryFlag::Cargo, 1, false),
            );
        }
    });
    let mut sess_3 = sess_b.clone();
    sess_3.session_id = 90_003;
    sess_3.user_id = Some(1003);
    sess_3.character_id = Some(p3_id);
    sess_3.ship_id = Some(ship3);
    let mut sess_4 = sess_b.clone();
    sess_4.session_id = 90_004;
    sess_4.user_id = Some(1004);
    sess_4.character_id = Some(p4_id);
    sess_4.ship_id = Some(ship4);

    // Negative: first 3 pilots joining stay queued (status 30) and do NOT emit match-found notifications
    assert_eq!(
        rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(72_984)])
            .await
            .unwrap(),
        PyRep::Integer(30)
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnMatchFound"));
    assert_eq!(
        rpc(&ctx, &mut sess_3, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f3), PyRep::Integer(72_984)])
            .await
            .unwrap(),
        PyRep::Integer(30)
    );
    assert_eq!(
        rpc(&ctx, &mut sess_4, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f4), PyRep::Integer(72_984)])
            .await
            .unwrap(),
        PyRep::Integer(30)
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnProvingGroundsMatchFound"));

    // Positive: 4th pilot joins -> forms match and emits all 3 proving grounds notifications
    let now_ft = ctx.now_blueos_filetime_ticks();
    assert_eq!(
        rpc(&ctx, &mut sess_b, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f2), PyRep::Integer(72_984)])
            .await
            .unwrap(),
        PyRep::Integer(5)
    );
    let match_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(!has_notif(&match_notifs, "OnMatchMakingFailure"));
    assert_eq!(find_notif(&match_notifs, "OnMatchFound"), &[]);
    assert_eq!(find_notif(&match_notifs, "OnProvingGroundsMatchFound"), &[]);
    let exp_args = find_notif(&match_notifs, "OnAddExpiryDurationForAbyssalPvpContent");
    assert_eq!(exp_args.len(), 1);
    let exp_ft = exp_args[0].as_i64().expect("end_time filetime");
    assert!(exp_ft >= now_ft + 900 * 10_000_000);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 3. Tournament enter, match state change, and leave via live tourneyMgr RPCs
    assert!(
        rpc(&ctx, &mut sess_b, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(999_999)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnEnterTournamentSystem"));

    let _ = ctx.db.set_character_solar_system(PILOT_A, 30_002_187, &ctx.sde);
    sess_a.solar_system_id = Some(30_002_187);
    ctx.sync_session(&mut sess_a);
    let match_id = 1001_i64;
    rpc(&ctx, &mut sess_a, "tourneyMgr", "CreateMatch", &[PyRep::Integer(1), PyRep::Integer(match_id)])
        .await
        .unwrap();
    rpc(
        &ctx,
        &mut sess_a,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(PILOT_B)],
    )
    .await
    .unwrap();
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // Negative: non-referee PILOT_B cannot StartCountdown -> Err and zero OnTournamentMatchStateChange
    assert!(
        rpc(&ctx, &mut sess_b, "tourneyMgr", "StartCountdown", &[PyRep::Integer(match_id)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnTournamentMatchStateChange"));

    // Positive: referee PILOT_A starts countdown -> red player gets myTeam=0, referee gets myTeam=-1, matchStatus=2
    rpc(&ctx, &mut sess_a, "tourneyMgr", "StartCountdown", &[PyRep::Integer(match_id)])
        .await
        .unwrap();
    let b_state = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&b_state, "OnTournamentMatchStateChange"),
        &[PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(2)]
    );
    let a_state = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&a_state, "OnTournamentMatchStateChange"),
        &[PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(-1), PyRep::Integer(2)]
    );

    // Negative: calling StartCountdown again when already in Countdown emits zero duplicate notifications
    rpc(&ctx, &mut sess_a, "tourneyMgr", "StartCountdown", &[PyRep::Integer(match_id)])
        .await
        .unwrap();
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnTournamentMatchStateChange"));

    // TeleportToArena -> OnEnterTournamentSystem (0 args)
    let _ = ctx.db.set_character_solar_system(PILOT_A, SYS_JITA, &ctx.sde);
    sess_a.solar_system_id = Some(SYS_JITA);
    rpc(&ctx, &mut sess_a, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(match_id)])
        .await
        .unwrap();
    let enter_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&enter_notifs, "OnEnterTournamentSystem"), &[]);

    // Negative: non-referee TeleportToStation rejected -> zero OnLeaveTournamentSystem
    assert!(
        rpc(&ctx, &mut sess_b, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnLeaveTournamentSystem"));

    rpc(&ctx, &mut sess_a, "tourneyMgr", "MatchCompleted", &[PyRep::Integer(match_id)])
        .await
        .unwrap();
    rpc(&ctx, &mut sess_a, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)])
        .await
        .unwrap();
    let leave_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&leave_notifs, "OnLeaveTournamentSystem"), &[]);
}
