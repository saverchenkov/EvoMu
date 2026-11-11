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

/// Clears all escort waves (`1..=total_waves`) of an incursion site via `on_incursion_boss_destroyed`
/// so its final-wave boss spawns in space and is eligible for site completion.
#[allow(dead_code)]
pub async fn clear_incursion_site_escort_waves(
    ctx: &ServerContext,
    solar_system_id: u32,
    site_id: &str,
) -> i64 {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for _ in 0..6 {
        let (escorts, done, boss_bid) = eve_server::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let site = reg
                .incursions
                .values()
                .flat_map(|inc| &inc.sites)
                .find(|s| s.site_id == site_id)
                .unwrap_or_else(|| panic!("incursion site {site_id} not found"));
            let pending: Vec<i64> = site
                .escort_spawns
                .iter()
                .filter(|e| !e.destroyed)
                .map(|e| e.npc_ball_id)
                .collect();
            let wave_done = reg
                .incursion_site_waves
                .get(site_id)
                .is_none_or(|w| w.current_wave >= w.total_waves)
                && pending.is_empty();
            (pending, wave_done, site.boss_npc_ball_id)
        });
        if done {
            return boss_bid;
        }
        for ebid in escorts {
            let _ = sys_handle.remove_ball(ebid).await;
            let _ = eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed(
                ctx,
                solar_system_id,
                ebid,
                None,
            )
            .await;
        }
    }
    panic!("failed to clear escort waves for incursion site {site_id}");
}

#[allow(dead_code)]
pub fn seed_fast_checkout_plex_offer(ctx: &ServerContext, user_id: u64, offer_id: u32, plex_qty: u32, price_cents: u64) {
    use eve_server::services::quasar_payment_store_types::{
        add_user_quick_pay_token, save_store_catalog, PaymentTokenRecord, StoreCatalog,
        StoreCategory, StoreOffer, StoreProduct,
    };
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory { id: 1, name: "PLEX".into(), description: "PLEX Packs".into() }],
                products: vec![StoreProduct {
                    id: 101,
                    name: format!("{plex_qty} PLEX"),
                    description: format!("{plex_qty} PLEX"),
                    type_id: 44_992,
                    quantity: plex_qty,
                }],
                offers: vec![StoreOffer {
                    id: offer_id,
                    name: format!("{plex_qty} PLEX"),
                    description: format!("{plex_qty} PLEX"),
                    category_id: 1,
                    price_plex: 0,
                    product_ids: vec![101],
                    targeted: false,
                    price_cents: Some(price_cents),
                    currency: Some("USD".into()),
                }],
            },
        );
    });
    add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        user_id,
        PaymentTokenRecord {
            id: 1,
            user_id,
            token_str: "tok_quick_1".into(),
            is_quick_pay: true,
            consumed: false,
        },
    );
}

/// GM pilot flown into space purely through client RPCs and GM slash commands.
#[allow(dead_code)]
pub struct LivePilot {
    pub sess: SessionState,
    pub char_id: i64,
    pub ship_id: i64,
    /// `beyonce` bound object for the current solar system (`beyonce.MachoBindObject`).
    pub beyonce: u64,
}

/// Dispatches a GM `slash.SlashCmd` and panics with the command text on failure.
#[allow(dead_code)]
pub async fn slash_cmd(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    ctx.dispatch_rpc(sess, "slash", None, "SlashCmd", &[PyRep::String(cmd.into())], &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{cmd} failed: {e:?}"))
}

/// Provisions a GM account docked in Jita 4-4, creates `ship_type_id` with `/createitem`, boards it
/// (`ship.Board`), undocks (`stationSvc.Undock`) and teleports to `sys_id` with `/tr` (lands at 10 km on X).
#[allow(dead_code)]
pub async fn gm_pilot_in_space(ctx: &ServerContext, sess_id: u64, char_id: i64, ship_type_id: u32, sys_id: u32) -> LivePilot {
    let kw = HashMap::new();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    login_pilot(ctx, &mut sess, &user, &pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let created = slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await;
    let ship_id = created.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem itemID");
    let required: Vec<u32> = ctx.sde.types.get(&ship_type_id).map(|t| {
        [182_u16, 183, 184, 1285, 1289, 1290].iter().filter_map(|a| t.default_attributes.get(a)).map(|v| v.round() as u32).filter(|&s| s > 0).collect()
    }).unwrap_or_default();
    for skill_id in required {
        slash_cmd(ctx, &mut sess, &format!("/giveskill {skill_id} 5")).await;
    }
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None).await.expect("ship.Board");
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("stationSvc.Undock");
    slash_cmd(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);
    let bind = ctx.dispatch_rpc(&mut sess, "beyonce", None, "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::None], &kw, None)
        .await
        .expect("beyonce.MachoBindObject");
    let PyRep::Tuple(bound) = bind else { panic!("bind tuple") };
    let beyonce = bound[0].as_i64().expect("beyonce bound id") as u64;
    ctx.advance_secs(31);
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    LivePilot { sess, char_id, ship_id, beyonce }
}

/// Creates `qty` x `type_id` in the pilot's cargo (`/createitem`) and moves the stack into `flag` of the
/// active ship through `invbroker.Add`; returns the moved item id.
#[allow(dead_code)]
pub async fn stock_ship_bay(ctx: &ServerContext, p: &mut LivePilot, type_id: u32, qty: i32, flag: i64) -> i64 {
    let created = slash_cmd(ctx, &mut p.sess, &format!("/createitem {type_id} {qty}")).await;
    let item_id = created.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem itemID");
    ctx.dispatch_rpc(&mut p.sess, "invbroker", None, "Add", &[PyRep::Integer(item_id), PyRep::Integer(p.ship_id), PyRep::Integer(flag)], &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("invbroker.Add {type_id} -> flag {flag} failed: {e:?}"));
    item_id
}

/// Flies the pilot's ship to `dest` with `beyonce.CmdGotoPoint`, stepping the system actor until within
/// 500 m (or past closest approach), then `beyonce.CmdStop`; returns the final ball position.
#[allow(dead_code)]
pub async fn fly_to(ctx: &ServerContext, p: &mut LivePilot, dest: glam::DVec3) -> glam::DVec3 {
    let kw = HashMap::new();
    let sys = ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space"));
    ctx.dispatch_rpc(&mut p.sess, "beyonce", Some(p.beyonce), "CmdGotoPoint", &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)], &kw, None)
        .await
        .expect("CmdGotoPoint");
    let mut best = f64::MAX;
    for _ in 0..600 {
        sys.step_manual(1.0).await.unwrap();
        let d = sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position.distance(dest);
        if d < 500.0 || d > best {
            break;
        }
        best = d;
    }
    ctx.dispatch_rpc(&mut p.sess, "beyonce", Some(p.beyonce), "CmdStop", &[], &kw, None).await.expect("CmdStop");
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position
}
