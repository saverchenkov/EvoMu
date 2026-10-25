//! Shared integration test utilities and common session helpers.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

/// Fixed virtual-clock base (Unix seconds) shared by time-dependent integration tests.
#[allow(dead_code)]
pub const TEST_CLOCK_BASE_UNIX: u64 = 1_800_000_000;

/// Pins `ctx` to a deterministic, non-drifting virtual clock (`T-7`) so assertions on
/// server-produced timestamps never race the host wall clock across a second boundary.
/// Time then only moves via `ctx.advance_secs` / explicit tick deltas.
#[allow(dead_code)]
pub fn pin_clock(ctx: &ServerContext) {
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        TEST_CLOCK_BASE_UNIX,
    )));
}

#[allow(dead_code)]
pub async fn login_pilot(
    ctx: &ServerContext,
    sess: &mut SessionState,
    username: &str,
    password: &str,
) {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String(username.into()),
            PyRep::String(password.into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
}

/// Ensures the session pilot has 8 launched Core Scanner Probes in the current system (fitting a Core Probe
/// Launcher I + 8 Core Scanner Probe I on the active ship and firing it via `dogmaIM.Activate` when needed),
/// then places all probes around `center` with `range_m` and runs the catalog `scanMgr.RequestScans`.
/// Returns the scan result rows (KeyVal per signature).
#[allow(dead_code)]
pub async fn probe_scan(ctx: &ServerContext, sess: &mut SessionState, center: glam::DVec3, range_m: f64) -> Vec<PyRep> {
    let kw = HashMap::new();
    let (char_id, sys_id, ship_id) = (sess.character_id.unwrap(), sess.solar_system_id.unwrap(), sess.ship_id.unwrap());
    let active = |ctx: &ServerContext| -> Vec<i64> {
        let mut ids: Vec<i64> = ctx.active_scanner_probes.read().unwrap().values()
            .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state != 0)
            .map(|p| p.probe_id).collect();
        ids.sort_unstable();
        ids
    };
    if active(ctx).is_empty() {
        let launcher_id = ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(char_id).or_default();
            for sid in [3402u32, 3412, 3413, 3426] {
                skills.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
            let ids = [snap.next_entity_id + 1, snap.next_entity_id + 2];
            snap.next_entity_id += 2;
            for (iid, tid, qty) in [(ids[0], 17938_u32, 1), (ids[1], 30013, 8)] {
                snap.items.insert(iid, eve_db::InventoryItemRecord {
                    item_id: iid, type_id: tid, owner_id: char_id, location_id: ship_id,
                    flag: eve_db::InventoryFlag::HiSlot2, quantity: qty, singleton: qty == 1, ..Default::default()
                });
            }
            ids[0]
        });
        ctx.invalidate_ship_fitting(ship_id);
        ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .expect("launch scanner probes");
    }
    let d = range_m * 0.4;
    let offs = [glam::DVec3::ZERO, glam::DVec3::X * d, -glam::DVec3::X * d, glam::DVec3::Y * d, -glam::DVec3::Y * d, glam::DVec3::Z * d, -glam::DVec3::Z * d, glam::DVec3::splat(d * 0.5)];
    let dict = PyRep::Dict(active(ctx).iter().enumerate().map(|(i, &pid)| {
        let p = center + offs[i % offs.len()];
        (PyRep::Integer(pid), PyRep::keyval([
            ("pos", PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])),
            ("scanRange", PyRep::Real(range_m)),
        ]))
    }).collect());
    let bound = ctx.dispatch_rpc(sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let bound_id = bound.dict_get("boundID").and_then(|v| v.as_i64()).map(|v| v as u64);
    match ctx.dispatch_rpc(sess, "scanMgr", bound_id, "RequestScans", &[dict], &kw, None).await.expect("scanMgr.RequestScans") {
        PyRep::List(rows) => rows,
        other => panic!("scanMgr.RequestScans must return a row list, got {other:?}"),
    }
}

/// Field of the `scanMgr.RequestScans` row for `sig_id`.
#[allow(dead_code)]
pub fn scan_row_field(rows: &[PyRep], sig_id: &str, key: &str) -> PyRep {
    rows.iter()
        .find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(sig_id))
        .and_then(|r| r.dict_get(key).cloned())
        .unwrap_or_else(|| panic!("scan row {sig_id} missing {key}"))
}
