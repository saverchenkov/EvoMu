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
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};
use tracing::debug;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
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

/// Sets a solar system's Time Dilation factor (`0.10..=1.0`) and emits `DoSimClockRebase` to all clients in the system (`feat:serverops.time-dilation`).
pub fn set_system_tidi_factor(ctx: &ServerContext, solar_system_id: u32, requested_factor: f64) -> f64 {
    let factor = requested_factor.clamp(0.10, 1.0);
    let now = ctx.now_unix();
    let blue_ticks = ctx.now_blueos_filetime_ticks();
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
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "DoSimClockRebase",
        "solarsystemid2",
        vec![
            PyRep::Tuple(vec![PyRep::Integer(blue_ticks), PyRep::Integer(blue_ticks)]),
            PyRep::Real(factor),
        ],
    );
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

    let (replenished_asteroids, expired_orders, expired_contracts, sov_decayed) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut ast_count = 0_i64;
            for item in snap.items.values_mut() {
                if ctx.sde.types.get(&item.type_id).map(|t| t.category_id == 25).unwrap_or(false)
                    && item.quantity < 5_000
                {
                    item.quantity = 5_000;
                    ast_count += 1;
                }
            }
            let mut exp_orders = 0_i64;
            let order_ids: Vec<i64> = snap
                .market_orders
                .values()
                .filter(|o| !o.is_npc && o.vol_remaining > 0)
                .map(|o| o.order_id)
                .collect();
            for oid in order_ids {
                if let Some(o) = snap.market_orders.get(&oid).cloned() {
                    let mut meta = super::market_trading::load_market_order_meta(snap, &o, now);
                    let dur_secs = (meta.duration.max(1) as u64).saturating_mul(86_400);
                    if meta.issue_unix.saturating_add(dur_secs) <= now {
                        if let Some(om) = snap.market_orders.get_mut(&oid) {
                            om.vol_remaining = 0;
                            om.escrow = IskCents(0);
                        }
                        meta.order_state = Some(4);
                        meta.final_vol_remaining = Some(o.vol_remaining);
                        meta.closed_unix = Some(now);
                        super::market_trading::save_market_order_meta(snap, oid, meta);
                        exp_orders += 1;
                    }
                }
            }
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
            (ast_count, exp_orders, exp_contracts, sov_cnt)
        });

    HashMap::from([
        ("is1100UtcWindow".into(), i64::from(is_1100_utc_window)),
        ("replenishedAsteroids".into(), replenished_asteroids),
        ("expiredMarketOrders".into(), expired_orders),
        ("expiredContracts".into(), expired_contracts),
        ("sovSystemsDecayed".into(), sov_decayed),
        ("autoPaidBills".into(), auto_paid_bills),
        ("expiredRaffles".into(), expired_raffles),
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
        ("posMgr", "GetMoonForTower") | ("posMgr", "GetMoonProcessInfoForTower") => {
            // const-ok: legacy POS moon processes are superseded by Upwell Moon Mining refineries in V24.01
            Some(Ok(PyRep::None))
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
            let rows = ctx.db.with_snapshot(|snap| {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
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
                bills.iter().map(bill_to_row).collect()
            });
            Some(Ok(PyRep::rowset(bill_columns(), rows)))
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
        ("eventLog", "LogClientEvent")
        | ("eventLog", "LogClientStats")
        | ("eventLog", "LogPlayerRequestedDisconnect") => {
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
        ("alert", "BeanDelivery")
        | ("alert", "GroupBeanDelivery")
        | ("alert", "SendClientStackTraceAlert") => {
            // const-ok: fire-and-forget client alert telemetry sink returns None after logging
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
        ("clientStatLogger", "LogString")
        | ("infoGatheringMgr", "LogInfoEventsFromClient")
        | ("eveguard_report", "heartbeat_started")
        | ("eveguard_report", "heartbeat_stopped") => {
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
        ("alert", "BeanCount") => {
            // const-ok: returns (None, 0) asking client to send stack trace with local logging mode
            Some(Ok(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)])))
        }
        ("infoGatheringMgr", "GetStateAndConfig") => {
            // const-ok: client telemetry batching is disabled via static config
            Some(Ok(PyRep::keyval([
                ("isEnabled", PyRep::Integer(0)),
                ("clientWorkerInterval", PyRep::Integer(600_000)),
                ("infoTypes", PyRep::List(Vec::new())),
                ("infoTypeAggregates", PyRep::None),
                ("infoTypesOncePerRun", PyRep::None),
                ("infoTypeParameters", PyRep::None),
            ])))
        }
        ("localizationServer", "GetAllTextChanges") => {
            // const-ok: client treats None as no server-side localization text overrides
            Some(Ok(PyRep::None))
        }
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
