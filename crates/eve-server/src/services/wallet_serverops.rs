//! `wallet_serverops` — personal wallet transfers, CSPA charge settings, and cluster/server-ops
//! endpoints the V24 client calls during normal operation.
//!
//! Handles:
//! - `account.GiveCash(toID, amount, reason)` — player donation with both journals + `OnAccountChange`
//! - `account.SetContactCost(cost)` / `account.GetDefaultContactCost()` — CSPA charge for strangers
//! - `machoNet.GetClusterGameStatisticsForClient()` — online pilot counts per solar system
//! - `pingService.Ping()` — latency probe answered with the authoritative server blue time
//! - client telemetry sinks: `eventLog`, `clientStatLogger`, `alert`, `infoGatheringMgr`, `eveguard_report`
//! - `localizationServer.GetAllTextChanges()` — server-side text overrides (none configured)
//! - `FastCheckoutService` — real-money PLEX checkout, served as disabled (no payment provider)

#![forbid(unsafe_code)]

use super::wallet_services_ext;
pub use super::wallet_services_ext::*;

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, IskCents, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};
use tracing::debug;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

fn check_starbase_auth(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    structure_id: i64,
) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() { return Ok(()); }
    let owner_corp = snap.structures.get(&structure_id).map(|s| s.owner_corp_id)
        .or_else(|| snap.items.get(&structure_id).map(|i| i.owner_id))
        .ok_or_else(|| rpc_err("StructureNotFound", format!("Starbase structure {structure_id} not found")))?;
    let char_corp = session.corporation_id
        .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id)).unwrap_or(0);
    if owner_corp != char_corp && !(char_id > 0 && owner_corp == char_id) {
        return Err(rpc_err("NotStarbaseOwner", format!("Structure {structure_id} owned by {owner_corp}, caller belongs to {char_corp}")));
    }
    if owner_corp == char_id { return Ok(()); }
    let has_role = snap.corporations.get(&char_corp).map(|c| {
        if c.ceo_id == char_id { return true; }
        let r = c.member_roles.get(&char_id).copied().unwrap_or(0) | session.corp_role;
        r & ((1 << 0) | (1 << 24) | (1 << 25) | (1 << 40)) != 0
    }).unwrap_or(false) || (session.corp_role & ((1 << 0) | (1 << 24) | (1 << 25) | (1 << 40)) != 0);
    if !has_role {
        return Err(rpc_err("RoleNotAuthorized", "Starbase operations require Director or Starbase Operator role"));
    }
    Ok(())
}

/// Creates a corporation or personal bill (office rent `bill_type_id = 2`, infrastructure `6`, alliance maintenance `5`).
pub fn create_bill(
    ctx: &ServerContext,
    bill_type_id: i64,
    debtor_id: i64,
    creditor_id: i64,
    amount_isk: f64,
    due_unix: u64,
    interest: f64,
    external_id: i64,
    external_id2: i64,
) -> i64 {
    let now = ctx.now_unix();
    let amount_cents = IskCents::from_f64_isk(amount_isk).0;
    let (bill_id, is_corp_debtor) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bill_id = snap.next_entity_id;
        let rec = CorpBillRecord {
            bill_id,
            bill_type_id,
            debtor_id,
            creditor_id,
            amount_cents,
            due_unix,
            interest,
            external_id,
            external_id2,
            paid: false,
            paid_by_division: None,
        };
        save_json_note(
            snap,
            bill_note_id(bill_id),
            -71,
            bill_id,
            format!("bill:{bill_id}"),
            &rec,
            now,
        );
        (bill_id, snap.corporations.contains_key(&debtor_id))
    });
    let bill_kv = PyRep::keyval([
        ("billID", PyRep::Integer(bill_id)),
        ("billTypeID", PyRep::Integer(bill_type_id)),
        ("debtorID", PyRep::Integer(debtor_id)),
        ("creditorID", PyRep::Integer(creditor_id)),
        ("amount", PyRep::Real(amount_isk)),
        ("dueDateTime", PyRep::Integer(due_unix as i64)),
        ("interest", PyRep::Real(interest)),
        ("externalID", PyRep::Integer(external_id)),
        ("externalID2", PyRep::Integer(external_id2)),
    ]);
    if is_corp_debtor {
        ctx.notify(
            NotificationTarget::Corporation(debtor_id),
            "OnBillReceived",
            "corpid",
            vec![bill_kv],
        );
    } else {
        ctx.notify(
            NotificationTarget::Character(debtor_id),
            "OnBillReceived",
            "charid",
            vec![bill_kv],
        );
    }
    bill_id
}

/// Processes due corporation bills whose `bill_type_id` has automatic payment enabled.
pub fn process_due_automatic_bills(ctx: &ServerContext) -> usize {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let bills: Vec<CorpBillRecord> = snap
            .owner_notes
            .values()
            .filter(|n| n.character_id == -71)
            .filter_map(|n| serde_json::from_str::<CorpBillRecord>(&n.note).ok())
            .filter(|b| !b.paid && b.due_unix <= now)
            .collect();
        let mut paid_count = 0;
        for mut bill in bills {
            let ap_nid = autopay_note_id(bill.debtor_id);
            let Some(ap_note) = snap.owner_notes.get(&ap_nid) else {
                continue;
            };
            let Ok(ap) = serde_json::from_str::<CorpAutoPaySettings>(&ap_note.note) else {
                continue;
            };
            if !ap.enabled_bill_types.get(&bill.bill_type_id).copied().unwrap_or(false) {
                continue;
            }
            let div_key = ap.division_id.clamp(1000, 1006);
            let div_id = (div_key - 999) as u8;
            let amt = IskCents(bill.amount_cents);
            let Some(corp) = snap.corporations.get_mut(&bill.debtor_id) else {
                continue;
            };
            corp.ensure_wallet_divisions();
            let can_pay = corp
                .wallet_divisions
                .iter()
                .find(|d| d.division_id == div_id)
                .map(|d| d.balance.0 >= amt.0 && corp.master_wallet.0 >= amt.0)
                .unwrap_or(false);
            if !can_pay {
                continue;
            }
            corp.master_wallet = corp.master_wallet.checked_sub(amt).unwrap_or(IskCents(0));
            if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                d.balance = d.balance.checked_sub(amt).unwrap_or(IskCents(0));
            }
            let debtor_bal = corp.master_wallet;
            snap.append_journal(
                bill.debtor_id,
                "BillPayment",
                amt,
                debtor_bal,
                format!("Automatic payment of bill {}", bill.bill_id),
            );
            if let Some(cred) = snap.corporations.get_mut(&bill.creditor_id) {
                cred.master_wallet = cred.master_wallet.checked_add(amt).unwrap_or(cred.master_wallet);
            }
            bill.paid = true;
            bill.paid_by_division = Some(div_key);
            save_json_note(
                snap,
                bill_note_id(bill.bill_id),
                -71,
                bill.bill_id,
                format!("bill:{}", bill.bill_id),
                &bill,
                now,
            );
            paid_count += 1;
        }
        paid_count
    })
}

/// Sets a solar system's Time Dilation factor (`0.10..=1.0`) (`feat:serverops.time-dilation`).
pub fn set_system_tidi_factor(ctx: &ServerContext, solar_system_id: u32, requested_factor: f64) -> f64 {
    let factor = requested_factor.clamp(0.10, 1.0);
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
        save_json_note(
            snap,
            nid,
            -75,
            solar_system_id as i64,
            format!("tidi:{solar_system_id}"),
            &factor,
            now,
        );
    });
    factor
}

/// Reads the current Time Dilation factor (`0.10..=1.0`, default `1.0`) for `solar_system_id`.
pub fn get_system_tidi_factor(ctx: &ServerContext, solar_system_id: u32) -> f64 {
    ctx.db.with_snapshot(|snap| {
        let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
        snap.owner_notes
            .get(&nid)
            .and_then(|n| serde_json::from_str::<f64>(&n.note).ok())
            .unwrap_or(1.0)
    })
}

/// Assigns or reinforces a solar system onto a cluster node (`feat:serverops.node-balancing`).
pub fn assign_or_reinforce_system_node(
    ctx: &ServerContext,
    solar_system_id: u32,
    dedicated_node_id: Option<u32>,
) -> u32 {
    let now = ctx.now_unix();
    let default_node = 1 + (solar_system_id % 4);
    ctx.db.with_mut_snapshot(|snap| {
        let nid = -(7_600_000_000_000_i64.saturating_add(solar_system_id as i64));
        if let Some(node_id) = dedicated_node_id {
            save_json_note(
                snap,
                nid,
                -76,
                solar_system_id as i64,
                format!("node:{solar_system_id}"),
                &node_id,
                now,
            );
            node_id
        } else {
            snap.owner_notes
                .get(&nid)
                .and_then(|n| serde_json::from_str::<u32>(&n.note).ok())
                .unwrap_or(default_node)
        }
    })
}

/// Executes the 11:00 UTC cluster downtime cycle and daily maintenance jobs (`feat:serverops.downtime`, `feat:serverops.daily-jobs`).
pub fn run_daily_downtime_and_jobs(ctx: &ServerContext) -> HashMap<String, i64> {
    let now = ctx.now_unix();
    let seconds_in_day = now % 86_400;
    let is_1100_utc_window = (11 * 3_600..11 * 3_600 + 900).contains(&seconds_in_day);

    let expired_raffles = sweep_expired_raffles(ctx) as i64;
    let auto_paid_bills = process_due_automatic_bills(ctx) as i64;
    let evacuated_abyssal = crate::services::pve_special_sites::evacuate_abyssal_pockets_for_downtime(ctx);
    crate::services::fleet_social::fleet_social_notify::prune_offline_fleets_and_channels(ctx);
    // parity: feat:lifecycle.wreck-downtime, feat:lifecycle.mission-downtime-respawn, plan:SP-7
    let (npc_wrecks_cleaned, mission_npcs_respawned) =
        crate::sp6_lifecycle::cleanup_npc_wrecks_and_reset_missions_on_downtime(ctx);

    let (replenished_asteroids, expired_order_list, expired_contracts, sov_decayed) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut ast_count = 0_i64;
            for item in snap.items.values_mut() {
                if (30_000_000..=50_000_000).contains(&item.location_id)
                    && ctx
                        .sde
                        .types
                        .get(&item.type_id)
                        .map(|t| t.category_id == 25)
                        .unwrap_or(false)
                {
                    let target_qty = item
                        .dynamic_attributes
                        .get(&1805)
                        .copied()
                        .unwrap_or(25_000.0) as i32;
                    if item.quantity < target_qty {
                        item.quantity = target_qty;
                        ast_count += 1;
                    }
                }
            }
            let expired_order_list = super::market_contracts::sweep_expired_orders(snap, now);
            let mut exp_contracts = 0_i64;
            for c in snap.contracts.values_mut() {
                if c.status == "Outstanding" && c.expiration_unix > 0 && c.expiration_unix <= now {
                    c.status = "Expired".into();
                    exp_contracts += 1;
                }
            }
            let mut sov_cnt = 0_i64;
            for sov in snap.system_states.values_mut() {
                if sov.sov_alliance_id.is_some() && sov.military_index > 1 {
                    sov.military_index -= 1;
                    sov_cnt += 1;
                }
            }
            (ast_count, expired_order_list, exp_contracts, sov_cnt)
        });
    let expired_orders = expired_order_list.len() as i64;
    super::market_contracts::emit_expired_order_notifications(ctx, &expired_order_list);
    if is_1100_utc_window {
        ctx.notify(
            crate::router::NotificationTarget::All,
            "OnClusterShutdownInitiated",
            "clientID",
            vec![
                PyRep::String("DailyClusterDowntime".into()),
                PyRep::Integer(now as i64),
            ],
        );
    }

    HashMap::from([
        ("is1100UtcWindow".into(), i64::from(is_1100_utc_window)),
        ("replenishedAsteroids".into(), replenished_asteroids),
        ("expiredMarketOrders".into(), expired_orders),
        ("expiredContracts".into(), expired_contracts),
        ("sovSystemsDecayed".into(), sov_decayed),
        ("autoPaidBills".into(), auto_paid_bills),
        ("expiredRaffles".into(), expired_raffles),
        ("evacuatedAbyssalPilots".into(), evacuated_abyssal),
        ("npcWrecksCleaned".into(), npc_wrecks_cleaned as i64),
        ("missionNpcsRespawned".into(), mission_npcs_respawned as i64),
    ])
}

fn bill_to_row(b: &CorpBillRecord) -> Vec<PyRep> {
    vec![
        PyRep::Integer(b.bill_id),
        PyRep::Integer(b.bill_type_id),
        PyRep::Integer(b.debtor_id),
        PyRep::Integer(b.creditor_id),
        PyRep::Real(IskCents(b.amount_cents).to_wire_f64()),
        PyRep::Integer(super::market_trading::unix_to_filetime(b.due_unix)),
        PyRep::Real(b.interest),
        PyRep::Integer(b.external_id),
        PyRep::Integer(i64::from(b.paid)),
        PyRep::Integer(b.external_id2),
    ]
}

fn bill_columns() -> Vec<&'static str> {
    vec![
        "billID",
        "billTypeID",
        "debtorID",
        "creditorID",
        "amount",
        "dueDateTime",
        "interest",
        "externalID",
        "paid",
        "externalID2",
    ]
}

/// NPC corporation that collects market sales tax (`ownerID2` of tax journal entries), looked up
/// by name in the SDE NPC corporation table.
pub fn tax_collector_id(ctx: &ServerContext) -> i64 {
    ctx.sde
        .npc_corporations
        .values()
        .find(|c| c.name == "Secure Commerce Commission")
        .map(|c| c.corporation_id)
        .unwrap_or(0)
}

/// Personal wallet `accountKey` used in `OnAccountChange` (same key as the `/giveisk` path).
const CASH_ACCOUNT_KEY: &str = "cash";

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = super::wallet_ledger::handle(ctx, session, service, method, args, _kwargs) {
        return Some(res);
    }
    if let Some(res) = wallet_services_ext::handle(ctx, session, service, method, args, _kwargs) {
        return Some(res);
    }
    match (service, method) {
        // --- Legacy Starbase (`posMgr`) stubs (`feat:removed.pos-towers`) ---
        ("posMgr", "GetControlTowers") => {
            // const-ok: legacy POS control towers are superseded by Upwell structures in V24.01
            Some(Ok(PyRep::List(Vec::new())))
        }
        ("posMgr", "GetMoonForTower") => {
            // const-ok: legacy POS moon processes are superseded by Upwell Moon Mining refineries in V24.01
            Some(Ok(PyRep::None))
        }
        ("posMgr", "GetMoonProcessInfoForTower") => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let prov = ctx.db.with_snapshot(|snap| snap.orbital_provision_types.get(&item_id).copied());
            Some(Ok(prov.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("posMgr", "SetSessionCheck") => {
            // const-ok: session check confirmation
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }
        ("posMgr", "AnchorOrbital") => Some(Err(rpc_err(
            "StarbaseRemoved",
            "Legacy POS starbases and control towers are superseded by Upwell structures in V24.01",
        ))),

        ("posMgr", "AssumeStructureControl") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                check_starbase_auth(snap, session, char_id, item_id)?;
                snap.starbase_settings.entry(item_id).or_default().active_controller_id = Some(char_id);
                Ok(())
            });
            if let Err(e) = res { return Some(Err(e)); }
            ctx.notify(NotificationTarget::Character(char_id), "OnAssumeStructureControl", "charid", vec![PyRep::Integer(char_id), PyRep::Integer(item_id)]);
            Some(Ok(PyRep::None))
        }

        ("posMgr", "RelinquishStructureControl") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                if let Some(st) = snap.starbase_settings.get_mut(&item_id) {
                    if st.active_controller_id == Some(char_id) { st.active_controller_id = None; }
                }
                Ok(())
            });
            if let Err(e) = res { return Some(Err(e)); }
            ctx.notify(NotificationTarget::Character(char_id), "OnRelinquishStructureControl", "charid", vec![PyRep::Integer(char_id), PyRep::Integer(item_id)]);
            Some(Ok(PyRep::None))
        }

        ("posMgr", "GetStarbasePermissions") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let (deploy_flags, usage_flags) = ctx.db.with_snapshot(|snap| {
                let settings = snap.starbase_settings.get(&item_id);
                let (anchor, unanchor, online, offline) = if let Some(s) = settings {
                    (
                        if s.anchor == "alliance" { 2 } else if s.anchor == "starbase" { 3 } else { 1 },
                        if s.unanchor == "alliance" { 2 } else if s.unanchor == "starbase" { 3 } else { 1 },
                        if s.online == "alliance" { 2 } else if s.online == "starbase" { 3 } else { 1 },
                        if s.offline == "alliance" { 2 } else if s.offline == "starbase" { 3 } else { 1 },
                    )
                } else {
                    (1, 1, 1, 1)
                };
                let deploy = PyRep::Object {
                    type_name: "util.Row".into(),
                    args: Box::new(PyRep::Tuple(vec![
                        PyRep::List(vec![PyRep::String("anchor".into()), PyRep::String("unanchor".into()), PyRep::String("online".into()), PyRep::String("offline".into())]),
                        PyRep::List(vec![PyRep::Integer(anchor), PyRep::Integer(unanchor), PyRep::Integer(online), PyRep::Integer(offline)]),
                    ])),
                };
                let usage = PyRep::rowset(
                    vec!["structureID", "viewput", "viewputtake", "use"],
                    vec![vec![PyRep::Integer(item_id), PyRep::Integer(anchor), PyRep::Integer(unanchor), PyRep::Integer(online)]],
                );
                (deploy, usage)
            });
            Some(Ok(PyRep::Tuple(vec![deploy_flags, usage_flags])))
        }

        ("posMgr", "SetStarbasePermissions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let flag_anchor = args.get(1).and_then(|a| a.as_str()).unwrap_or("corp").to_string();
            let flag_unanchor = args.get(2).and_then(|a| a.as_str()).unwrap_or("corp").to_string();
            let flag_online = args.get(3).and_then(|a| a.as_str()).unwrap_or("corp").to_string();
            let flag_offline = args.get(4).and_then(|a| a.as_str()).unwrap_or("corp").to_string();
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                check_starbase_auth(snap, session, char_id, item_id)?;
                let entry = snap.starbase_settings.entry(item_id).or_default();
                entry.anchor = flag_anchor;
                entry.unanchor = flag_unanchor;
                entry.online = flag_online;
                entry.offline = flag_offline;
                Ok(())
            });
            Some(res.map(|()| PyRep::None))
        }

        ("posMgr", "GetTowerNotificationSettings") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let (send_fuel, show_cal) = ctx.db.with_snapshot(|snap| {
                snap.starbase_settings.get(&item_id).map(|s| (s.notify_fuel, s.notify_calendar)).unwrap_or((true, false))
            });
            Some(Ok(PyRep::Object {
                type_name: "util.Row".into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::List(vec![PyRep::String("sendFuelNotifications".into()), PyRep::String("showInCalendar".into())]),
                    PyRep::List(vec![PyRep::Bool(send_fuel), PyRep::Bool(show_cal)]),
                ])),
            }))
        }

        ("posMgr", "SetTowerNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let show_calendar = matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let send_fuel = !matches!(args.get(2), Some(PyRep::Bool(false)) | Some(PyRep::Integer(0)));
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                check_starbase_auth(snap, session, char_id, item_id)?;
                let s = snap.starbase_settings.entry(item_id).or_default();
                s.notify_fuel = send_fuel;
                s.notify_calendar = show_calendar;
                Ok(())
            });
            Some(res.map(|()| PyRep::None))
        }

        ("posMgr", "GetTowerSentrySettings") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let (standing, status, status_drop, corp_war, standing_owner_id) = ctx.db.with_snapshot(|snap| {
                if let Some(s) = snap.starbase_settings.get(&item_id) {
                    (
                        s.attack_standing_threshold.unwrap_or(0.0),
                        s.attack_security_status_threshold.unwrap_or(0.0),
                        s.attack_if_other_security_status_dropping,
                        s.attack_if_at_war,
                        snap.characters.get(&session.character_id.unwrap_or(0)).map(|c| c.corporation_id).unwrap_or(98_000_001),
                    )
                } else {
                    (0.0, 0.0, false, true, 98_000_001)
                }
            });
            Some(Ok(PyRep::Object {
                type_name: "util.Row".into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::List(vec![
                        PyRep::String("standing".into()),
                        PyRep::String("status".into()),
                        PyRep::String("statusDrop".into()),
                        PyRep::String("corpWar".into()),
                        PyRep::String("standingOwnerID".into()),
                    ]),
                    PyRep::List(vec![
                        PyRep::Real(standing),
                        PyRep::Real(status),
                        PyRep::Bool(status_drop),
                        PyRep::Bool(corp_war),
                        PyRep::Integer(standing_owner_id),
                    ]),
                ])),
            }))
        }

        ("posMgr", "SetTowerSentrySettings") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let standing = args.get(1).and_then(|a| match a { PyRep::Real(r) => Some(*r), PyRep::Integer(i) => Some(*i as f64), _ => None }).unwrap_or(0.0);
            let status = args.get(2).and_then(|a| match a { PyRep::Real(r) => Some(*r), PyRep::Integer(i) => Some(*i as f64), _ => None }).unwrap_or(0.0);
            let status_drop = matches!(args.get(3), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let corp_war = matches!(args.get(4), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let use_alliance = matches!(args.get(5), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));

            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                check_starbase_auth(snap, session, char_id, item_id)?;
                let entry = snap.starbase_settings.entry(item_id).or_default();
                entry.use_alliance_standings = use_alliance;
                entry.attack_if_other_security_status_dropping = status_drop;
                entry.attack_if_at_war = corp_war;
                entry.attack_standing_threshold = Some(standing);
                entry.attack_security_status_threshold = Some(status);
                Ok(())
            });
            Some(res.map(|()| PyRep::None))
        }

        ("posMgr", "SetTowerPassword") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let password = args.get(1).and_then(|a| a.as_str()).unwrap_or("password");
            let allow_corp = matches!(args.get(2), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)) | None);
            let allow_alliance = matches!(args.get(3), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)) | None);

            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                check_starbase_auth(snap, session, char_id, item_id)?;
                let st = snap.starbase_settings.entry(item_id).or_default();
                st.password = Some(password.to_string());
                st.allow_corporation_members = allow_corp;
                st.allow_alliance_members = allow_alliance;
                Ok(())
            });
            Some(res.map(|()| PyRep::None))
        }

        ("posMgr", "GetConsumptionFactor") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let factor = ctx.db.with_snapshot(|snap| {
                let sys_id = session.solar_system_id.unwrap_or(30000142);
                if let Some(state) = snap.system_states.get(&sys_id) {
                    if state.sov_alliance_id.is_some() { 0.75 } else { 1.0 }
                } else {
                    1.0
                }
            });
            Some(Ok(PyRep::Real(factor)))
        }

        ("posMgr", "GetSiloCapacityByItemID") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let cap = ctx.db.with_snapshot(|snap| {
                snap.items.get(&item_id).and_then(|it| ctx.sde.types.get(&it.type_id)).map(|t| t.capacity).unwrap_or(20_000.0)
            });
            Some(Ok(PyRep::Real(cap)))
        }

        ("posMgr", "LinkResourceForTower") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let target_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
                Some(other) => other.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                check_starbase_auth(snap, session, char_id, tower_id)?;
                let tower_corp = snap.structures.get(&tower_id).map(|s| s.owner_corp_id).unwrap_or(0);
                let st = snap.starbase_settings.entry(tower_id).or_default();
                for tid in &target_ids {
                    if *tid > 0 {
                        let is_valid_target = if let Some(target_struct) = snap.structures.get(tid) {
                            target_struct.owner_corp_id == tower_corp
                        } else if let Some(item) = snap.items.get(tid) {
                            item.location_id == tower_id && item.type_id == 14343
                        } else {
                            false
                        };
                        if !is_valid_target {
                            return Err(rpc_err("InvalidLinkTarget", format!("Target {tid} is not a valid structure or silo for tower {tower_id}")));
                        }
                        if !st.linked_resources.contains(tid) {
                            st.linked_resources.push(*tid);
                        }
                    }
                }
                Ok(())
            });
            Some(res.map(|()| PyRep::None))
        }

        ("posMgr", "RunMoonProcessCycleforTower") => Some(Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: "posMgr.RunMoonProcessCycleforTower is not implemented".into(),
        })),

        ("posMgr", "ChangeStructureProvisionType") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let item_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100_000_002);
            let provision_type_raw = args.get(2).and_then(|a| a.as_i64()).unwrap_or(16634);
            let provision_type = match u32::try_from(provision_type_raw) {
                Ok(t) => t,
                Err(_) => return Some(Err(rpc_err("InvalidTypeID", "Provision type ID must be a valid positive 32-bit integer"))),
            };
            let is_valid_moon_mat = ctx.sde.types.get(&provision_type).map(|t| t.group_id == 427).unwrap_or(false);
            if !is_valid_moon_mat {
                return Some(Err(rpc_err("InvalidProvisionType", format!("Type {provision_type} is not a valid moon material (group 427)"))));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                let target = if snap.structures.contains_key(&item_id) {
                    item_id
                } else if let Some(item) = snap.items.get(&item_id) {
                    if item.location_id == tower_id && item.type_id == 14343 {
                        tower_id
                    } else {
                        return Err(rpc_err("InvalidTarget", format!("Target {item_id} is not an anchored silo for tower {tower_id}")));
                    }
                } else {
                    return Err(rpc_err("InvalidTarget", format!("Target {item_id} not found")));
                };
                check_starbase_auth(snap, session, char_id, target)?;
                snap.orbital_provision_types.insert(item_id, provision_type as i64);
                Ok(())
            });
            Some(res.map(|()| PyRep::None))
        }

        ("posMgr", "UnanchorOrbital") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidOrbitalID", "UnanchorOrbital requires orbital itemID")));
            };
            let sess_sys = session.solar_system_id.ok_or_else(|| rpc_err("NotInSpace", "UnanchorOrbital requires active solar system session"));
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<PyRep, BridgeRpcError> {
                let sys_id = sess_sys?;
                let ship_id = session.ship_id.unwrap_or(0);
                let has_ship = ship_id > 0 && snap.items.contains_key(&ship_id);
                let it = snap.items.get_mut(&item_id).ok_or_else(|| rpc_err("OrbitalNotFound", format!("Orbital item {item_id} not found")))?;
                if let Some(st) = snap.structures.get(&item_id) {
                    if st.solar_system_id != sys_id {
                        return Err(rpc_err("NotInSameSystem", format!("Orbital is located in system {}, caller in {sys_id}", st.solar_system_id)));
                    }
                    if let Some(ship_pos) = ctx.systems.get_ball_position_sync(sys_id, ship_id) {
                        let orbital_pos = glam::DVec3::from_array(st.position);
                        let dist = (ship_pos - orbital_pos).length();
                        if dist > 5000.0 {
                            return Err(rpc_err("OrbitalOutOfRange", format!("Ship is {dist:.1} m from orbital {item_id} (must be <= 5000 m)")));
                        }
                    }
                }
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if it.owner_id != char_id && it.owner_id != char_corp {
                    return Err(rpc_err("NotOrbitalOwner", format!("Character {char_id} is not authorized to unanchor orbital {item_id}")));
                }
                if it.owner_id == char_corp {
                    let has_dir = snap.corporations.get(&char_corp)
                        .map(|c| c.ceo_id == char_id || c.member_roles.get(&char_id).copied().unwrap_or(0) & (1 << 0) != 0 || session.corp_role & (1 << 0) != 0)
                        .unwrap_or(false);
                    if !has_dir {
                        return Err(rpc_err("RoleNotAuthorized", "Unanchoring corporate orbital structures requires Director or CEO role"));
                    }
                }
                if let Some(st) = snap.structures.get_mut(&item_id) { st.state = "Unanchored".into(); }
                if has_ship {
                    it.location_id = ship_id;
                    it.flag = InventoryFlag::Cargo;
                } else {
                    it.location_id = char_id;
                    it.flag = InventoryFlag::Hangar;
                }
                let tid = it.type_id;
                let q = it.quantity;
                Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(item_id)),
                    ("typeID", PyRep::Integer(tid as i64)),
                    ("ownerID", PyRep::Integer(char_id)),
                    ("quantity", PyRep::Integer(q as i64)),
                    ("state", PyRep::String("Unanchored".into())),
                ]))
            });
            Some(res)
        }

        // --- Corporation & Personal Bills (`billMgr`, `feat:wallet.bills`) ---
        ("billMgr", "get") => {
            // const-ok: empty bill list fallback when no active bills pending
            Some(Ok(PyRep::List(Vec::new())))
        }
        ("billMgr", "GetCorporationBills") | ("billMgr", "GetCorporationBillsReceivable") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_receivable = method == "GetCorporationBillsReceivable";
            let res = ctx.db.with_snapshot(|snap| {
                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let target_corp = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
                let corp_id = match target_corp {
                    Some(cid) if cid != caller_corp => {
                        return Err(rpc_err(
                            "CorpAccessDenied",
                            "Cannot query bills for a foreign corporation",
                        ));
                    }
                    Some(cid) => cid,
                    None => caller_corp,
                };
                if corp_id != 0 {
                    let corp = snap.corporations.get(&corp_id);
                    let is_ceo = corp.map(|c| c.ceo_id == char_id).unwrap_or(false);
                    let roles = corp
                        .and_then(|c| c.member_roles.get(&char_id).copied())
                        .unwrap_or(0);
                    let has_privs = is_ceo
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR)
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_ACCOUNTANT)
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT);
                    if !has_privs {
                        return Err(rpc_err(
                            "CorpRoleRequired",
                            "Director or accountant role required to view corporation bills",
                        ));
                    }
                }
                let mut bills: Vec<CorpBillRecord> = snap
                    .owner_notes
                    .values()
                    .filter(|n| n.character_id == -71)
                    .filter_map(|n| serde_json::from_str::<CorpBillRecord>(&n.note).ok())
                    .filter(|b| {
                        if is_receivable {
                            b.creditor_id == corp_id
                        } else {
                            !b.paid && (b.debtor_id == corp_id || b.debtor_id == char_id)
                        }
                    })
                    .collect();
                bills.sort_by_key(|b| b.bill_id);
                Ok(bills.iter().map(bill_to_row).collect())
            });
            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(bill_columns(), rows))),
                Err(e) => Some(Err(e)),
            }
        }
        ("billMgr", "CharPayBill") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(bill_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "CharPayBill requires billID")));
            };
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let nid = bill_note_id(bill_id);
                let Some(note) = snap.owner_notes.get(&nid) else {
                    return Err(rpc_err("BillNotFound", format!("Bill {bill_id} not found")));
                };
                let mut bill: CorpBillRecord = serde_json::from_str(&note.note)
                    .map_err(|_| rpc_err("BillNotFound", "Corrupt bill record"))?;
                if bill.paid {
                    return Err(rpc_err("BillAlreadyPaid", "Bill is already paid"));
                }
                let amt = IskCents(bill.amount_cents);
                let ch = snap
                    .characters
                    .get_mut(&char_id)
                    .ok_or_else(|| rpc_err("CharacterNotFound", "Character not found"))?;
                let new_bal = ch
                    .balance
                    .checked_sub(amt)
                    .ok_or_else(|| rpc_err("NotEnoughMoney", "Insufficient ISK to pay bill"))?;
                ch.balance = new_bal;
                snap.append_journal(
                    char_id,
                    "BillPayment",
                    amt,
                    new_bal,
                    format!("Paid bill {bill_id}"),
                );
                if let Some(cred) = snap.corporations.get_mut(&bill.creditor_id) {
                    cred.master_wallet = cred.master_wallet.checked_add(amt).unwrap_or(cred.master_wallet);
                }
                bill.paid = true;
                save_json_note(snap, nid, -71, bill_id, format!("bill:{bill_id}"), &bill, now);
                Ok(new_bal)
            });
            match res {
                Ok(bal) => {
                    let _ = ctx.notify_account_change(char_id, "cash", char_id, bal.to_wire_f64());
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("billMgr", "PayCorporationBill") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(bill_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "PayCorporationBill requires billID")));
            };
            let from_account_key = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("fromAccountKey").and_then(|v| v.as_i64()))
                .unwrap_or(1000)
                .clamp(1000, 1006) as u16;
            let div_id = (from_account_key - 999) as u8;
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let nid = bill_note_id(bill_id);
                let Some(note) = snap.owner_notes.get(&nid) else {
                    return Err(rpc_err("BillNotFound", format!("Bill {bill_id} not found")));
                };
                let mut bill: CorpBillRecord = serde_json::from_str(&note.note)
                    .map_err(|_| rpc_err("BillNotFound", "Corrupt bill record"))?;
                if bill.paid {
                    return Err(rpc_err("BillAlreadyPaid", "Bill is already paid"));
                }
                if bill.debtor_id != corp_id {
                    return Err(rpc_err("AccessDenied", "Bill does not belong to caller corporation"));
                }
                let amt = IskCents(bill.amount_cents);
                let corp = snap
                    .corporations
                    .get_mut(&corp_id)
                    .ok_or_else(|| rpc_err("CorporationNotFound", "Corporation not found"))?;
                let roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if corp.ceo_id != char_id
                    && !eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_ACCOUNTANT)
                    && !eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                {
                    return Err(rpc_err("CorpRoleRequired", "Accountant role required to pay corporation bills"));
                }
                corp.ensure_wallet_divisions();
                let div_bal = corp
                    .wallet_divisions
                    .iter()
                    .find(|d| d.division_id == div_id)
                    .map(|d| d.balance.min(corp.master_wallet))
                    .unwrap_or(IskCents(0));
                if div_bal.0 < amt.0 {
                    return Err(rpc_err("NotEnoughMoney", "Insufficient corporation division ISK to pay bill"));
                }
                corp.master_wallet = corp.master_wallet.checked_sub(amt).unwrap_or(IskCents(0));
                if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                    d.balance = d.balance.checked_sub(amt).unwrap_or(IskCents(0));
                }
                let bal_after = corp.master_wallet;
                snap.append_journal(
                    corp_id,
                    "BillPayment",
                    amt,
                    bal_after,
                    format!("Paid corporation bill {bill_id}"),
                );
                if let Some(cred) = snap.corporations.get_mut(&bill.creditor_id) {
                    cred.master_wallet = cred.master_wallet.checked_add(amt).unwrap_or(cred.master_wallet);
                }
                bill.paid = true;
                bill.paid_by_division = Some(from_account_key);
                save_json_note(snap, nid, -71, bill_id, format!("bill:{bill_id}"), &bill, now);
                Ok(())
            });
            Some(res.map(|()| PyRep::Bool(true)))
        }
        ("billMgr", "GetAutomaticPaySettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rep = ctx.db.with_snapshot(|snap| {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let ap = snap
                    .owner_notes
                    .get(&autopay_note_id(corp_id))
                    .and_then(|n| serde_json::from_str::<CorpAutoPaySettings>(&n.note).ok())
                    .unwrap_or(CorpAutoPaySettings {
                        division_id: 1000,
                        enabled_bill_types: BTreeMap::new(),
                    });
                let mut inner = Vec::new();
                for (bt, enabled) in ap.enabled_bill_types {
                    inner.push((PyRep::Integer(bt), PyRep::Bool(enabled)));
                }
                inner.push((
                    PyRep::String("divisionID".into()),
                    PyRep::Integer(ap.division_id.max(1000) as i64),
                ));
                PyRep::Dict(vec![(PyRep::Integer(corp_id), PyRep::Dict(inner))])
            });
            Some(Ok(rep))
        }
        ("billMgr", "SendAutomaticPaySettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let mut ap = snap
                    .owner_notes
                    .get(&autopay_note_id(corp_id))
                    .and_then(|n| serde_json::from_str::<CorpAutoPaySettings>(&n.note).ok())
                    .unwrap_or(CorpAutoPaySettings {
                        division_id: 1000,
                        enabled_bill_types: BTreeMap::new(),
                    });
                if let Some(PyRep::Dict(entries)) = args.first() {
                    for (k, v) in entries {
                        if let Some(bt) = k.as_i64() {
                            if let PyRep::Dict(inner) = v {
                                for (ik, iv) in inner {
                                    if let Some(ibt) = ik.as_i64() {
                                        ap.enabled_bill_types
                                            .insert(ibt, iv.as_bool().unwrap_or(false));
                                    } else if ik.as_str() == Some("divisionID") {
                                        ap.division_id = iv.as_i64().unwrap_or(1000) as u16;
                                    }
                                }
                            } else {
                                ap.enabled_bill_types
                                    .insert(bt, v.as_bool().unwrap_or(false));
                            }
                        } else if k.as_str() == Some("divisionID") {
                            ap.division_id = v.as_i64().unwrap_or(1000) as u16;
                        }
                    }
                }
                save_json_note(
                    snap,
                    autopay_note_id(corp_id),
                    -72,
                    corp_id,
                    format!("autopay:{corp_id}"),
                    &ap,
                    now,
                );
            });
            let _ = process_due_automatic_bills(ctx);
            Some(Ok(PyRep::Bool(true)))
        }
        ("account", "GiveCash") => Some(give_cash(ctx, session, args)),
        ("account", "SetContactCost") => Some(set_contact_cost(ctx, session, args)),
        ("account", "GetDefaultContactCost") => Some(get_default_contact_cost(ctx, session)),
        ("machoNet", "GetClusterGameStatisticsForClient") => Some(Ok(cluster_statistics(ctx))),
        ("pingService", "Ping") => Some(Ok(PyRep::Integer(ctx.now_blueos_filetime_ticks()))),
        ("localizationServer", "GetAllTextChanges") => Some(get_all_text_changes(ctx, session)),
        ("localizationServer", "UpdateLocalizationQAWrap") => Some(update_localization_qa_wrap(ctx, session, args)),
        ("clientStatLogger", "LogString")
        | ("infoGatheringMgr", "LogInfoEventsFromClient")
        | ("eveguard_report", "heartbeat_started")
        | ("eveguard_report", "heartbeat_stopped") => {
            if service == "clientStatLogger" || method == "LogInfoEventsFromClient" {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: format!("{service}.{method} is not implemented"),
                }));
            }
            // const-ok: fire-and-forget client telemetry sink returns None after logging
            debug!(
                session_id = session.session_id,
                char_id = ?session.character_id,
                service,
                method,
                arg_count = args.len(),
                "client telemetry received"
            );
            Some(Ok(PyRep::None))
        }
        ("infoGatheringMgr", "GetStateAndConfig") => Some(Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: "infoGatheringMgr.GetStateAndConfig is not implemented".into(),
        })),
        ("FastCheckoutService", "IsFastCheckoutEnabledForUser") => {
            // const-ok: real-money checkout is disabled on this server
            Some(Ok(PyRep::Bool(false)))
        }
        ("FastCheckoutService", "GetOffersForUser") => {
            // const-ok: real-money checkout offers list is empty because checkout is disabled
            Some(Ok(PyRep::List(Vec::new())))
        }
        ("FastCheckoutService", "BuyOffer") => Some(Err(rpc_err(
            "FastCheckoutDisabled",
            "Real-money PLEX checkout is disabled on this server",
        ))),
        ("FastCheckoutService", "ClearFastCheckoutCacheForUser") => {
            Some(Ok(PyRep::None))
        }
        ("FastCheckoutService", "GetTestingConfiguration") => {
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Bool(false),
                PyRep::String("".into()),
                PyRep::Bool(false),
            ])))
        }
        _ => None,
    }
}

fn give_cash(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let to_id = args
        .first()
        .and_then(|a| a.as_i64())
        .ok_or_else(|| rpc_err("InvalidArgument", "GiveCash requires toID"))?;
    let amount_isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
    let reason = args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string();
    if !amount_isk.is_finite() || amount_isk <= 0.0 {
        return Err(rpc_err("InvalidArgument", "GiveCash amount must be positive and finite"));
    }
    if to_id == char_id {
        return Err(rpc_err("InvalidArgument", "Cannot give ISK to yourself"));
    }
    let amount = IskCents::from_f64_isk(amount_isk);
    let now = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let target_is_char = snap.characters.contains_key(&to_id);
        if !target_is_char && !snap.corporations.contains_key(&to_id) {
            return Err(rpc_err("InvalidRecipient", format!("Recipient {to_id} not found")));
        }
        let sender = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| rpc_err("CharacterNotFound", format!("Character {char_id} not found")))?;
        let new_sender_bal = sender
            .balance
            .checked_sub(amount)
            .ok_or_else(|| rpc_err("NotEnoughMoney", "Not enough ISK in wallet for this transfer"))?;
        sender.balance = new_sender_bal;
        let journal_reason = if reason.is_empty() {
            format!("Player donation to {to_id}")
        } else {
            format!("Player donation to {to_id}: {reason}")
        };
        let meta = eve_db::ledger::JournalMeta::new(eve_db::ledger::ref_types::PLAYER_DONATION, char_id, to_id, now)
            .with_context(eve_db::ledger::JournalContext::Character, char_id);
        snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
            owner_id: char_id,
            entry_type: "PlayerDonation",
            amount: IskCents(-amount.0),
            balance_after: new_sender_bal,
            reason: journal_reason,
            meta: meta.clone(),
        });

        let recipient_bal = if target_is_char {
            let rec = snap.characters.get_mut(&to_id).expect("checked above");
            rec.balance = rec.balance.checked_add(amount).unwrap_or(rec.balance);
            rec.balance
        } else {
            let corp = snap.corporations.get_mut(&to_id).expect("checked above");
            corp.master_wallet = corp.master_wallet.checked_add(amount).unwrap_or(corp.master_wallet);
            corp.master_wallet
        };
        let rec_reason = if reason.is_empty() {
            format!("Player donation from {char_id}")
        } else {
            format!("Player donation from {char_id}: {reason}")
        };
        snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
            owner_id: to_id,
            entry_type: "PlayerDonation",
            amount,
            balance_after: recipient_bal,
            reason: rec_reason,
            meta,
        });
        Ok((new_sender_bal, target_is_char.then_some(recipient_bal)))
    })?;
    let (sender_bal, recipient_bal) = res;
    let _ = ctx.notify_account_change(char_id, CASH_ACCOUNT_KEY, char_id, sender_bal.to_wire_f64());
    if let Some(rb) = recipient_bal {
        let _ = ctx.notify_account_change(to_id, CASH_ACCOUNT_KEY, to_id, rb.to_wire_f64());
    }
    Ok(PyRep::Real(sender_bal.to_wire_f64()))
}

fn set_contact_cost(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let cost = match args.first() {
        None | Some(PyRep::None) => None,
        Some(v) => {
            let isk = v
                .as_f64()
                .ok_or_else(|| rpc_err("InvalidArgument", "SetContactCost expects an ISK amount or None"))?;
            if !isk.is_finite() || isk < 0.0 {
                return Err(rpc_err("InvalidArgument", "CSPA charge must be a non-negative ISK amount"));
            }
            Some(IskCents::from_f64_isk(isk))
        }
    };
    ctx.db.with_mut_snapshot(|snap| match cost {
        Some(c) if c.0 > 0 => {
            snap.character_cspa_charges.insert(char_id, c);
        }
        _ => {
            snap.character_cspa_charges.remove(&char_id);
        }
    });
    Ok(PyRep::None)
}

fn get_default_contact_cost(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let cost = ctx.db.with_snapshot(|snap| snap.character_cspa_charges.get(&char_id).copied());
    Ok(match cost {
        Some(c) => PyRep::Real(c.to_wire_f64()),
        None => PyRep::None,
    })
}

/// `(pilotsPerSystem, dockedPerSystem, statDivisor)` built from the live online-session registry.
/// DERIVED shape: mirrors the cluster session statistics tuple (total + docked per solarSystemID).
fn cluster_statistics(ctx: &ServerContext) -> PyRep {
    let mut totals: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    ctx.db.with_snapshot(|snap| {
        for ch in snap.characters.values() {
            if !ctx.is_character_online(ch.character_id, ch.user_id) {
                continue;
            }
            let entry = totals.entry(ch.solar_system_id as i64).or_insert((0, 0));
            entry.0 += 1;
            if ch.station_id.is_some() || ch.structure_id.is_some() {
                entry.1 += 1;
            }
        }
    });
    let pilots = totals
        .iter()
        .map(|(sys, (total, _))| (PyRep::Integer(*sys), PyRep::Integer(*total)))
        .collect();
    let docked = totals
        .iter()
        .map(|(sys, (_, d))| (PyRep::Integer(*sys), PyRep::Integer(*d)))
        .collect();
    PyRep::Tuple(vec![PyRep::Dict(pilots), PyRep::Dict(docked), PyRep::Real(1.0)])
}

const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

// GetAllTextChanges is a read-only query returning stored localization overrides.
fn get_all_text_changes(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_auth(session)?;
    ctx.db.with_snapshot(|snap| {
        let overrides: BTreeMap<i64, String> = snap
            .owner_notes
            .get(&LOCALIZATION_NOTE_ID)
            .and_then(|n| serde_json::from_str(&n.note).ok())
            .unwrap_or_default();
        if overrides.is_empty() {
            Ok(PyRep::None)
        } else {
            Ok(PyRep::Dict(
                overrides
                    .into_iter()
                    .map(|(id, txt)| (PyRep::Integer(id), PyRep::String(txt)))
                    .collect(),
            ))
        }
    })
}

fn update_localization_qa_wrap(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_auth(session)?;
    let qa_roles = eve_db::service_roles::ROLE_GMH
        | eve_db::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_PROGRAMMER
        | eve_db::service_roles::ROLE_CONTENT;
    if !session.has_gm_role() && (session.role & qa_roles) == 0 {
        return Err(BridgeRpcError {
            code: "RoleRequired".into(),
            message: "localizationServer.UpdateLocalizationQAWrap requires GM or Content/Programmer role".into(),
        });
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut overrides: BTreeMap<i64, String> = snap
            .owner_notes
            .get(&LOCALIZATION_NOTE_ID)
            .and_then(|n| serde_json::from_str(&n.note).ok())
            .unwrap_or_default();
        if let Some(PyRep::Dict(entries)) = args.first() {
            for (k, v) in entries {
                if let (Some(msg_id), Some(text)) = (k.as_i64(), v.as_str()) {
                    overrides.insert(msg_id, text.to_string());
                }
            }
        } else if args.first().and_then(PyRep::as_bool).is_some() {
            // Fixes R1-P1-4: Reject fabricated message 1 overrides; boolean QA-wrap toggle is not supported
            return Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "Boolean QA-wrap toggle is not supported in UpdateLocalizationQAWrap".into(),
            });
        }
        snap.owner_notes.insert(
            LOCALIZATION_NOTE_ID,
            eve_db::OwnerNoteRecord {
                note_id: LOCALIZATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: serde_json::to_string(&overrides).unwrap_or_default(),
                timestamp_unix: 0,
            },
        );
        Ok(PyRep::Bool(true))
    })
}
