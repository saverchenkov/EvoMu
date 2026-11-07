//! Live fleet command and probe scanning suite driven only through client RPCs (`fleetObjectHandler`,
//! `beyonce`, `dogmaIM`, `fighterMgr`, `scanMgr`) plus GM slash commands for seeding, with real ticks:
//! - Fleet boss hand-over (MakeLeader, LeaveFleet, socket disconnect) and its notifications.
//! - Stored / per-warp fleet formations with oriented landing offsets and fleet target tagging.
//! - Probe lifecycle notifications with distance-derived warp timing and dogma-derived scan duration.

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::types::InventoryFlag;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::fleet_social::fleet_social_notify::handle_fleet_member_disconnect;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const HIGHSEC: u32 = 30_000_144; // Perimeter (0.9)
const AU_M: f64 = 149_597_870_700.0;
const ARCHON: u32 = 23757;
const VEXOR: u32 = 626;
const EQUITE: u32 = 40358;
const RIFTER: u32 = 587;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

fn pt(p: DVec3) -> PyRep {
    PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])
}

fn int(v: i64) -> PyRep {
    PyRep::Integer(v)
}

async fn call_kw(
    ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep], kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, None, method, args, kw, None).await
}

async fn call(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    call_kw(ctx, p, svc, method, args, &HashMap::new()).await
}

async fn ok(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    call(ctx, p, svc, method, args).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

fn drain(ctx: &ServerContext, p: &mut LivePilot) -> Vec<(String, Vec<PyRep>)> {
    let mut out = p.sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(p.char_id) {
        if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) {
            out.push((n.event, n.args));
        }
    }
    out
}

fn boss(ctx: &ServerContext, fid: i64) -> i64 {
    ctx.db.with_snapshot(|s| s.fleets.get(&fid).expect("fleet").boss_id)
}

async fn create_fleet(ctx: &ServerContext, fc: &mut LivePilot) -> i64 {
    let fid = ok(ctx, fc, "fleetObjectHandler", "CreateFleet", &[]).await.as_i64().expect("fleetID");
    fc.sess.fleet_id = Some(fid);
    fid
}

async fn join(ctx: &ServerContext, fc: &mut LivePilot, p: &mut LivePilot, fid: i64, wing: PyRep, squad: PyRep, role: i64) {
    ok(ctx, fc, "fleetObjectHandler", "Invite", &[int(p.char_id), wing, squad, int(role)]).await;
    ok(ctx, p, "fleetObjectHandler", "AcceptInvite", &[int(fid)]).await;
    p.sess.fleet_id = Some(fid);
}

async fn pos(ctx: &ServerContext, id: i64) -> DVec3 {
    ctx.systems.get_or_spawn(HIGHSEC).get_ball_snapshot(id).await.unwrap().expect("ball").1.position
}

async fn step(ctx: &ServerContext, secs: usize) {
    let sys = ctx.systems.get_or_spawn(HIGHSEC);
    for _ in 0..secs {
        sys.step_manual(1.0).await.unwrap();
    }
}

#[tokio::test]
async fn test_live_fleet_boss_handover_via_make_leader_leave_and_disconnect() {
    let ctx = paused_ctx();
    let mut p1 = common::gm_pilot_in_space(&ctx, 21, 91_200_001, VEXOR, HIGHSEC).await;
    let mut p2 = common::gm_pilot_in_space(&ctx, 22, 91_200_002, VEXOR, HIGHSEC).await;
    let mut p3 = common::gm_pilot_in_space(&ctx, 23, 91_200_003, VEXOR, HIGHSEC).await;
    let mut p4 = common::gm_pilot_in_space(&ctx, 24, 91_200_004, VEXOR, HIGHSEC).await;

    let fid = create_fleet(&ctx, &mut p1).await;
    let wing = ok(&ctx, &mut p1, "fleetObjectHandler", "CreateWing", &[]).await.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
    let sq1 = ok(&ctx, &mut p1, "fleetObjectHandler", "CreateSquad", &[int(wing)]).await.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
    let sq2 = ok(&ctx, &mut p1, "fleetObjectHandler", "CreateSquad", &[int(wing)]).await.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
    join(&ctx, &mut p1, &mut p2, fid, int(wing), PyRep::None, 2).await;
    join(&ctx, &mut p1, &mut p3, fid, int(wing), int(sq1), 3).await;
    join(&ctx, &mut p1, &mut p4, fid, int(wing), int(sq2), 4).await;
    let _ = drain(&ctx, &mut p3);

    // MakeLeader both ways, notifying the rest of the fleet.
    ok(&ctx, &mut p1, "fleetObjectHandler", "MakeLeader", &[int(p2.char_id)]).await;
    assert_eq!(boss(&ctx, fid), p2.char_id);
    assert!(drain(&ctx, &mut p3).iter().any(|(ev, _)| ev == "OnFleetMemberChanged"));
    ok(&ctx, &mut p2, "fleetObjectHandler", "MakeLeader", &[int(p1.char_id)]).await;
    assert_eq!(boss(&ctx, fid), p1.char_id);

    // A disconnected member cannot receive the boss role.
    let p2_id = p2.char_id;
    handle_fleet_member_disconnect(&ctx, &mut p2.sess);
    let offline = call(&ctx, &mut p1, "fleetObjectHandler", "MakeLeader", &[int(p2_id)]).await.expect_err("offline target");
    assert_eq!(offline.code, "TargetMemberOffline");

    // Boss leaves: the disconnected wing commander is skipped, the connected squad commander takes over.
    let _ = drain(&ctx, &mut p3);
    ok(&ctx, &mut p1, "fleetObjectHandler", "LeaveFleet", &[]).await;
    assert_eq!(boss(&ctx, fid), p3.char_id, "LeaveFleet must skip disconnected WC and promote connected SC");
    assert!(drain(&ctx, &mut p3).iter().any(|(ev, _)| ev == "OnFleetMemberChanged"));

    // Boss socket drop: immediate automatic hand-over to the remaining connected member.
    let _ = drain(&ctx, &mut p4);
    handle_fleet_member_disconnect(&ctx, &mut p3.sess);
    assert_eq!(boss(&ctx, fid), p4.char_id, "boss disconnect must promote the connected member");
    assert!(drain(&ctx, &mut p4).iter().any(|(ev, a)| ev == "OnChangeFleetBoss" && a.get(1) == Some(&int(p4.char_id))));

    // Disconnect grace expires on the lifecycle tick and drops the offline members from the fleet.
    ctx.advance_secs(130);
    let members = ctx.db.with_snapshot(|s| s.fleets.get(&fid).map(|f| f.members.clone()).unwrap_or_default());
    assert!(!members.contains(&p2_id) && members.contains(&p4.char_id), "offline members expire after the 120 s grace: {members:?}");
}

#[tokio::test]
async fn test_live_fleet_formation_warps_and_target_tagging() {
    let ctx = paused_ctx();
    let mut fc = common::gm_pilot_in_space(&ctx, 31, 91_200_011, ARCHON, HIGHSEC).await;
    let mut p2 = common::gm_pilot_in_space(&ctx, 32, 91_200_012, VEXOR, HIGHSEC).await;
    let mut p3 = common::gm_pilot_in_space(&ctx, 33, 91_200_013, VEXOR, HIGHSEC).await;
    let mut p4 = common::gm_pilot_in_space(&ctx, 34, 91_200_014, VEXOR, HIGHSEC).await;
    let high_sys = ctx.systems.get_or_spawn(HIGHSEC);
    let fc_start = pos(&ctx, fc.ship_id).await;
    let _ = high_sys.set_ball_position(p2.ship_id, DVec3::new(fc_start.x + 5_000.0, fc_start.y, fc_start.z)).await;
    let _ = high_sys.set_ball_position(p3.ship_id, DVec3::new(fc_start.x - 5_000.0, fc_start.y, fc_start.z)).await;
    let start = DVec3::new(fc_start.x, fc_start.y + 15_000.0, fc_start.z);
    let _ = high_sys.set_ball_position(p4.ship_id, start).await;

    let PyRep::Tuple(formations) = ok(&ctx, &mut fc, "beyonce", "GetFormations", &[]).await else { panic!("formations") };
    assert_eq!(formations.len(), 2, "GetFormations keeps the native _destiny LoadFormations 2-tuple (Diamond, Arrow)");
    let fid = create_fleet(&ctx, &mut fc).await;
    for p in [&mut p2, &mut p3, &mut p4] {
        join(&ctx, &mut fc, p, fid, PyRep::None, PyRep::None, 4).await;
    }
    ok(&ctx, &mut p4, "fleetObjectHandler", "SetTakesFleetWarp", &[PyRep::Bool(false)]).await;

    // 1. Stored formation settings drive a fleet warp along +Z: wall spreads laterally (X/Y), not along Z.
    let stored = ok(&ctx, &mut fc, "fleetObjectHandler", "SetFleetFormationSettings", &[int(1), PyRep::Real(8_000.0), PyRep::Real(20_000.0)]).await;
    assert_eq!(stored.dict_get("formationName"), Some(&PyRep::String("Plane".into())));
    assert_eq!(ok(&ctx, &mut fc, "fleetObjectHandler", "GetFormationSettings", &[]).await.dict_get("formationID"), Some(&int(1)));
    let wall_dest = [fc_start.x, fc_start.y, fc_start.z + 500_000_000.0];
    let wall = ok(&ctx, &mut fc, "fleetObjectHandler", "FleetWarp", &[PyRep::Real(wall_dest[0]), PyRep::Real(wall_dest[1]), PyRep::Real(wall_dest[2]), PyRep::Real(0.0)]).await;
    assert_eq!(wall.dict_get("skippedOptOutMemberIDs"), Some(&PyRep::List(vec![int(p4.char_id)])));
    step(&ctx, 180).await;
    let (w1, w2, w3) = (pos(&ctx, fc.ship_id).await, pos(&ctx, p2.ship_id).await, pos(&ctx, p3.ship_id).await);
    let p4_now = pos(&ctx, p4.ship_id).await;
    assert!(p4_now.distance(start) < 50_000.0 && w1.distance(fc_start) > 400_000_000.0, "opted-out member must stay behind: {p4_now:?} vs FC {w1:?}");
    assert!(w1.distance(w2) >= 2_500.0 && w1.distance(w2) <= 25_000.0 && w2.distance(w3) >= 2_500.0, "wall spacing {w1:?} {w2:?} {w3:?}");
    assert!((w2.z - w3.z).abs() < 500.0 && (w2.z - w1.z).abs() < 2_500.0, "wall along +Z warp must spread laterally in X/Y");
    assert!(DVec3::new(w2.x - w1.x, w2.y - w1.y, 0.0).length() >= 2_500.0, "wall along +Z warp must spread laterally in X/Y");

    // 2. Per-warp Arrow override (kwargs) along +X: wingmen trail the FC and split to opposite sides.
    let arrow_kw = HashMap::from([("formationID".into(), int(3)), ("formationSpacing".into(), PyRep::Real(6_000.0))]);
    let arrow_args = [PyRep::Real(w1.x + 500_000_000.0), PyRep::Real(w1.y), PyRep::Real(w1.z), PyRep::Real(0.0)];
    call_kw(&ctx, &mut fc, "fleetObjectHandler", "FleetWarp", &arrow_args, &arrow_kw).await.expect("FleetWarp Arrow");
    step(&ctx, 180).await;
    let (a1, a2, a3) = (pos(&ctx, fc.ship_id).await, pos(&ctx, p2.ship_id).await, pos(&ctx, p3.ship_id).await);
    assert!(a2.x < a1.x - 4_000.0 && a3.x < a1.x - 4_000.0, "arrow wingmen trail the FC along the warp axis");
    assert!(a2.distance(a3) > 8_000.0, "arrow wingmen split across opposite sides");

    // 3. Target tagging: permissions, tag alphabet, on-grid + completed lock, re-tag/untag, auto-clear on kill.
    let npc = |r: PyRep| r.dict_get("npcID").and_then(|v| v.as_i64()).expect("npcID");
    let t1 = npc(common::slash_cmd(&ctx, &mut fc.sess, &format!("/spawnnpc {RIFTER}")).await);
    let t2 = npc(common::slash_cmd(&ctx, &mut fc.sess, &format!("/spawnnpc {RIFTER}")).await);
    let offgrid = npc(common::slash_cmd(&ctx, &mut p4.sess, &format!("/spawnnpc {RIFTER}")).await);
    let tag = |id: i64, t: &str| [int(id), PyRep::String(t.into())];
    let code = |r: Result<PyRep, BridgeRpcError>| r.expect_err("tag must fail").code;

    assert_eq!(code(call(&ctx, &mut p2, "beyonce", "CmdFleetTagTarget", &tag(t1, "A")).await), "FleetAccessDenied");
    for bad in ["0", "AA", "*"] {
        assert_eq!(code(call(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t1, bad)).await), "InvalidTargetTag");
    }
    assert_eq!(code(call(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(offgrid, "A")).await), "TargetNotOnGrid");
    assert_eq!(code(call(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t1, "A")).await), "TargetNotLocked");
    let fc_ship = fc.ship_id;
    ok(&ctx, &mut fc, "dogmaIM", "AddTargetOBO", &[int(fc_ship), int(t1)]).await;
    ok(&ctx, &mut fc, "dogmaIM", "AddTargetOBO", &[int(fc_ship), int(t2)]).await;
    assert_eq!(code(call(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t1, "A")).await), "TargetNotLocked");
    step(&ctx, 60).await;

    let _ = drain(&ctx, &mut p2);
    ok(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t1, "A")).await;
    assert!(drain(&ctx, &mut p2).iter().any(|(ev, _)| ev == "OnFleetStateChange"));
    ok(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t2, "A")).await;
    ok(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t1, "1")).await;
    ok(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &tag(t2, "")).await;
    let state = ok(&ctx, &mut fc, "beyonce", "GetFleetState", &[]).await;
    assert_eq!(state.dict_get("targetTags"), Some(&PyRep::Dict(vec![(int(t1), PyRep::String("1".into()))])));

    // Destroy tagged t1 with live Equite volleys: tag auto-clears and the fleet is notified.
    let eq_stack = common::stock_ship_bay(&ctx, &mut fc, EQUITE, 12, InventoryFlag::FighterBay as i64).await;
    ok(&ctx, &mut fc, "fighterMgr", "LoadFightersToTube", &[int(0), int(eq_stack)]).await;
    let PyRep::List(ids) = ok(&ctx, &mut fc, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![int(0)])]).await else { panic!("launch") };
    let equite = ids[0].as_i64().unwrap();
    let sys = ctx.systems.get_or_spawn(HIGHSEC);
    let _ = drain(&ctx, &mut p2);
    for _ in 0..20 {
        if sys.get_ball_snapshot(t1).await.unwrap().is_none() {
            break;
        }
        ok(&ctx, &mut fc, "fighterMgr", "CmdActivateAbilitySlots", &[int(equite), int(2), int(t1)]).await;
        ctx.advance_secs(12);
    }
    assert!(sys.get_ball_snapshot(t1).await.unwrap().is_none(), "tagged target must be destroyed in space");
    assert!(drain(&ctx, &mut p2).iter().any(|(ev, _)| ev == "OnFleetStateChange"));
    assert_eq!(ok(&ctx, &mut fc, "beyonce", "GetFleetState", &[]).await.dict_get("targetTags"), Some(&PyRep::Dict(Vec::new())));
}

// parity: notify:OnProbeWarpEnd
#[tokio::test]
async fn test_live_probe_scanning_notifications_and_delayed_ball_arrival() {
    eve_testkit::attest(&[
        "notify:OnProbeWarpEnd",
    ]);
    let ctx = paused_ctx();
    let mut p = common::gm_pilot_in_space(&ctx, 41, 91_200_021, VEXOR, HIGHSEC).await;
    let ship = p.ship_id;
    let origin = pos(&ctx, ship).await;
    let fitted = common::slash_cmd(&ctx, &mut p.sess, &format!("/fit {ship} 17938 {}", InventoryFlag::HiSlot0 as i64)).await;
    let launcher = fitted.dict_get("itemID").and_then(|v| v.as_i64()).expect("launcher id");
    let probes = common::slash_cmd(&ctx, &mut p.sess, "/createitem 30013 8").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ok(&ctx, &mut p, "dogmaIM", "LoadAmmo", &[int(ship), PyRep::List(vec![int(launcher)]), PyRep::List(vec![int(probes)])]).await;

    // 1. Launch 8 Core Scanner Probes: 8 OnNewProbe (FILETIME expiry, distinct positions) + 8 Idle states.
    ok(&ctx, &mut p, "dogmaIM", "Activate", &[int(launcher), PyRep::String("launchProbes".into())]).await;
    let launch = drain(&ctx, &mut p);
    let new_probes: Vec<&Vec<PyRep>> = launch.iter().filter(|(ev, _)| ev == "OnNewProbe").map(|(_, a)| a).collect();
    assert_eq!(new_probes.len(), 8, "launching a full launcher emits 8 OnNewProbe");
    let mut probe_ids = Vec::new();
    let mut distinct = HashSet::new();
    for args in &new_probes {
        let kv = &args[0];
        assert!(kv.dict_get("expiry").and_then(|v| v.as_i64()).unwrap_or(0) > 100_000_000_000_000_000, "expiry must be FILETIME");
        let Some(PyRep::Tuple(t)) = kv.dict_get("pos") else { panic!("pos tuple") };
        distinct.insert(t.iter().map(|c| c.as_f64().unwrap().round() as i64).collect::<Vec<_>>());
        probe_ids.push(kv.dict_get("probeID").and_then(|v| v.as_i64()).expect("probeID"));
    }
    probe_ids.sort_unstable();
    assert_eq!(distinct.len(), 8, "each probe has its own launch offset");
    assert_eq!(launch.iter().filter(|(ev, a)| ev == "OnProbeStateChanged" && a.get(1) == Some(&int(1))).count(), 8);

    // 2. Short (50 km) reposition: Moving, no warp, unbonused 10 s scan (SDE attr 73), no instant teleport.
    let (p0, p1) = (probe_ids[0], probe_ids[1]);
    let short_dest = origin + DVec3::X * 50_000.0;
    let req = |entries: &[(i64, DVec3)]| {
        PyRep::Dict(entries.iter().map(|&(id, d)| (int(id), PyRep::keyval([("pos", pt(d)), ("scanRange", PyRep::Real(4.0 * AU_M))]))).collect())
    };
    ok(&ctx, &mut p, "scanMgr", "RequestScans", &[req(&[(p0, short_dest)])]).await;
    let short = drain(&ctx, &mut p);
    assert!(short.iter().any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(2)]));
    assert!(!short.iter().any(|(ev, _)| ev == "OnProbeWarpStart"));
    let base_ms = short.iter().find(|(ev, _)| ev == "OnSystemScanStarted").and_then(|(_, a)| a.get(1)).and_then(|v| v.as_i64()).unwrap();
    assert_eq!(base_ms, 10_000, "unbonused Core Probe Launcher I scan duration equals SDE attr 73");
    assert!(pos(&ctx, p0).await.distance(short_dest) > 10_000.0);
    ctx.advance_secs(20);
    assert!(drain(&ctx, &mut p).iter().any(|(ev, _)| ev == "OnSystemScanStopped"));

    // 3. 5 AU / 15 AU warps with Astrometrics IV, Astrometric Acquisition IV and Scan Acquisition Array I.
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 3412 4").await;
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 25811 4").await;
    common::slash_cmd(&ctx, &mut p.sess, &format!("/fit {ship} 33176 {}", InventoryFlag::MedSlot0 as i64)).await;
    let (d5, d15) = (origin + DVec3::X * 5.0 * AU_M, origin + DVec3::Y * 15.0 * AU_M);
    ok(&ctx, &mut p, "scanMgr", "RequestScans", &[req(&[(p0, d5), (p1, d15)])]).await;
    let warp = drain(&ctx, &mut p);
    for id in [p0, p1] {
        assert!(warp.iter().any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(id), int(3)]));
    }
    let dur = |id: i64| warp.iter().find(|(ev, a)| ev == "OnProbeWarpStart" && a.first() == Some(&int(id))).and_then(|(_, a)| a.get(4)).and_then(|v| v.as_i64()).unwrap();
    assert!(dur(p1) > dur(p0) && dur(p0) > 0, "15 AU warp ({}ms) must exceed 5 AU warp ({}ms)", dur(p1), dur(p0));
    let bonused_ms = warp.iter().find(|(ev, _)| ev == "OnSystemScanStarted").and_then(|(_, a)| a.get(1)).and_then(|v| v.as_i64()).unwrap();
    assert_eq!(bonused_ms, 4_320, "Astrometrics IV x Acquisition IV x Scan Acquisition Array I must yield 4320 ms");
    assert!(!warp.iter().any(|(ev, _)| ev == "OnProbeWarpEnd" || ev == "OnSystemScanStopped"));
    assert!(pos(&ctx, p0).await.distance(d5) > AU_M, "warping probe must not teleport at t=0");

    // Warp lands after 3 s: OnProbeWarpEnd + Scanning(4) + ball at destination; scan still running.
    ctx.advance_secs(3);
    let landed = drain(&ctx, &mut p);
    for id in [p0, p1] {
        assert!(landed.iter().any(|(ev, a)| ev == "OnProbeWarpEnd" && a.as_slice() == [int(id)]));
    }
    assert!(landed.iter().any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(4)]));
    assert!(!landed.iter().any(|(ev, _)| ev == "OnSystemScanStopped"), "scan still active right after landing");
    assert!(pos(&ctx, p0).await.distance(d5) < 1.0, "probe ball arrives when OnProbeWarpEnd fires");

    // Scan timer completes: Idle(1) + OnSystemScanStopped(probes, results, absentTargets).
    ctx.advance_secs(6);
    let done = drain(&ctx, &mut p);
    assert!(done.iter().any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(1)]));
    let stopped = done.iter().find(|(ev, _)| ev == "OnSystemScanStopped").map(|(_, a)| a).expect("OnSystemScanStopped");
    assert_eq!(stopped.len(), 3, "OnSystemScanStopped carries (probes, results, absentTargets)");
}
