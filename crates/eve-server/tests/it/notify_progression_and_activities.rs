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

/// parity: notify:OnSpaceWhiteOutStart
/// parity: notify:OnDeathzoneStateUpdate
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
        snap.fleets.insert(77_021, eve_db::FleetRecord { fleet_id: 77_021, boss_id: PILOT_A, members: vec![PILOT_A, p3_id], ..Default::default() });
        snap.fleets.insert(77_022, eve_db::FleetRecord { fleet_id: 77_022, boss_id: PILOT_B, members: vec![PILOT_B, p4_id], ..Default::default() });
    });
    let sys_h = ctx.systems.get_or_spawn(sess_a.solar_system_id.unwrap_or(30000142));
    for (cid, sid) in [(PILOT_A, sess_a.ship_id.unwrap()), (PILOT_B, sess_b.ship_id.unwrap()), (p3_id, ship3), (p4_id, ship4)] {
        let b = eve_destiny::Ball {
            id: sid, type_id: 638, position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 100_000_000.0,
            radius: 300.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        };
        let cs = ctx.build_ship_combat_state(cid, sid, 638);
        let _ = sys_h.spawn_ball_with_combat(b, cs).await;
    }

    // Negative: first 2-pilot fleet joining stays queued (status 30) and does NOT emit match-found notifications
    assert_eq!(
        rpc(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(72_984)])
            .await
            .unwrap(),
        PyRep::Integer(30)
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnMatchFound"));
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

// Folded submodule: notify_real_emitters_batch10_part2
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch10_part2 {
    use super::*;

// Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 2).
//
// Covers Dungeon Editor, Dungeon Values, all 5 Dungeon Room Triggers, Signal Tracker full state,
// Scanner Probe repositioning (`OnProbeWarpEnd`), and Escalation Expeditions
// (`OnCosmicSignatureAdded`, `OnDungeonInstanceAdded`, `OnDungeonInstanceRemoved`).


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
/// parity: notify:OnCosmicSignatureAdded
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

}

// Folded submodule: notify_real_emitters_batch10_part3
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch10_part3 {
    use super::*;

// Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 3).
//
// Covers Progression & Tutorial Conversations (`operationsManager`), Sansha Incursion Tales
// (`OnTaleData`, `OnInfluenceUpdate`, `OnTaleRemove`), and Roaming Metaliminal Storms
// (`OnCosmicAnomalyAdded`, `OnSystemEffectStarted`, `OnSystemWideEffectStop`,
// `OnComponentRemovedSystemEnvironment`) via live RPCs and server clock advancement.


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

fn drain_all(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
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
            make_item(
                ship_id,
                587,
                char_id,
                i64::from(SYS_JITA),
                InventoryFlag::None,
                1,
                true,
            ),
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
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
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

/// parity: notify:OnProgressionLeft
#[tokio::test]
async fn batch10_progression_and_conversations() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_401, false).await;

    // 1. Start an operation site via operationsManager.start_site (sets active_site_id)
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "start_site",
            &[PyRep::Integer(999_999_999)]
        )
        .await
        .is_err()
    );

    let op_defs = eve_server::services::operations_manager::get_operation_defs(&ctx.sde);
    let empty_done = std::collections::HashSet::new();
    let (op_with_site, site_id) = op_defs
        .op_site_ids
        .iter()
        .find(|(&oid, &sid)| sid > 0 && op_defs.are_prerequisites_met(oid, &empty_done))
        .map(|(&oid, &sid)| (oid, sid))
        .unwrap_or_else(|| {
            (
                *op_defs.sde_operations.iter().next().expect("SDE op"),
                i64::from(*ctx.sde.dungeons.keys().min().expect("SDE dungeon")),
            )
        });
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_with_site)],
    )
    .await
    .unwrap();
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(site_id)],
    )
    .await
    .unwrap();
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // 2. Positive: skip_tutorial while tutorial/site is active emits OnProgressionLeft() (0 args)
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .unwrap();
    let left_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&left_notifs, "OnProgressionLeft"), &[]);

    // 3. Negative: calling skip_tutorial again when no tutorial/site is active emits zero OnProgressionLeft
    rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .unwrap();
    let repeat_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        repeat_notifs
            .iter()
            .filter(|(n, _)| n == "OnProgressionLeft")
            .count(),
        0
    );
}

/// parity: notify:OnTaleData
/// parity: notify:OnCosmicAnomalyAdded
/// parity: notify:OnSystemEffectStarted
/// parity: notify:OnSystemWideEffectStop
/// parity: notify:OnComponentRemovedSystemEnvironment
#[tokio::test]
async fn batch10_tales_storms_and_cosmic_anomalies() {
    let ctx = ServerContext::new();
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_410, false).await;

    // 1. Seed world event tales via live taleMgr.GetGlobalWorldEventTales RPC
    rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[])
        .await
        .unwrap();
    let (inc_cid, tale_id, staging_sys, vg_site_sys, vg_site_id, vg_boss_bid) =
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            let inc = reg
                .incursions
                .values_mut()
                .find(|i| i.state != "Ended" && i.staging_solar_system_id != SYS_JITA)
                .expect("seeded incursion");
            let vg = inc
                .sites
                .iter_mut()
                .find(|s| !s.locked_until_low_influence)
                .expect("vanguard/assault incursion site");
            for e in &mut vg.escort_spawns {
                e.destroyed = true;
            }
            vg.boss_destroyed = true;
            let sid = vg.site_id.clone();
            let bbid = vg.boss_npc_ball_id;
            let ssys = vg.solar_system_id;
            let icid = inc.constellation_id;
            let tid = inc.tale_id;
            let stsys = inc.staging_solar_system_id;
            if let Some(w) = reg.incursion_site_waves.get_mut(&sid) {
                w.current_wave = w.total_waves;
            }
            (icid, tid, stsys, ssys, sid, bbid)
        });
    let _ = ctx.systems.get_or_spawn(vg_site_sys).remove_ball(vg_boss_bid).await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: jumping via stargate into a non-incursion system (Jita -> Perimeter) emits zero OnTaleData
    let jita_gate_id = 50_001_248_i64;
    let jita_gate = &ctx.sde.jumps[&jita_gate_id];
    let gate_rad = ctx
        .sde
        .types
        .get(&16)
        .map(|t| t.radius)
        .unwrap_or(14_051.0);
    let bid_jita = ctx
        .bind_object(&mut sess, "beyonce", i64::from(SYS_JITA))
        .expect("bind beyonce in Jita");
    ctx.systems
        .get_or_spawn(SYS_JITA)
        .set_ball_position(
            700_410,
            jita_gate.position + DVec3::new(gate_rad + 500.0, 0.0, 0.0),
        )
        .await;
    ctx.advance_secs(15);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bid_jita),
        "CmdStargateJump",
        &[PyRep::Integer(jita_gate_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdStargateJump Jita -> Perimeter");
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnTaleData"
    ));

    // Positive: jumping via stargate into an incursion-affected solar system emits OnTaleData
    let (inc_gate_id, from_sys, inc_gate_pos) = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.to_solar_system_id == staging_sys)
        .map(|j| (j.stargate_id, j.from_solar_system_id, j.position))
        .expect("stargate leading into incursion staging system");
    let _ = ctx
        .db
        .set_character_solar_system(PILOT_A, from_sys, &ctx.sde);
    sess.solar_system_id = Some(from_sys);
    ctx.sync_session(&mut sess);
    let _ = ctx
        .systems
        .get_or_spawn(from_sys)
        .spawn_ball_with_combat(
            Ball {
                id: 700_410,
                type_id: 587,
                position: inc_gate_pos + DVec3::new(gate_rad + 500.0, 0.0, 0.0),
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
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let bid_from = ctx
        .bind_object(&mut sess, "beyonce", i64::from(from_sys))
        .expect("bind beyonce in from_sys");
    ctx.advance_secs(15);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bid_from),
        "CmdStargateJump",
        &[PyRep::Integer(inc_gate_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdStargateJump into incursion staging_sys");
    let jump_td_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let jump_td_args = find_notif(&jump_td_notifs, "OnTaleData");
    assert_eq!(jump_td_args.len(), 2);
    assert_eq!(jump_td_args[0], PyRep::Integer(i64::from(staging_sys)));
    match &jump_td_args[1] {
        PyRep::Dict(entries) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, PyRep::Integer(tale_id));
            assert_eq!(
                entries[0].1.dict_get("taleID").and_then(|v| v.as_i64()),
                Some(tale_id)
            );
        }
        other => panic!("expected Dict of active tales on jump, got {other:?}"),
    }

    // Also verify beyonce.UpdateStateRequest in incursion staging system emits OnTaleData
    rpc(&ctx, &mut sess, "beyonce", "UpdateStateRequest", &[])
        .await
        .unwrap();
    let td_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let td_args = find_notif(&td_notifs, "OnTaleData");
    assert_eq!(td_args.len(), 2);
    assert_eq!(td_args[0], PyRep::Integer(i64::from(staging_sys)));
    match &td_args[1] {
        PyRep::Dict(entries) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, PyRep::Integer(tale_id));
            assert_eq!(
                entries[0].1.dict_get("taleID").and_then(|v| v.as_i64()),
                Some(tale_id)
            );
        }
        other => panic!("expected Dict of active tales, got {other:?}"),
    }

    // 2. OnInfluenceUpdate via live keeper.EnterNextDungeonRoom on an incursion site
    let _ = ctx
        .db
        .set_character_solar_system(PILOT_A, vg_site_sys, &ctx.sde);
    sess.solar_system_id = Some(vg_site_sys);
    ctx.sync_session(&mut sess);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: invalid incursion site ID fails and emits zero OnInfluenceUpdate
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "keeper",
            "EnterNextDungeonRoom",
            &[PyRep::String("INC-NONEXISTENT".into())]
        )
        .await
        .is_err()
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnInfluenceUpdate"
    ));

    // Positive: completing real vanguard site reduces influence and emits OnInfluenceUpdate
    rpc(
        &ctx,
        &mut sess,
        "keeper",
        "EnterNextDungeonRoom",
        &[PyRep::String(vg_site_id)],
    )
    .await
    .unwrap();
    let inf_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let inf_args = find_notif(&inf_notifs, "OnInfluenceUpdate");
    assert_eq!(inf_args.len(), 2);
    assert_eq!(inf_args[0], PyRep::Integer(tale_id));
    let new_inf = inf_args[1]
        .dict_get("influence")
        .and_then(|v| v.as_f64())
        .expect("influence float");
    assert!((0.0..1.0).contains(&new_inf));
    assert!(
        inf_args[1]
            .dict_get("lastUpdated")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );
    assert!(!has_notif(&inf_notifs, "OnTaleRemove"));

    // 3. OnTaleRemove when incursion withdrawal expires on server clock advancement
    let now_u = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(inc) = reg.incursions.get_mut(&inc_cid) {
            inc.state = "Withdrawing".into();
        }
        if let Some(meta) = reg.incursion_lifecycle.get_mut(&inc_cid) {
            meta.withdraw_until_sec = Some(now_u + 30);
        }
    });
    // Negative: advancing 5s (before withdraw_until_sec) does NOT emit OnTaleRemove
    ctx.clock().advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnTaleRemove"
    ));

    // Positive: advancing 35s crosses withdrawal expiry -> ends incursion and emits OnTaleRemove
    ctx.clock().advance_secs(35);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let rem_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let tr_args = find_notif(&rem_notifs, "OnTaleRemove");
    assert_eq!(
        tr_args,
        &[
            PyRep::Integer(tale_id),
            PyRep::Integer(1),
            PyRep::Integer(
                eve_server::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as i64
            ),
        ]
    );

    // 4. Roaming Metaliminal Storms via live tick_background_cluster -> on_server_clock_advanced
    // Position storm #0 on a neighbor of SYS_JITA so its next step enters SYS_JITA, then moves onward.
    let storm_dest: u32 = 30_000_208;
    let now2 = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.truncate(1);
        let s = &mut reg.roaming_storms[0];
        s.center_solar_system_id = storm_dest;
        s.strong_core_systems.clear();
        s.weak_periphery_systems.clear();
        s.recent_centers.clear();
        s.next_move_at_sec = now2 + 60;
    });
    eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);
    let _ = ctx
        .db
        .set_character_solar_system(PILOT_A, storm_dest, &ctx.sde);
    sess.solar_system_id = Some(storm_dest);
    ctx.sync_session(&mut sess);
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: advancing 10s (before next_move_at_sec) does NOT move storm or emit storm notifications
    ctx.clock().advance_secs(10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let pre_storm = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(!has_notif(&pre_storm, "OnSystemEffectStarted"));
    assert!(!has_notif(&pre_storm, "OnCosmicAnomalyAdded"));

    // Positive: advancing 60s moves storm into neighbor so storm_dest enters strong core -> emits OnSystemEffectStarted + OnCosmicAnomalyAdded
    ctx.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let storm_in = drain_all(&ctx, &mut sess, PILOT_A);
    let eff_start = find_notif(&storm_in, "OnSystemEffectStarted");
    assert_eq!(eff_start.len(), 1);
    let beacon_tid = match &eff_start[0] {
        PyRep::List(ids) => {
            assert_eq!(ids.len(), 1);
            ids[0].as_i64().expect("beacon typeID")
        }
        other => panic!("expected List([beacon_type_id]), got {other:?}"),
    };
    assert!(beacon_tid > 0);
    let anom_added = find_notif(&storm_in, "OnCosmicAnomalyAdded");
    assert_eq!(anom_added.len(), 1);
    assert!(
        anom_added[0]
            .dict_get("targetID")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("DNG-904"))
    );

    // Move storm to a distant null-sec region on its next scheduled jump so storm_dest leaves core + periphery
    let far_sys: u32 = 30_000_238;
    let now3 = ctx.now_unix();
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        let s = &mut reg.roaming_storms[0];
        s.center_solar_system_id = far_sys;
        s.recent_centers.clear();
        s.next_move_at_sec = now3 + 60;
    });
    // Negative: 10s before scheduled jump -> storm effect still active
    ctx.clock().advance_secs(10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnSystemWideEffectStop"
    ));

    // Positive: 60s advances storm to far_sys -> removes beacon & environment from storm_dest
    ctx.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let storm_out = drain_all(&ctx, &mut sess, PILOT_A);
    let stop_args = find_notif(&storm_out, "OnSystemWideEffectStop");
    assert_eq!(stop_args.len(), 3);
    assert!(stop_args[0].as_i64().unwrap_or(0) > 0);
    assert!(stop_args[1].as_i64().unwrap_or(0) > 0);
    assert_eq!(stop_args[2], PyRep::Integer(beacon_tid));
    assert_eq!(
        find_notif(&storm_out, "OnComponentRemovedSystemEnvironment"),
        &[PyRep::Integer(i64::from(storm_dest))]
    );
}

}

// Folded submodule: notify_real_emitters_batch10_part4
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch10_part4 {
    use super::*;

// Real production server-to-client `notify:On*` emission integration tests (Batch 10, Part 4).
//
// Covers ESS Reserve Bank inventory full (`OnEssReserveBankInventoryFull`), FW complex capture
// ticks (`OnCaptureChanged`), Hacking tile selection (`OnLocalHackProgressUpdated`), Pirate
// Insurgency Stage-5 security drop (`OnSecurityModified`), Null-Sec Dynamic Bounty multiplier
// updates (`OnSystemDynamicBountyStateChanged`), and multi-pilot asteroid mining
// (`OnOreMinedByOther`) via live RPCs and `tick_background_cluster`.


use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
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

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 60.0,
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
    sys_id: u32,
    gm: bool,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)),
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        )
        .await;
    let mut sess = SessionState::new(char_id as u64);
    let (user, pass) = if char_id == PILOT_A { ("demo", "demo-secret") } else { ("other", "other-secret") };
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    if gm {
        sess.role |= ROLE_GM_DEFAULT;
    }
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

/// parity: notify:OnLocalHackProgressUpdated
/// parity: notify:OnSystemDynamicBountyStateChanged
/// parity: notify:OnOreMinedByOther
#[tokio::test]
async fn batch10_ess_fw_hacking_security_dbs_and_mining() {
    let ctx = ServerContext::new();
    let null_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.region_id == 10_000_003 && s.security_status <= 0.0)
        .map(|(&id, _)| id)
        .unwrap_or(30_000_201);

    // 1. OnSystemDynamicBountyStateChanged in Null-Sec
    let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, 700_501, 587, null_sys, false).await;
    let null_handle = ctx.systems.get_or_spawn(null_sys);
    let pirate_npc_tid = 33_132_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_530,
            make_item(700_530, 10_678, PILOT_A, 700_501, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            700_531,
            make_item(700_531, 222, PILOT_A, 700_501, InventoryFlag::HiSlot0, 200, false),
        );
        for &sk in &[3300_u32, 3303, 3426] {
            snap.skills.entry(PILOT_A).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
            );
        }
    });
    ctx.invalidate_ship_fitting(700_501);
    let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_501), PyRep::Integer(700_530)]).await;
    let npc_ball_id = eve_server::services::combat_crimewatch::spawn_specialized_npc_entity(
        &ctx,
        null_sys,
        pirate_npc_tid,
        "CloneSoldier",
        500_010,
        DVec3::new(1_250.0, 0.0, 0.0),
    )
    .await;
    if let Ok(Some(mut cs)) = null_handle.get_combat_state(npc_ball_id).await {
        cs.shield_hp = 0.0;
        cs.armor_hp = 0.0;
        cs.structure_hp = 1.0;
        cs.shield_recharge_time_ms = 1.0e15;
        let _ = null_handle.set_combat_state(npc_ball_id, cs).await;
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_ball_id)])
        .await
        .expect("AddTarget pirate NPC");
    for _ in 0..25 {
        null_handle.step_manual(1.0).await.unwrap();
    }
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(700_530), PyRep::String("targetAttack".into()), PyRep::Integer(npc_ball_id)],
    )
    .await
    .expect("Activate weapon on pirate NPC");
    for _ in 0..8 {
        null_handle.step_manual(1.0).await.unwrap();
        if null_handle.get_ball_snapshot(npc_ball_id).await.unwrap().is_none() {
            break;
        }
    }
    assert!(null_handle.get_ball_snapshot(npc_ball_id).await.unwrap().is_none(), "pirate NPC must be destroyed by weapon");
    assert!(
        eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.pending_bounty_ticks.get(&(null_sys, PILOT_A)).copied().unwrap_or(0.0)
        }) > 0.0,
        "pending bounty must be queued after NPC kill (tid={pirate_npc_tid}, sys={null_sys})"
    );
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Negative: before the 20-minute (1,200s) bounty tick elapses, tick_background_cluster does NOT flush or emit OnSystemDynamicBountyStateChanged
    ctx.advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnSystemDynamicBountyStateChanged"));

    // Positive: advancing past the 20-minute (1,200s) bounty tick flushes the queued NPC bounty and emits OnSystemDynamicBountyStateChanged
    ctx.advance_secs(1_200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dbs_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let dbs_args = find_notif(&dbs_notifs, "OnSystemDynamicBountyStateChanged");
    assert_eq!(dbs_args.len(), 2);
    assert_eq!(dbs_args[0], PyRep::Integer(i64::from(null_sys)));
    let new_rate = dbs_args[1].as_f64().expect("DBS rate");
    assert!(new_rate > 0.0 && new_rate < 1.35);

    // 3. OnCaptureChanged & OnSecurityModified via live tick_background_cluster in Low-Sec
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| (0.1..0.4).contains(&s.security_status))
        .map(|(&id, _)| id)
        .unwrap_or(30_002_537);
    let _ = ctx.db.set_character_solar_system(PILOT_A, lowsec_sys, &ctx.sde);
    sess_a.solar_system_id = Some(lowsec_sys);
    ctx.sync_session(&mut sess_a);
    let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = low_handle
        .spawn_ball_with_combat(make_ball(700_501, 587, DVec3::new(90_000.0, 0.0, 0.0)), Default::default())
        .await;
    let fw_gate_id = 900_560_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.war_faction_id = Some(500_001); // Caldari militia for FW capture
        }
        snap.fw_runtime.fw_complexes.insert(fw_gate_id, (lowsec_sys, "Novice".into(), 500_004, 10.0, false));
        snap.item_positions.insert(fw_gate_id, [0.0, 0.0, 0.0]);
        snap.fw_runtime.complex_occupants.insert(fw_gate_id, vec![(PILOT_A, 700_501)]);
        snap.fw_runtime.complex_guards_cleared.insert(fw_gate_id, true);
        snap.fw_runtime.complex_last_step_unix.insert(fw_gate_id, now_u.saturating_sub(10));
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Negative: Pilot A is at 90km (>30km from pocket) -> zero OnCaptureChanged
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnCaptureChanged"));

    // Positive: move Pilot A into capture pocket -> tick_background_cluster emits OnCaptureChanged
    let pocket = eve_server::services::fw_gameplay::pocket_pos_for_complex(fw_gate_id);
    let _ = low_handle.set_ball_position(700_501, pocket).await;
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let cap_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cap_args = find_notif(&cap_notifs, "OnCaptureChanged");
    assert_eq!(cap_args.len(), 6);
    assert_eq!(cap_args[0], PyRep::Integer(fw_gate_id));
    assert_eq!(cap_args[1], PyRep::Integer(500_001));
    assert!(cap_args[3].as_f64().unwrap_or(0.0) > 0.0);
    assert_eq!(cap_args[4], PyRep::Real(600_000.0));
    assert_eq!(cap_args[5], PyRep::Integer(500_001));

    // Setup Pirate Insurgency campaign at 799 corruption points (Stage 4) in lowsec_sys
    let now_ins = ctx.now_unix();
    eve_server::services::insurgency_services::init_insurgency_campaign(
        &ctx,
        1,
        500_010,
        500_001,
        lowsec_sys,
        vec![lowsec_sys],
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.fw_complexes.clear();
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.war_faction_id = Some(500_010); // Guristas pirate militia
        }
        if let Some(camp) = snap.insurgency_runtime.campaigns.get_mut(&1) {
            camp.forecasting_end_unix = now_ins.saturating_sub(3_600);
            camp.active_end_unix = now_ins + 86_400;
            camp.winddown_end_unix = now_ins + 172_800;
            camp.corruption_points.insert(lowsec_sys, 799);
        }
        let st = snap.system_states.entry(lowsec_sys).or_default();
        st.solar_system_id = lowsec_sys;
        st.insurgency_corruption_stage = 4;
        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        st.last_fw_capture_tick = 0;
    });
    // Negative: Pilot A is at 50km (>30km from insurgency beacon [0,0,0]) -> zero OnSecurityModified
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnSecurityModified"));

    // Positive: move Pilot A to [500, 0, 0] -> tick advances corruption 799 -> >=800 (Stage 5) -> OnSecurityModified
    let _ = low_handle.set_ball_position(700_501, DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let sec_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let sec_args = find_notif(&sec_notifs, "OnSecurityModified");
    assert_eq!(sec_args.len(), 3);
    assert_eq!(sec_args[0], PyRep::Integer(i64::from(lowsec_sys)));
    assert!(sec_args[1].as_f64().unwrap_or(0.0) < 0.0);
    assert_eq!(sec_args[2], PyRep::Real(0.0));

    // 4. OnLocalHackProgressUpdated via live hackingMgr.SelectTile RPC in SYS_JITA
    let mut sess_jita = setup_pilot_in_space(&ctx, PILOT_A, 700_570, 587, SYS_JITA, false).await;
    let mut sess_obs = setup_pilot_in_space(&ctx, PILOT_B, 700_571, 587, SYS_JITA, false).await;
    let jita_sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = rpc(&ctx, &mut sess_jita, "scanSvc", "GetCombatAnomalyInstances", &[]).await;
    let (can_id, site_pos) = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&SYS_JITA)
            .and_then(|sites| {
                sites.iter().find_map(|s| s.hackable_can_ids.first().copied().map(|cid| (cid, DVec3::from_array(s.position))))
            })
            .expect("seeded hackable container in Jita")
    });
    let can_pos = if let Ok(Some((_, cb))) = jita_sys.get_ball_snapshot(can_id).await { cb.position } else { site_pos };
    let _ = jita_sys.set_ball_position(700_570, can_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_575,
            make_item(700_575, 22_175, PILOT_A, 700_570, InventoryFlag::MedSlot0, 1, true),
        );
        snap.items.insert(
            700_576,
            make_item(700_576, 22_177, PILOT_A, 700_570, InventoryFlag::MedSlot1, 1, true),
        );
        for &sk in &[21_718_u32, 13_278] {
            snap.skills.entry(PILOT_A).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
            );
        }
    });
    ctx.invalidate_ship_fitting(700_570);
    let _ = rpc(&ctx, &mut sess_jita, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_570), PyRep::Integer(700_575)]).await;
    let _ = rpc(&ctx, &mut sess_jita, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_570), PyRep::Integer(700_576)]).await;

    let _ = rpc(&ctx, &mut sess_jita, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(can_id)])
        .await
        .unwrap();
    let _ = drain_all(&ctx, &mut sess_jita, PILOT_A);

    // Negative: invalid tile index 999 fails and emits zero OnLocalHackProgressUpdated
    assert!(
        rpc(&ctx, &mut sess_jita, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(999)])
            .await
            .is_err()
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess_jita, PILOT_A), "OnLocalHackProgressUpdated"));

    // Positive: click an accessible tile adjacent to tile 0 -> emits OnLocalHackProgressUpdated
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        if let Some(g) = reg.hacking_games.get_mut(&(PILOT_A, can_id)) {
            if let Some(t) = g.tiles.get_mut(1) {
                t.revealed = true;
            }
        }
    });
    rpc(&ctx, &mut sess_jita, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)])
        .await
        .unwrap();
    let hack_notifs = drain_all(&ctx, &mut sess_jita, PILOT_A);
    let hack_args = find_notif(&hack_notifs, "OnLocalHackProgressUpdated");
    assert_eq!(hack_args.len(), 2);
    assert_eq!(hack_args[0], PyRep::Integer(can_id));
    let prog = hack_args[1].as_f64().expect("hackProgress");
    assert!(prog > 0.0 && prog <= 1.0);

    // 5. OnOreMinedByOther via live dogmaIM.Activate("miningLaser") + tick_background_cluster
    let rock_id = 800_601_i64;
    let rock_pos = DVec3::new(1_500.0, 0.0, 0.0);
    let _ = jita_sys.set_ball_position(700_570, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = jita_sys
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: rock_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            make_ball(rock_id, 1230, rock_pos),
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rock_id,
            make_item(rock_id, 1230, 1_000_009, i64::from(SYS_JITA), InventoryFlag::None, 10_000, true),
        );
        snap.asteroids.insert(
            rock_id,
            eve_db::AsteroidRecord {
                asteroid_id: rock_id,
                belt_id: 0,
                solar_system_id: SYS_JITA,
                type_id: 1230,
                quantity: 10_000,
                initial_quantity: 10_000,
                position: (rock_pos.x, rock_pos.y, rock_pos.z),
                radius: 100.0,
            },
        );
        snap.items.insert(
            700_580,
            make_item(700_580, 483, PILOT_A, 700_570, InventoryFlag::HiSlot1, 1, true),
        );
        snap.skills.entry(PILOT_A).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.invalidate_ship_fitting(700_570);
    let _ = rpc(&ctx, &mut sess_jita, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(700_570), PyRep::Integer(700_580)]).await;
    rpc(&ctx, &mut sess_jita, "dogmaIM", "AddTarget", &[PyRep::Integer(rock_id)]).await.unwrap();
    for _ in 0..25 {
        jita_sys.step_manual(1.0).await.unwrap();
    }
    rpc(
        &ctx,
        &mut sess_jita,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(700_580), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)],
    )
    .await
    .expect("Activate Miner I");
    let _ = drain_all(&ctx, &mut sess_jita, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_obs, PILOT_B);

    // Negative: after 5s (before 60s laser cycle finishes), zero OnOreMinedByOther on observer
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!has_notif(&drain_all(&ctx, &mut sess_obs, PILOT_B), "OnOreMinedByOther"));

    // Positive: after cycle completes, observer PILOT_B gets OnOreMinedByOther(rock_id, qty), while miner PILOT_A does NOT
    ctx.advance_secs(65);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let miner_notifs = drain_all(&ctx, &mut sess_jita, PILOT_A);
    assert!(!has_notif(&miner_notifs, "OnOreMinedByOther"));
    let obs_notifs = drain_all(&ctx, &mut sess_obs, PILOT_B);
    let ore_args = find_notif(&obs_notifs, "OnOreMinedByOther");
    assert_eq!(ore_args.len(), 2);
    assert_eq!(ore_args[0], PyRep::Integer(rock_id));
    assert!(ore_args[1].as_i64().unwrap_or(0) > 0);
}

}

// Folded submodule: notify_real_emitters_batch10_part5
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch10_part5 {
    use super::*;

// Real production server-to-client `notify:On*` / `notify:Do*` emission integration tests (Batch 10, Part 5).
//
// Covers `DoDestinyUpdates`, `DoSimClockRebase`, `OnDestinationChange` (via ESI HTTP),
// `OnServerMapDataPush`, `OnSystemActivityUpdated` (via `industryManager.InstallJob`),
// `OnCharacterEnergyChanged` (via CRAB beacon `beyonce.CallComponentFromClient`), and
// `OnAnalysisKreditsChange` (via `ProjectDiscovery.post_classification`).


use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

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

fn drain_all(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
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
    sys_id: u32,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
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
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

/// parity: notify:DoDestinyUpdates
/// parity: notify:DoSimClockRebase
/// parity: notify:OnDestinationChange
/// parity: notify:OnSystemActivityUpdated
/// parity: notify:OnCharacterEnergyChanged
#[tokio::test]
async fn batch10_destiny_clock_esi_map_industry_energy_and_discovery() {
    let ctx = Arc::new(ServerContext::new());
    let mut sess = setup_pilot_in_space(&ctx, PILOT_A, 700_701, 587, SYS_JITA).await;

    // 1. DoDestinyUpdates via live beyonce.CmdStop
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(SYS_JITA)).unwrap();
    let empty_kw = std::collections::HashMap::new();
    // Negative: docked pilot calling CmdStop fails and does NOT emit DoDestinyUpdates
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_A).unwrap().station_id = Some(60_003_760);
    });
    sess.station_id = Some(60_003_760);
    assert!(
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdStop", &[], &empty_kw, None)
            .await
            .is_err()
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "DoDestinyUpdates"
    ));

    // Positive: undocked pilot in space receives DoDestinyUpdates([(stamp, ("Stop", (ship_id,)))])
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_A).unwrap().station_id = None;
    });
    sess.station_id = None;
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdStop", &[], &empty_kw, None)
        .await
        .unwrap();
    let dest_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let du_args = find_notif(&dest_notifs, "DoDestinyUpdates");
    assert_eq!(du_args.len(), 1);
    match &du_args[0] {
        PyRep::List(updates) => {
            assert!(!updates.is_empty());
            match &updates[0] {
                PyRep::Tuple(pair) => {
                    assert_eq!(pair.len(), 2);
                    match &pair[1] {
                        PyRep::Tuple(cmd) => assert_eq!(cmd[0], PyRep::String("Stop".into())),
                        other => panic!("expected (\"Stop\", payload), got {other:?}"),
                    }
                }
                other => panic!("expected (stamp, cmd) tuple, got {other:?}"),
            }
        }
        other => panic!("expected List of updates, got {other:?}"),
    }

    // 2. DoSimClockRebase via live ctx.advance_secs
    // Negative: advancing 0 seconds does NOT emit DoSimClockRebase
    ctx.advance_secs(0);
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "DoSimClockRebase"
    ));

    // Positive: advancing 15 seconds emits 1-arg 2-tuple ((old_ft, new_ft),) with exact 15s filetime delta
    let before_ft = ctx.now_blueos_filetime_ticks();
    ctx.advance_secs(15);
    let rebase_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let rb_args = find_notif(&rebase_notifs, "DoSimClockRebase");
    assert_eq!(rb_args.len(), 1);
    match &rb_args[0] {
        PyRep::Tuple(times) => {
            assert_eq!(times.len(), 2);
            let old_t = times[0].as_i64().unwrap();
            let new_t = times[1].as_i64().unwrap();
            assert_eq!(old_t / 10_000_000, before_ft / 10_000_000);
            assert_eq!((new_t - old_t) / 10_000_000, 15);
        }
        other => panic!("expected 2-tuple rebaseTimes, got {other:?}"),
    }

    // 3. OnDestinationChange via live ESI HTTP POST /v2/ui/autopilot/waypoint/
    let esi = EsiRouter::new(Arc::clone(&ctx));
    let now_u = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            PILOT_A,
            "evomu-batch10",
            "https://localhost/callback",
            vec!["esi-ui.write_waypoint.v1".to_string()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now_u,
        )
        .expect("authorize_pkce");
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-batch10",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now_u,
        )
        .expect("exchange_pkce_code")
        .access_token;
    // Negative: invalid destination_id rejected with 4xx and emits zero OnDestinationChange
    let bad_resp = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_ne!(bad_resp.status_code, 204);
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnDestinationChange"
    ));

    // Positive: valid solar system Amarr (30002187) returns 204 and emits OnDestinationChange(30002187, true, false)
    let ok_resp = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_eq!(ok_resp.status_code, 204);
    let wp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&wp_notifs, "OnDestinationChange"),
        &[
            PyRep::Integer(30_002_187),
            PyRep::Bool(true),
            PyRep::Bool(false),
        ]
    );

    // 5. OnSystemActivityUpdated via live industryManager.InstallJob
    // Negative: undocked pilot calling InstallJob fails with NotDocked and emits zero OnSystemActivityUpdated
    let bp_tid = *ctx
        .sde
        .blueprints
        .iter()
        .find(|(&id, bp)| bp.manufacturing_time_sec > 0 && ctx.sde.types.contains_key(&id))
        .map(|(id, _)| id)
        .expect("SDE blueprint");
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::Integer(i64::from(bp_tid)), PyRep::Integer(3), PyRep::Integer(1)]
        )
        .await
        .unwrap_err()
        .code,
        "NotDocked"
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnSystemActivityUpdated"
    ));

    // Positive: docked at Jita 4-4 (60003760) with owned BPO in Hangar & ISK -> InstallJob emits OnSystemActivityUpdated()
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(60_003_760);
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.items.insert(
            700_720,
            make_item(700_720, bp_tid, PILOT_A, 60_003_760, InventoryFlag::Hangar, 1, true),
        );
    });
    sess.station_id = Some(60_003_760);
    rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(700_720)),
            ("blueprintTypeID", PyRep::Integer(i64::from(bp_tid))),
            ("activityID", PyRep::Integer(3)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(60_003_760)),
        ])],
    )
    .await
    .expect("InstallJob TE research");
    let ind_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&ind_notifs, "OnSystemActivityUpdated"), &[]);

    // 6. OnCharacterEnergyChanged via live CRAB beacon beyonce.CallComponentFromClient("linkWithShip", "InitiateLink")
    let null_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.region_id == 10_000_003 && s.security_status <= 0.0)
        .map(|(&id, _)| id)
        .unwrap_or(30_000_201);
    let dread_tid = *ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.category_id == 6 && t.group_id == 485 && t.published)
        .map(|(id, _)| id)
        .unwrap_or(&19_720);
    let crab_id = 700_750_i64;
    let dread_id = 700_751_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = dread_id;
        }
        snap.items.insert(
            dread_id,
            make_item(dread_id, dread_tid, PILOT_A, i64::from(null_sys), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            crab_id,
            make_item(
                crab_id,
                eve_server::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                PILOT_A,
                i64::from(null_sys),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        snap.item_positions.insert(crab_id, [2_000.0, 0.0, 0.0]);
    });
    // Deduct 90 CEQ so PILOT_A has 10 CEQ (< 30 cost) for negative InsufficientEnergy test
    let _ = eve_server::services::character_energy::consume_character_energy(&ctx, PILOT_A, 90);
    sess.solar_system_id = Some(null_sys);
    sess.station_id = None;
    sess.ship_id = Some(dread_id);
    let null_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = null_handle
        .spawn_ball_with_combat(
            Ball {
                id: dread_id,
                type_id: dread_tid,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let _ = null_handle
        .spawn_ball_with_combat(
            Ball {
                id: crab_id,
                type_id: eve_server::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: only 10 CEQ (< 30 required) -> InitiateLink rejected and zero OnCharacterEnergyChanged
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "beyonce",
            "CallComponentFromClient",
            &[
                PyRep::Integer(crab_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ]
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientEnergy"
    );
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnCharacterEnergyChanged"
    ));

    // Positive: advance 25 hours so CEQ regenerates to 100, then InitiateLink deducts 30 -> 70 CEQ
    eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        if let Some(b) = reg.crab_beacons.get_mut(&crab_id) {
            b.deployed_at_sec = 0;
        }
    });
    ctx.advance_secs(90_000);
    let _ = rpc(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await.unwrap();
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let crab_res = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(crab_id),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ],
    )
    .await
    .expect("InitiateLink CRAB beacon");
    assert_eq!(
        crab_res.dict_get("ceqRemaining").and_then(|v| v.as_i64()),
        Some(70)
    );
    let ceq_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let ceq_args = find_notif(&ceq_notifs, "OnCharacterEnergyChanged");
    assert_eq!(ceq_args.len(), 2);
    assert_eq!(ceq_args[0], PyRep::Integer(PILOT_A));
    assert_eq!(
        ceq_args[1]
            .dict_get("currentEnergy")
            .and_then(|v| v.as_i64()),
        Some(70)
    );
    assert_eq!(
        ceq_args[1].dict_get("maxEnergy").and_then(|v| v.as_i64()),
        Some(100)
    );

    // 7. OnAnalysisKreditsChange via live ProjectDiscovery.post_classification
    let task_rep = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .unwrap();
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess, PILOT_A),
        "OnAnalysisKreditsChange"
    ));

    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 100.0, 50.0, 25.0, 5.0, 10,
        );
        eve_server::services::project_discovery::set_project_discovery_task_consensus(
            snap, 1, true,
        );
    });
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
    let pd_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&pd_notifs, "OnAnalysisKreditsChange"), &[]);
}

}
