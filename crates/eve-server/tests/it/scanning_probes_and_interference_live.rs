//! Live integration tests for probe scanning notifications and solar system interference.
//!
//! Remediates:
//! - `notify:OnNewProbe`
//! - `notify:OnProbeStateChanged`
//! - `notify:OnProbeWarpStart`
//! - `notify:OnSystemScanStarted`
//! - `notify:OnSystemScanStopped`
//! - `notify:OnSolarsystemInterferenceChanged`

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::types::InventoryFlag;
use eve_destiny::DVec3;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_crab_beacons::{CRAB_INTERFERENCE_DELTA, TYPE_CRAB_BEACON};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const HIGHSEC: u32 = 30_000_144; // Perimeter (0.9)
const NULLSEC: u32 = 30_000_208; // Nullsec
const AU_M: f64 = 149_597_870_700.0;
const VEXOR: u32 = 626;
const ARCHON: u32 = 23757;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"),
    );
    let ctx =
        ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

fn int(v: i64) -> PyRep {
    PyRep::Integer(v)
}

fn pt(p: DVec3) -> PyRep {
    PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])
}

async fn rpc(
    ctx: &ServerContext,
    p: &mut LivePilot,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn ok(
    ctx: &ServerContext,
    p: &mut LivePilot,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    rpc(ctx, p, svc, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
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

async fn ball_pos(ctx: &ServerContext, sys_id: u32, id: i64) -> DVec3 {
    ctx.systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(id)
        .await
        .unwrap()
        .expect("ball")
        .1
        .position
}

// parity: notify:OnNewProbe, notify:OnProbeStateChanged, notify:OnProbeWarpStart
// parity: notify:OnSystemScanStarted, notify:OnSystemScanStopped
#[tokio::test]
async fn test_live_probe_launch_warp_scan_and_state_transitions() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnProbeStateChanged",
        "notify:OnProbeWarpStart",
        "notify:OnSystemScanStarted",
        "notify:OnSystemScanStopped",
    ]);

    let ctx = paused_ctx();
    let mut p = common::gm_pilot_in_space(&ctx, 61, 92_400_001, VEXOR, HIGHSEC).await;
    let ship = p.ship_id;
    let origin = ball_pos(&ctx, HIGHSEC, ship).await;

    // Seed cosmic signatures in HIGHSEC via GetSystemScanMgr
    let scan_mgr = ok(&ctx, &mut p, "scanMgr", "GetSystemScanMgr", &[]).await;
    assert!(
        matches!(scan_mgr.dict_get("signatures"), Some(PyRep::List(l)) if !l.is_empty()),
        "System must have cosmic signatures for scan results"
    );

    // Fit Core Probe Launcher I (17938) and load 8 Core Scanner Probe I (30013)
    let fitted = common::slash_cmd(
        &ctx,
        &mut p.sess,
        &format!("/fit {ship} 17938 {}", InventoryFlag::HiSlot0 as i64),
    )
    .await;
    let launcher = fitted
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("launcher itemID");
    let probes = common::slash_cmd(&ctx, &mut p.sess, "/createitem 30013 8")
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("probe stack itemID");
    ok(
        &ctx,
        &mut p,
        "dogmaIM",
        "LoadAmmo",
        &[
            int(ship),
            PyRep::List(vec![int(launcher)]),
            PyRep::List(vec![int(probes)]),
        ],
    )
    .await;
    let _ = drain(&ctx, &mut p);

    // 1. Launch probes -> OnNewProbe (BlueOS FILETIME expiry + 3-tuple pos) & OnProbeStateChanged(Idle=1)
    let before_launch_ft = ctx.now_blueos_filetime_ticks();
    ok(
        &ctx,
        &mut p,
        "dogmaIM",
        "Activate",
        &[int(launcher), PyRep::String("launchProbes".into())],
    )
    .await;
    let launch_notifs = drain(&ctx, &mut p);
    let new_probes: Vec<&Vec<PyRep>> = launch_notifs
        .iter()
        .filter(|(ev, _)| ev == "OnNewProbe")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(new_probes.len(), 8, "Launching 8 probes emits 8 OnNewProbe");

    let mut probe_ids = Vec::new();
    let mut distinct_positions = HashSet::new();
    for args in &new_probes {
        assert_eq!(args.len(), 1, "OnNewProbe takes 1 KeyVal arg");
        let kv = &args[0];
        let pid = kv.dict_get("probeID").and_then(|v| v.as_i64()).expect("probeID");
        assert!(pid > 0);
        assert_eq!(kv.dict_get("typeID").and_then(|v| v.as_i64()), Some(30_013));
        assert!(
            kv.dict_get("scanRange").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0,
            "OnNewProbe must include positive scanRange"
        );
        let expiry_ft = kv.dict_get("expiry").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(
            expiry_ft > 130_000_000_000_000_000 && expiry_ft > before_launch_ft,
            "OnNewProbe expiry must be BlueOS FILETIME ticks, got {expiry_ft}"
        );
        let Some(PyRep::Tuple(pos_t)) = kv.dict_get("pos") else {
            panic!("OnNewProbe must include 3-tuple pos");
        };
        assert_eq!(pos_t.len(), 3, "pos must be a 3-tuple");
        distinct_positions.insert(
            pos_t
                .iter()
                .map(|c| c.as_f64().unwrap().round() as i64)
                .collect::<Vec<_>>(),
        );
        assert!(
            matches!(kv.dict_get("destination"), Some(PyRep::Tuple(d)) if d.len() == 3),
            "OnNewProbe must include 3-tuple destination"
        );
        assert_eq!(kv.dict_get("state").and_then(|v| v.as_i64()), Some(1));
        probe_ids.push(pid);
    }
    probe_ids.sort_unstable();
    assert_eq!(distinct_positions.len(), 8, "Each probe has distinct launch pos");
    assert_eq!(
        launch_notifs
            .iter()
            .filter(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice().get(1) == Some(&int(1)))
            .count(),
        8,
        "Launch emits OnProbeStateChanged(Idle=1) for all 8 probes"
    );

    // 2. Sub-warp move (50 km <= 150 km bubble): Moving(2), no OnProbeWarpStart, unbonused 10s scan
    let (p0, p1) = (probe_ids[0], probe_ids[1]);
    let short_dest = origin + DVec3::X * 50_000.0;
    let req = |entries: &[(i64, DVec3)]| {
        PyRep::Dict(
            entries
                .iter()
                .map(|&(id, d)| {
                    (
                        int(id),
                        PyRep::keyval([("pos", pt(d)), ("scanRange", PyRep::Real(4.0 * AU_M))]),
                    )
                })
                .collect(),
        )
    };
    ok(&ctx, &mut p, "scanMgr", "RequestScans", &[req(&[(p0, short_dest)])]).await;
    let short_notifs = drain(&ctx, &mut p);
    assert!(
        short_notifs
            .iter()
            .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(2)]),
        "Sub-warp move emits OnProbeStateChanged(Moving=2)"
    );
    assert!(
        !short_notifs.iter().any(|(ev, _)| ev == "OnProbeWarpStart"),
        "Sub-warp move within 150km must not emit OnProbeWarpStart"
    );
    let started_short = short_notifs
        .iter()
        .find(|(ev, _)| ev == "OnSystemScanStarted")
        .map(|(_, a)| a)
        .expect("OnSystemScanStarted");
    assert_eq!(started_short.len(), 3);
    assert_eq!(started_short[1].as_i64(), Some(10_000));
    assert!(!short_notifs.iter().any(|(ev, _)| ev == "OnSystemScanStopped"));

    ctx.advance_secs(20);
    let short_done = drain(&ctx, &mut p);
    assert!(short_done
        .iter()
        .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(4)]));
    assert!(short_done
        .iter()
        .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(1)]));
    assert!(short_done.iter().any(|(ev, _)| ev == "OnSystemScanStopped"));

    // 3. Idle scan (probes already at destination): immediate OnProbeStateChanged(Scanning=4)
    let idle_start_ft = ctx.now_blueos_filetime_ticks();
    ok(&ctx, &mut p, "scanMgr", "RequestScans", &[req(&[(p0, short_dest)])]).await;
    let idle_scan = drain(&ctx, &mut p);
    assert!(
        idle_scan
            .iter()
            .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(4)]),
        "Already-idle probe emits OnProbeStateChanged(Scanning=4) immediately"
    );
    let started_idle = idle_scan
        .iter()
        .find(|(ev, _)| ev == "OnSystemScanStarted")
        .map(|(_, a)| a)
        .expect("OnSystemScanStarted");
    assert_eq!(started_idle[0].as_i64(), Some(idle_start_ft));
    ctx.advance_secs(12);
    let idle_done = drain(&ctx, &mut p);
    assert!(idle_done
        .iter()
        .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(p0), int(1)]));
    assert!(idle_done.iter().any(|(ev, _)| ev == "OnSystemScanStopped"));

    // 4. Warp (5 AU & 15 AU) with Astrometrics IV, Astrometric Acquisition IV, Scan Acquisition Array I
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 3412 4").await;
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 25811 4").await;
    common::slash_cmd(
        &ctx,
        &mut p.sess,
        &format!("/fit {ship} 33176 {}", InventoryFlag::MedSlot0 as i64),
    )
    .await;
    let (d5, d15) = (origin + DVec3::X * 5.0 * AU_M, origin + DVec3::Y * 15.0 * AU_M);
    let warp_t0_ft = ctx.now_blueos_filetime_ticks();
    ok(
        &ctx,
        &mut p,
        "scanMgr",
        "RequestScans",
        &[req(&[(p0, d5), (p1, d15)])],
    )
    .await;
    let warp_notifs = drain(&ctx, &mut p);
    for id in [p0, p1] {
        assert!(
            warp_notifs
                .iter()
                .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(id), int(3)]),
            "Warping probe {id} emits OnProbeStateChanged(Warping=3)"
        );
    }
    let warp_info = |id: i64| {
        warp_notifs
            .iter()
            .find(|(ev, a)| ev == "OnProbeWarpStart" && a.first() == Some(&int(id)))
            .map(|(_, a)| a.clone())
            .expect("OnProbeWarpStart")
    };
    let (w0, w1) = (warp_info(p0), warp_info(p1));
    assert_eq!(w0.len(), 5, "OnProbeWarpStart has 5 args");
    assert!(matches!(&w0[1], PyRep::Tuple(t) if t.len() == 3));
    assert!(matches!(&w0[2], PyRep::Tuple(t) if t.len() == 3));
    assert_eq!(w0[3].as_i64(), Some(warp_t0_ft));
    let (dur0, dur1) = (w0[4].as_i64().unwrap(), w1[4].as_i64().unwrap());
    assert!(dur1 > dur0 && dur0 > 0, "15 AU warp ({dur1}ms) > 5 AU warp ({dur0}ms)");

    let started_warp = warp_notifs
        .iter()
        .find(|(ev, _)| ev == "OnSystemScanStarted")
        .map(|(_, a)| a)
        .expect("OnSystemScanStarted");
    assert_eq!(
        started_warp[0].as_i64(),
        Some(warp_t0_ft + dur1 * 10_000),
        "Scan startTime must be scheduled after slowest probe finishes warping"
    );
    assert_eq!(
        started_warp[1].as_i64(),
        Some(4_320),
        "Astrometrics IV x Acquisition IV x Scan Acquisition Array I yields 4320ms"
    );
    assert!(!warp_notifs.iter().any(|(ev, _)| ev == "OnSystemScanStopped"));

    // Advance 3s: probes land and transition to Scanning(4); scan not yet stopped
    ctx.advance_secs(3);
    let landed = drain(&ctx, &mut p);
    for id in [p0, p1] {
        assert!(landed
            .iter()
            .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(id), int(4)]));
    }
    assert!(!landed.iter().any(|(ev, _)| ev == "OnSystemScanStopped"));

    // Advance 6s: scan timer fires -> OnProbeStateChanged(Idle=1) + OnSystemScanStopped with results
    ctx.advance_secs(6);
    let scan_done = drain(&ctx, &mut p);
    for id in [p0, p1] {
        assert!(scan_done
            .iter()
            .any(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice() == [int(id), int(1)]));
    }
    let stopped = scan_done
        .iter()
        .find(|(ev, _)| ev == "OnSystemScanStopped")
        .map(|(_, a)| a)
        .expect("OnSystemScanStopped");
    assert_eq!(stopped.len(), 3);
    assert!(matches!(&stopped[0], PyRep::List(l) if l.len() == 8));
    assert!(matches!(&stopped[1], PyRep::List(l) if !l.is_empty()));

    // 5. Dock + Undock disconnects probes; ReconnectToLostProbes emits OnProbeStateChanged(Idle=1)
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.solar_system_id == HIGHSEC)
        .map(|st| st.station_id)
        .expect("station in HIGHSEC");
    ok(
        &ctx,
        &mut p,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), int(station_id), PyRep::Real(0.0)],
    )
    .await;
    let sys_h = ctx.systems.get_or_spawn(HIGHSEC);
    for _ in 0..120 {
        let _ = sys_h.step_manual(5.0).await;
        if matches!(
            sys_h.get_ball_snapshot(p.ship_id).await.unwrap().unwrap().1.mode,
            eve_destiny::MovementMode::Stop
        ) {
            break;
        }
    }
    ctx.advance_secs(11);
    ok(&ctx, &mut p, "stationSvc", "Dock", &[int(station_id)]).await;
    ctx.advance_secs(12);
    ok(&ctx, &mut p, "stationSvc", "Undock", &[]).await;
    let _ = drain(&ctx, &mut p);
    ok(&ctx, &mut p, "scanMgr", "ReconnectToLostProbes", &[]).await;
    let recon = drain(&ctx, &mut p);
    assert_eq!(
        recon
            .iter()
            .filter(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice().get(1) == Some(&int(1)))
            .count(),
        8,
        "ReconnectToLostProbes emits OnProbeStateChanged(Idle=1) for 8 lost probes"
    );

    // 6. RecoverProbes emits OnProbeStateChanged(Returning=5)
    ok(&ctx, &mut p, "scanMgr", "RecoverProbes", &[]).await;
    let rec = drain(&ctx, &mut p);
    assert_eq!(
        rec.iter()
            .filter(|(ev, a)| ev == "OnProbeStateChanged" && a.as_slice().get(1) == Some(&int(5)))
            .count(),
        8,
        "RecoverProbes emits OnProbeStateChanged(Returning=5) for 8 recovered probes"
    );
}

#[tokio::test]
async fn test_live_solarsystem_interference_changed_via_crab_beacon() {
    let ctx = paused_ctx();
    let mut cap_pilot = common::gm_pilot_in_space(&ctx, 71, 92_500_001, ARCHON, NULLSEC).await;
    let mut observer = common::gm_pilot_in_space(&ctx, 72, 92_500_002, VEXOR, NULLSEC).await;
    let mut outsider = common::gm_pilot_in_space(&ctx, 73, 92_500_003, VEXOR, HIGHSEC).await;

    common::slash_cmd(&ctx, &mut cap_pilot.sess, "/giveskill 11584 5").await;
    let _ = drain(&ctx, &mut cap_pilot);
    let _ = drain(&ctx, &mut observer);
    let _ = drain(&ctx, &mut outsider);

    // Deploy and link 1st CRAB beacon in NULLSEC -> OnSolarsystemInterferenceChanged(sys_id, state)
    let b1 = common::slash_cmd(&ctx, &mut cap_pilot.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1"))
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("beacon 1 id");
    ok(&ctx, &mut cap_pilot, "posMgr", "AnchorStructure", &[int(b1)]).await;
    ok(
        &ctx,
        &mut cap_pilot,
        "beyonce",
        "CallComponentFromClient",
        &[
            int(b1),
            PyRep::String("linkWithShip".into()),
            PyRep::String("InitiateLink".into()),
        ],
    )
    .await;

    let cap_notifs = drain(&ctx, &mut cap_pilot);
    let obs_notifs = drain(&ctx, &mut observer);
    let out_notifs = drain(&ctx, &mut outsider);

    assert!(
        !out_notifs
            .iter()
            .any(|(ev, _)| ev == "OnSolarsystemInterferenceChanged"),
        "Pilot in another solar system must not receive OnSolarsystemInterferenceChanged"
    );
    let obs_ev = obs_notifs
        .iter()
        .find(|(ev, _)| ev == "OnSolarsystemInterferenceChanged")
        .map(|(_, a)| a)
        .expect("Observer in NULLSEC must receive OnSolarsystemInterferenceChanged");
    assert_eq!(obs_ev.len(), 2, "OnSolarsystemInterferenceChanged has 2 args");
    assert_eq!(obs_ev[0], int(NULLSEC as i64));
    assert_eq!(obs_ev[1].dict_get("solarSystemID"), Some(&int(NULLSEC as i64)));
    assert_eq!(obs_ev[1].dict_get("interferenceBand"), Some(&int(1)));
    assert_eq!(obs_ev[1].dict_get("crabActivationsRemaining"), Some(&int(11)));
    let lvl1 = obs_ev[1]
        .dict_get("interferenceLevel")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((lvl1 - CRAB_INTERFERENCE_DELTA).abs() < 1e-4);
    assert!(cap_notifs
        .iter()
        .any(|(ev, _)| ev == "OnSolarsystemInterferenceChanged"));

    // Link 4 more CRAB beacons (total 5/12 = 0.4167 >= 0.35) -> Band 2 (Medium)
    for idx in 0..4 {
        let mut extra_cap =
            common::gm_pilot_in_space(&ctx, 80 + idx, 92_500_010 + idx as i64, ARCHON, NULLSEC).await;
        common::slash_cmd(&ctx, &mut extra_cap.sess, "/giveskill 11584 5").await;
        let bid = common::slash_cmd(
            &ctx,
            &mut extra_cap.sess,
            &format!("/createitem {TYPE_CRAB_BEACON} 1"),
        )
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("extra beacon id");
        ok(&ctx, &mut extra_cap, "posMgr", "AnchorStructure", &[int(bid)]).await;
        ok(
            &ctx,
            &mut extra_cap,
            "beyonce",
            "CallComponentFromClient",
            &[
                int(bid),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
        )
        .await;
    }

    let obs_band2 = drain(&ctx, &mut observer);
    let last_ev = obs_band2
        .iter()
        .rfind(|(ev, _)| ev == "OnSolarsystemInterferenceChanged")
        .map(|(_, a)| a)
        .expect("Band 2 OnSolarsystemInterferenceChanged");
    assert_eq!(last_ev[0], int(NULLSEC as i64));
    assert_eq!(last_ev[1].dict_get("interferenceBand"), Some(&int(2)));
    assert_eq!(
        last_ev[1].dict_get("bandLabel"),
        Some(&PyRep::String("Medium".into()))
    );
    assert_eq!(last_ev[1].dict_get("crabActivationsRemaining"), Some(&int(7)));
}
