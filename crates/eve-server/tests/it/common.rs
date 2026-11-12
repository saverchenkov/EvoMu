//! Shared integration test utilities and common session helpers.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::outbound_sequencer::OutFrame;
use eve_server::router::client_wire::{
    dispatch_call_on_wire, ClientWireCallOutcome, ClientWireSession,
};
use eve_server::router::ServerContext;
use eve_testkit::client_contract::SemanticLookupContext;
use eve_testkit::client_harness::{
    extract_bound_id_from_wire_rep, ClientErrorResponse, ClientHarness, ClientWireExchange,
    ClientWireFrame, ClientWireFrameKind, ClientWireTransport,
};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

#[allow(dead_code)]
fn to_wire_exchange(out: ClientWireCallOutcome) -> ClientWireExchange {
    let rpc_result = match out.rpc_result {
        Ok(rep) => Ok(rep),
        Err(err) => Err(ClientErrorResponse {
            code: err.code,
            message: err.message,
            wire_body: out.error_wire_body.unwrap_or(PyRep::None),
        }),
    };
    let frames = out
        .out_frames
        .into_iter()
        .zip(out.wire_packets)
        .map(|(f, wire_packet)| {
            let kind = match f {
                OutFrame::CallRsp { call_id, .. } => match &rpc_result {
                    Ok(reply) => ClientWireFrameKind::CallRsp {
                        call_id,
                        reply: reply.clone(),
                    },
                    Err(err) => ClientWireFrameKind::ErrorResponse {
                        call_id,
                        body: err.wire_body.clone(),
                    },
                },
                OutFrame::SessionChange { frame } => {
                    ClientWireFrameKind::SessionChange { packet: frame }
                }
                OutFrame::DoDestinyUpdate {
                    epoch,
                    stamp,
                    has_set_state,
                    updates,
                    wait_for_bubble,
                } => ClientWireFrameKind::DoDestinyUpdate {
                    epoch,
                    stamp,
                    has_set_state,
                    updates,
                    wait_for_bubble,
                },
                OutFrame::Notification { event, args } => {
                    ClientWireFrameKind::Notification { event, args }
                }
            };
            ClientWireFrame { kind, wire_packet }
        })
        .collect();
    ClientWireExchange {
        rpc_result,
        frames,
        marshal_errors: out.marshal_errors,
    }
}

/// Persistent `ClientWireTransport` backed by `eve_server::router::client_wire::ClientWireSession` (`C-2`).
#[allow(dead_code)]
pub struct ServerWireTransport<'c> {
    pub inner: ClientWireSession<'c>,
}

impl<'c> ClientWireTransport for ServerWireTransport<'c> {
    fn send_wire_call<'a>(
        &'a mut self,
        service: &'a str,
        bound_id: Option<u64>,
        method: &'a str,
        args: &'a [PyRep],
        kwargs: &'a HashMap<String, PyRep>,
    ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>> {
        Box::pin(async move {
            let out = self
                .inner
                .call_wire(service, bound_id, method, args, kwargs)
                .await;
            to_wire_exchange(out)
        })
    }

    fn semantic_lookup(&self) -> Option<SemanticLookupContext<'_>> {
        Some(SemanticLookupContext {
            sde: &self.inner.ctx.sde,
            db: &self.inner.ctx.db,
        })
    }
}

/// Single-call borrowed `ClientWireTransport` over `(&ServerContext, &mut SessionState)` (`C-2`).
#[allow(dead_code)]
pub struct SessionBorrowWireTransport<'s, 'c> {
    pub ctx: &'c ServerContext,
    pub session: &'s mut SessionState,
    pub preserve_char_notifications: bool,
}

impl<'s, 'c> ClientWireTransport for SessionBorrowWireTransport<'s, 'c> {
    fn send_wire_call<'a>(
        &'a mut self,
        service: &'a str,
        bound_id: Option<u64>,
        method: &'a str,
        args: &'a [PyRep],
        kwargs: &'a HashMap<String, PyRep>,
    ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>> {
        Box::pin(async move {
            let saved_session_notifs = if self.preserve_char_notifications {
                std::mem::take(&mut self.session.notifications)
            } else {
                Vec::new()
            };
            let saved_char_notifs = if self.preserve_char_notifications {
                self.session
                    .character_id
                    .filter(|&cid| cid != 0)
                    .map(|cid| self.ctx.drain_notifications_for_char(cid))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let out = dispatch_call_on_wire(
                self.ctx,
                self.session,
                service,
                bound_id,
                method,
                args,
                kwargs,
            )
            .await;
            if self.preserve_char_notifications {
                let mut restored_session = saved_session_notifs;
                restored_session.extend(out.notifications.iter().cloned());
                self.session.notifications = restored_session;
                let mut restored_char = saved_char_notifs;
                restored_char.extend(out.drained_char_notifications.iter().cloned());
                if !restored_char.is_empty() {
                    self.ctx.restore_pending_notifications(restored_char);
                }
            }
            to_wire_exchange(out)
        })
    }

    fn semantic_lookup(&self) -> Option<SemanticLookupContext<'_>> {
        Some(SemanticLookupContext {
            sde: &self.ctx.sde,
            db: &self.ctx.db,
        })
    }
}

/// Opens a new persistent [`ClientHarness`] on the exact client wire path for `session_id` (`C-2`).
#[allow(dead_code)]
pub fn new_wire_harness<'c>(
    ctx: &'c ServerContext,
    session_id: u64,
) -> ClientHarness<ServerWireTransport<'c>> {
    ClientHarness::new(ServerWireTransport {
        inner: ClientWireSession::new(ctx, session_id),
    })
}

/// Wraps an existing `SessionState` in a persistent [`ClientHarness`] on the wire path (`C-2`).
#[allow(dead_code)]
pub fn wrap_wire_harness<'c>(
    ctx: &'c ServerContext,
    session: SessionState,
) -> ClientHarness<ServerWireTransport<'c>> {
    ClientHarness::new(ServerWireTransport {
        inner: ClientWireSession::from_session(ctx, session),
    })
}

/// Wraps borrowed `(&ServerContext, &mut SessionState)` in a wire [`ClientHarness`] (`C-2`).
#[allow(dead_code)]
pub fn borrow_wire_harness<'s, 'c>(
    ctx: &'c ServerContext,
    session: &'s mut SessionState,
) -> ClientHarness<SessionBorrowWireTransport<'s, 'c>> {
    ClientHarness::new(SessionBorrowWireTransport {
        ctx,
        session,
        preserve_char_notifications: false,
    })
}

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
    let mut harness = borrow_wire_harness(ctx, sess);
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[
                PyRep::String(username.into()),
                PyRep::String(password.into()),
            ],
            &kw,
        )
        .await;
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
            for sid in [3402_u32, 3412, 3413, 3426] {
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
        let mut harness = borrow_wire_harness(ctx, sess);
        harness
            .call_ok("dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)], &kw)
            .await;
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
    let mut harness = borrow_wire_harness(ctx, sess);
    let bound = harness.call_ok("scanMgr", None, "GetSystemScanMgr", &[], &kw).await;
    let bound_id = extract_bound_id_from_wire_rep(&bound);
    match harness.call_ok("scanMgr", bound_id, "RequestScans", &[dict], &kw).await {
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
    let mut harness = ClientHarness::new(SessionBorrowWireTransport {
        ctx,
        session: sess,
        preserve_char_notifications: true,
    });
    harness
        .call(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(cmd.into())],
            &HashMap::new(),
        )
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
    let mut harness = new_wire_harness(ctx, sess_id);
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String(user), PyRep::String(pass)],
            &kw,
        )
        .await;
    harness
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;
    let created = harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/createitem {ship_type_id} 1"))],
            &kw,
        )
        .await;
    let ship_id = created.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem itemID");
    let required: Vec<u32> = ctx.sde.types.get(&ship_type_id).map(|t| {
        [182_u16, 183, 184, 1285, 1289, 1290].iter().filter_map(|a| t.default_attributes.get(a)).map(|v| v.round() as u32).filter(|&s| s > 0).collect()
    }).unwrap_or_default();
    for skill_id in required {
        harness
            .call_ok(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(format!("/giveskill {skill_id} 5"))],
                &kw,
            )
            .await;
    }
    ctx.advance_secs(31);
    harness
        .call_ok("ship", None, "Board", &[PyRep::Integer(ship_id)], &kw)
        .await;
    ctx.advance_secs(31);
    harness.call_ok("stationSvc", None, "Undock", &[], &kw).await;
    harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/tr {sys_id}"))],
            &kw,
        )
        .await;
    harness.transport_mut().inner.session.ship_id = Some(ship_id);
    let bind = harness
        .call_ok(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(sys_id)), PyRep::None],
            &kw,
        )
        .await;
    let beyonce = extract_bound_id_from_wire_rep(&bind).expect("beyonce bound id");
    ctx.advance_secs(31);
    let mut sess = harness.into_transport().inner.into_session();
    sess.live_notify_listener = false;
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
    let mut harness = borrow_wire_harness(ctx, &mut p.sess);
    harness
        .call(
            "invbroker",
            None,
            "Add",
            &[PyRep::Integer(item_id), PyRep::Integer(p.ship_id), PyRep::Integer(flag)],
            &HashMap::new(),
        )
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
    let mut harness = borrow_wire_harness(ctx, &mut p.sess);
    harness
        .call_ok(
            "beyonce",
            Some(p.beyonce),
            "CmdGotoPoint",
            &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)],
            &kw,
        )
        .await;
    let mut best = f64::MAX;
    for _ in 0..600 {
        sys.step_manual(1.0).await.unwrap();
        let d = sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position.distance(dest);
        if d < 500.0 || d > best {
            break;
        }
        best = d;
    }
    harness
        .call_ok("beyonce", Some(p.beyonce), "CmdStop", &[], &kw)
        .await;
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position
}

/// Runs world-state teardown invariants (`#908`, `#947`) and, when `EVOMU_PRE_CHECK=1` is set,
/// Persist-Reload Equivalence (PRE) and `SnapshotDirtyHints` fast-path checks.
#[allow(dead_code)]
pub fn assert_teardown_invariants(ctx: &ServerContext) {
    eve_testkit::pre_check::assert_world_and_pre_guards(&ctx.db, Some(&ctx.sde), false);
}

/// Unconditionally runs world-state teardown invariants, Persist-Reload Equivalence (PRE), and
/// `SnapshotDirtyHints` fast-path equivalence checks (`#908`).
#[allow(dead_code)]
pub fn assert_pre_check_strict(ctx: &ServerContext) {
    eve_testkit::pre_check::assert_world_and_pre_guards(&ctx.db, Some(&ctx.sde), true);
}

/// Runs the strict Quasar protobuf decode & PB-4 response-pairing tap (`#911`, `#893`, `#936`)
/// on a public gateway request/response exchange and any drained notice payloads.
#[allow(dead_code)]
pub fn tap_quasar_exchange(
    ctx: &ServerContext,
    request_type: &str,
    rsp: &eve_proto::quasar::EvePublicResponse,
    notices: &[eve_proto::quasar::GoogleAny],
) -> Vec<eve_testkit::quasar_tap::QuasarTapViolation> {
    eve_testkit::quasar_tap::tap_exchange(
        &ctx.quasar_gateway.descriptors,
        request_type,
        rsp.status_code,
        rsp.payload.as_ref(),
        notices,
    )
}

