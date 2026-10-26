//! Shared integration test utilities and common session helpers.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

/// Fixed virtual-clock base (Unix seconds) shared by time-dependent integration tests.
#[allow(dead_code)]
pub const TEST_CLOCK_BASE_UNIX: u64 = 1_800_000_000;

#[allow(dead_code)]
pub fn seed_test_nes_store_catalog(ctx: &ServerContext) {
    use eve_server::services::quasar_payment_store::{
        save_store_catalog, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
    };
    ctx.db.with_mut_snapshot(|snap| {
        if snap.service_state_blobs.contains_key("nes_store_catalog") {
            return;
        }
        let type_name = |tid: u32| -> String {
            ctx.sde.types.get(&tid).map(|t| t.name.clone()).unwrap_or_default()
        };
        let cat_name = |cid: u32, fallback: &str| -> String {
            ctx.sde
                .categories
                .get(&cid)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| fallback.to_string())
        };
        let catalog = StoreCatalog {
            categories: vec![
                StoreCategory {
                    id: 1,
                    name: "Clone State & Omega".into(),
                    description: "Omega Clone State subscriptions".into(),
                },
                StoreCategory {
                    id: 2,
                    name: cat_name(91, "Ship SKINs"),
                    description: "Permanent Ship Cosmetic Licenses".into(),
                },
                StoreCategory {
                    id: 3,
                    name: cat_name(5, "Accessories"),
                    description: "Pilot services and skill extraction".into(),
                },
                StoreCategory {
                    id: 4,
                    name: cat_name(30, "Apparel"),
                    description: "Pilot customizations and vanity items".into(),
                },
            ],
            products: vec![
                StoreProduct {
                    id: 101,
                    name: "30 Days Omega Clone State".into(),
                    description: "30 Days Omega clone subscription".into(),
                    type_id: 0,
                    quantity: 1,
                },
                StoreProduct {
                    id: 102,
                    name: type_name(34599),
                    description: type_name(34599),
                    type_id: 34599,
                    quantity: 1,
                },
                StoreProduct {
                    id: 103,
                    name: type_name(40519),
                    description: type_name(40519),
                    type_id: 40519,
                    quantity: 1,
                },
                StoreProduct {
                    id: 104,
                    name: type_name(3966),
                    description: type_name(3966),
                    type_id: 3966,
                    quantity: 1,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 1001,
                    name: "30 Days Omega Clone State".into(),
                    description: "30 Days Omega clone subscription".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![101],
                    targeted: false,
                },
                StoreOffer {
                    id: 1002,
                    name: type_name(34599),
                    description: type_name(34599),
                    category_id: 2,
                    price_plex: 110,
                    product_ids: vec![102],
                    targeted: false,
                },
                StoreOffer {
                    id: 1003,
                    name: type_name(40519),
                    description: type_name(40519),
                    category_id: 3,
                    price_plex: 140,
                    product_ids: vec![103],
                    targeted: false,
                },
                StoreOffer {
                    id: 1004,
                    name: type_name(3966),
                    description: type_name(3966),
                    category_id: 4,
                    price_plex: 75,
                    product_ids: vec![104],
                    targeted: false,
                },
                StoreOffer {
                    id: 2001,
                    name: "Targeted Omega Bundle".into(),
                    description: "Special targeted clone subscription".into(),
                    category_id: 1,
                    price_plex: 450,
                    product_ids: vec![101],
                    targeted: true,
                },
                StoreOffer {
                    id: 3001,
                    name: type_name(40519),
                    description: type_name(40519),
                    category_id: 3,
                    price_plex: 120,
                    product_ids: vec![103],
                    targeted: true,
                },
            ],
        };
        save_store_catalog(snap, &catalog);
    });
}

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
pub fn seed_test_lp_store_offers(ctx: &ServerContext) {
    use eve_db::IskCents;
    use eve_server::services::lp_store::{register_lp_store_offer, LPOffer, PARAGON_CORP_ID};
    ctx.db.with_mut_snapshot(|snap| {
        if snap.service_state_blobs.contains_key("lp_store_offers") {
            return;
        }
        let rows = [
            (1001, 1_000_035, 15810, 1, 2_400, 2_400_000, vec![], 0),
            (1002, 1_000_035, 27361, 1_000, 500, 500_000, vec![], 0),
            (1003, 1_000_035, 17619, 1, 10_000, 10_000_000, vec![], 0),
            (1004, 1_000_035, 15810, 1, 1_500, 1_500_000, vec![(34, 100)], 0),
            (2001, 1_000_036, 15808, 1, 2_400, 2_400_000, vec![], 0),
            (2002, 1_000_036, 17620, 1, 10_000, 10_000_000, vec![], 0),
            (3001, 1_000_044, 15806, 1, 2_400, 2_400_000, vec![], 0),
            (3002, 1_000_044, 17621, 1, 10_000, 10_000_000, vec![], 0),
            (4001, 1_000_037, 15807, 1, 2_400, 2_400_000, vec![], 0),
            (4002, 1_000_037, 17622, 1, 10_000, 10_000_000, vec![], 0),
            (5001, 1_000_125, 15810, 1, 3_000, 3_000_000, vec![], 0),
            (5002, 1_000_125, 43_682, 1, 100, 100_000, vec![], 20),
            (6001, PARAGON_CORP_ID, 72964, 1, 750, 0, vec![], 0),
            (6002, PARAGON_CORP_ID, 73377, 1, 750, 0, vec![], 0),
            (6003, PARAGON_CORP_ID, 73394, 1, 1_250, 0, vec![], 0),
            (6004, PARAGON_CORP_ID, 81348, 10, 1_000, 0, vec![], 0),
            (7001, 1_000_179, 17622, 1, 10_000, 10_000_000, vec![], 0),
            (7002, 1_000_180, 17619, 1, 10_000, 10_000_000, vec![], 0),
            (7003, 1_000_181, 17620, 1, 10_000, 10_000_000, vec![], 0),
            (7004, 1_000_182, 17621, 1, 10_000, 10_000_000, vec![], 0),
        ];
        for (offer_id, corp_id, type_id, quantity, lp_cost, isk, req_items, ak_cost) in rows {
            register_lp_store_offer(
                snap,
                LPOffer {
                    offer_id,
                    corp_id,
                    type_id,
                    quantity,
                    lp_cost,
                    isk_cost: IskCents::from_isk(isk),
                    req_items,
                    ak_cost,
                },
            );
        }
    });
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
