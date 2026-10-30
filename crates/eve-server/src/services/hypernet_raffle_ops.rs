//! `hypernet_raffle_ops` — HyperNet Relay (`raffleProxy`) Service Handlers.
//!
//! Implements `feat:wallet.hypernet-relay`, `feat:wallet.hypernet-hypercore-sizing-expiry`,
//! `rpc:raffleProxy.CreateRaffle`, `rpc:raffleProxy.BuyRandomTickets`, and
//! `rpc:raffleProxy.GetActiveHistoricPrices` (alongside `BuyTicket`, `AwardItem`, `GetRaffle`,
//! `Grab`, `FilteredGrab`, `GetCreatedParticipated`, `GetMyActiveTickets`, `GetMyRaffleHistory`,
//! `AmIBanned`, and subscription handlers).

// parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::wallet_services_ext::save_json_note;
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::{BTreeMap, HashMap};

pub const HYPERCORE_TYPE_ID: TypeId = 52_568;
pub const ALLOWED_RAFFLE_SIZES: [u32; 4] = [8, 16, 48, 512];
pub const RAFFLE_DURATION_SECS: u64 = 72 * 3_600;
pub const RAFFLE_SALES_TAX_PERCENT: i64 = 5;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RaffleStateRecord {
    pub raffle_id: i64,
    pub creator_id: i64,
    pub station_id: i64,
    pub item_id: i64,
    pub type_id: TypeId,
    pub ticket_count: u32,
    pub ticket_price_cents: i64,
    pub hypercores_used: i32,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub sold_tickets: BTreeMap<u32, i64>,
    pub winning_ticket: Option<u32>,
    pub winner_id: Option<i64>,
    pub item_awarded: bool,
    pub expired: bool,
}

fn raffle_note_id(raffle_id: i64) -> i64 {
    -(7_400_000_000_000_i64.saturating_add(raffle_id.abs()))
}

/// Computes required HyperCores (`type_id = 52568`) proportional to estimated item value and node count (`8, 16, 48, 512`).
///
/// DATA GAP: CCP's exact server-side HyperCore coefficient curve is not present in V24 client FSD or public wiki
/// (`HyperNet_Relay.md:36` specifies only that HyperCore count scales with Total Cost set by the host).
/// DERIVED: `max(1, ceil(est_value_isk * (ticket_count / 512)^0.25 / 5_000_000))`.
pub fn compute_required_hypercores(est_value_isk: f64, ticket_count: u32) -> Result<i32, BridgeRpcError> {
    if !ALLOWED_RAFFLE_SIZES.contains(&ticket_count) {
        return Err(rpc_err(
            "InvalidRaffleTicketCount",
            format!("HyperNet node count {ticket_count} must be one of [8, 16, 48, 512]"),
        ));
    }
    if !est_value_isk.is_finite() || est_value_isk <= 0.0 {
        return Err(rpc_err("InvalidRafflePrice", "Estimated raffle value must be positive"));
    }
    let size_factor = (ticket_count as f64 / 512.0).powf(0.25);
    let cores = ((est_value_isk * size_factor) / 5_000_000.0).ceil() as i32;
    Ok(cores.max(1))
}

/// Sweeps expired HyperNet raffles (`now_unix >= expires_unix` and not sold out), refunding ticket buyers' ISK
/// and returning the escrowed item to the creator while HyperCores remain consumed (`HyperNet_Relay.md:42`).
pub fn sweep_expired_raffles(ctx: &ServerContext) -> usize {
    let now = ctx.now_unix();
    let expired_ids: Vec<(i64, i64)> = ctx.db.with_mut_snapshot(|snap| {
        let raffles: Vec<RaffleStateRecord> = snap
            .owner_notes
            .values()
            .filter(|n| n.character_id == -74)
            .filter_map(|n| serde_json::from_str::<RaffleStateRecord>(&n.note).ok())
            .filter(|r| !r.expired && r.winner_id.is_none() && now >= r.expires_unix)
            .collect();
        let mut out = Vec::new();
        for mut r in raffles {
            let price = IskCents(r.ticket_price_cents);
            for (&_ticket_idx, &buyer_id) in &r.sold_tickets {
                if let Some(ch) = snap.characters.get_mut(&buyer_id) {
                    ch.balance = ch.balance.checked_add(price).unwrap_or(ch.balance);
                    let bal = ch.balance;
                    snap.append_journal(
                        buyer_id,
                        "RaffleTicketRefund",
                        price,
                        bal,
                        format!("HyperNet ticket refund for expired raffle {}", r.raffle_id),
                    );
                }
            }
            if let Some(it) = snap.items.get_mut(&r.item_id) {
                it.owner_id = r.creator_id;
                it.location_id = r.station_id;
                it.flag = InventoryFlag::Hangar;
                it.version += 1;
            }
            r.expired = true;
            save_json_note(
                snap,
                raffle_note_id(r.raffle_id),
                -74,
                r.raffle_id,
                format!("raffle:{}", r.raffle_id),
                &r,
                now,
            );
            out.push((r.raffle_id, r.creator_id));
        }
        out
    });
    for (rid, creator_id) in &expired_ids {
        ctx.notify(
            NotificationTarget::Character(*creator_id),
            "OnRaffleFinishedServer",
            "charid",
            vec![PyRep::Integer(*rid), PyRep::None],
        );
    }
    expired_ids.len()
}

pub fn raffle_to_keyval(r: &RaffleStateRecord) -> PyRep {
    let remaining = r.ticket_count.saturating_sub(r.sold_tickets.len() as u32);
    let sold_dict: Vec<(PyRep, PyRep)> = r
        .sold_tickets
        .iter()
        .map(|(&idx, &owner_id)| (PyRep::Integer(idx as i64), PyRep::Integer(owner_id)))
        .collect();
    PyRep::keyval([
        ("raffle_id", PyRep::Integer(r.raffle_id)),
        ("raffleID", PyRep::Integer(r.raffle_id)),
        ("owner_id", PyRep::Integer(r.creator_id)),
        ("location_id", PyRep::Integer(r.station_id)),
        ("item_id", PyRep::Integer(r.item_id)),
        ("type_id", PyRep::Integer(r.type_id as i64)),
        ("ticket_count", PyRep::Integer(r.ticket_count as i64)),
        ("tickets_remaining", PyRep::Integer(remaining as i64)),
        (
            "ticket_price",
            PyRep::Real(IskCents(r.ticket_price_cents).to_wire_f64()),
        ),
        ("hypercores_used", PyRep::Integer(r.hypercores_used as i64)),
        ("expiration_time", PyRep::Integer(r.expires_unix as i64)),
        ("sold_tickets", PyRep::Dict(sold_dict)),
        (
            "winning_ticket",
            r.winning_ticket
                .map(|t| PyRep::Integer(t as i64))
                .unwrap_or(PyRep::None),
        ),
        (
            "winner_id",
            r.winner_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        ("is_expired", PyRep::Bool(r.expired)),
        ("is_finished", PyRep::Bool(r.winner_id.is_some() || r.expired)),
        ("item_awarded", PyRep::Bool(r.item_awarded)),
    ])
}

fn compute_active_historic_prices(
    ctx: &ServerContext,
    tid: TypeId,
) -> (f64, f64) {
    let _ = sweep_expired_raffles(ctx);
    ctx.db.with_snapshot(|snap| {
        let mut active_prices = Vec::new();
        let mut historic_prices = Vec::new();
        for note in snap.owner_notes.values() {
            if note.character_id != -74 {
                continue;
            }
            let Ok(r) = serde_json::from_str::<RaffleStateRecord>(&note.note) else {
                continue;
            };
            if r.type_id != tid {
                continue;
            }
            let total_isk = ((r.ticket_price_cents as f64) / 100.0) * (r.ticket_count as f64);
            let is_active =
                !r.expired && r.winner_id.is_none() && (r.sold_tickets.len() as u32) < r.ticket_count;
            if is_active {
                active_prices.push(total_isk);
            } else if r.winner_id.is_some() {
                historic_prices.push(total_isk);
            }
        }

        let sde_fallback = ctx
            .sde
            .types
            .get(&tid)
            .map(|t| t.base_price)
            .filter(|&p| p > 0.0)
            .unwrap_or(0.0);

        let active_avg = if !active_prices.is_empty() {
            active_prices.iter().sum::<f64>() / (active_prices.len() as f64)
        } else {
            let mut order_sum = 0.0_f64;
            let mut order_cnt = 0_usize;
            for ord in snap.market_orders.values() {
                if !ord.is_bid && ord.type_id == tid && ord.vol_remaining > 0 {
                    order_sum += ord.price.to_wire_f64();
                    order_cnt += 1;
                }
            }
            if order_cnt > 0 {
                order_sum / (order_cnt as f64)
            } else {
                sde_fallback
            }
        };

        let historic_avg = if !historic_prices.is_empty() {
            historic_prices.iter().sum::<f64>() / (historic_prices.len() as f64)
        } else {
            let mut weighted_sum = 0.0_f64;
            let mut total_vol = 0_i64;
            let mut day_sum = 0.0_f64;
            let mut day_cnt = 0_usize;
            for ((_reg_id, hist_tid), days) in &snap.market_history {
                if *hist_tid != tid {
                    continue;
                }
                for d in days {
                    if d.average > 0.0 {
                        if d.volume > 0 {
                            weighted_sum += d.average * (d.volume as f64);
                            total_vol += d.volume;
                        }
                        day_sum += d.average;
                        day_cnt += 1;
                    }
                }
            }
            if total_vol > 0 {
                weighted_sum / (total_vol as f64)
            } else if day_cnt > 0 {
                day_sum / (day_cnt as f64)
            } else {
                sde_fallback
            }
        };

        (active_avg, historic_avg)
    })
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("raffleProxy", "AmIBanned") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let exists = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::Bool(!exists)))
        }
        (
            "raffleProxy",
            "SubscribeToRaffle"
            | "UnsubscribeFromRaffle"
            | "SubscribeToTickets"
            | "UnSubscribeFromTickets",
        ) => {
            // const-ok: HyperNet client subscription notifications return None
            Some(Ok(PyRep::None))
        }
        ("raffleProxy", "GetActiveHistoricPrices") => {
            let tid_raw = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| {
                    kwargs
                        .get("type_id")
                        .or_else(|| kwargs.get("typeID"))
                        .and_then(|v| v.as_i64())
                });
            let Some(tid_i64) = tid_raw.filter(|&id| id > 0) else {
                return Some(Err(rpc_err(
                    "InvalidArgument",
                    "GetActiveHistoricPrices requires a positive type_id",
                )));
            };
            let (active_avg, historic_avg) = compute_active_historic_prices(ctx, tid_i64 as TypeId);
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Real(active_avg),
                PyRep::Real(historic_avg),
            ])))
        }
        ("raffleProxy", "CreateRaffle") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (item_id, ticket_count, ticket_price_isk, custom_duration) = match args.first() {
                Some(PyRep::KeyVal(kv)) => (
                    kv.get("item_id")
                        .or_else(|| kv.get("itemID"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    kv.get("ticket_count")
                        .or_else(|| kv.get("ticketCount"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(8) as u32,
                    kv.get("ticket_price")
                        .or_else(|| kv.get("ticketPrice"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    kv.get("duration")
                        .or_else(|| kv.get("duration_secs"))
                        .or_else(|| kv.get("durationSecs"))
                        .and_then(|v| v.as_i64()),
                ),
                Some(PyRep::Dict(kv)) => {
                    let fk = |k: &str| kv.iter().find(|(ek, _)| ek.as_str() == Some(k)).map(|(_, ev)| ev);
                    (
                        fk("item_id").or_else(|| fk("itemID")).and_then(|v| v.as_i64()).unwrap_or(0),
                        fk("ticket_count").or_else(|| fk("ticketCount")).and_then(|v| v.as_i64()).unwrap_or(8) as u32,
                        fk("ticket_price").or_else(|| fk("ticketPrice")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        fk("duration")
                            .or_else(|| fk("duration_secs"))
                            .or_else(|| fk("durationSecs"))
                            .and_then(|v| v.as_i64()),
                    )
                }
                _ => (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(8) as u32,
                    args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0),
                    args.get(3).and_then(|a| a.as_i64()),
                ),
            };
            let duration_secs = match custom_duration {
                Some(d) if d <= 0 || d > (RAFFLE_DURATION_SECS as i64) => {
                    return Some(Err(rpc_err(
                        "InvalidRaffleDuration",
                        "HyperNet raffle duration must be between 1 second and 3 days (259200s)",
                    )));
                }
                Some(d) => d as u64,
                None => RAFFLE_DURATION_SECS,
            };
            if session.structure_id.is_some() {
                return Some(Err(rpc_err(
                    "InvalidRaffleLocation",
                    "HyperNet raffles can only be created in NPC stations",
                )));
            }
            let est_value_isk = ticket_price_isk * (ticket_count as f64);
            let required_cores = match compute_required_hypercores(est_value_isk, ticket_count) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let ticket_price_cents = IskCents::from_f64_isk(ticket_price_isk).0;
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let ch_rec = snap.characters.get(&char_id);
                if ch_rec.is_some_and(|c| c.structure_id.is_some()) {
                    return Err(rpc_err(
                        "InvalidRaffleLocation",
                        "HyperNet raffles can only be created in NPC stations",
                    ));
                }
                let char_station_id = ch_rec.and_then(|c| c.station_id);
                let Some(station_id) = session.station_id.or(char_station_id) else {
                    return Err(rpc_err(
                        "InvalidRaffleLocation",
                        "Must be docked in an NPC station to create a HyperNet raffle",
                    ));
                };
                if station_id >= 1_000_000_000_000 || !ctx.sde.stations.contains_key(&station_id) {
                    return Err(rpc_err(
                        "InvalidRaffleLocation",
                        format!("Location {station_id} is not an NPC station"),
                    ));
                }
                let item = snap
                    .items
                    .get(&item_id)
                    .cloned()
                    .ok_or_else(|| rpc_err("ItemNotFound", "Raffle item not found"))?;
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id);
                let has_contents = snap
                    .items
                    .values()
                    .any(|child| child.item_id != item_id && child.location_id == item_id && child.quantity > 0);
                // HyperNet_Relay.md:34: item must be a single unstacked item in an NPC station hangar.
                if item.owner_id != char_id
                    || item.location_id != station_id
                    || item.flag != InventoryFlag::Hangar
                    || item.quantity != 1
                    || Some(item_id) == active_ship_id
                    || snap.is_item_locked_in_contract_or_job(item_id)
                    || has_contents
                {
                    return Err(rpc_err(
                        "InvalidRaffleItem",
                        "Raffle item must be a single unstacked, unlocked, empty item in NPC station hangar (not active ship)",
                    ));
                }
                let core_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == station_id
                            && i.flag == InventoryFlag::Hangar
                            && i.type_id == HYPERCORE_TYPE_ID
                            && i.quantity >= required_cores
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.item_id)
                    .ok_or_else(|| {
                        rpc_err(
                            "InsufficientHyperCores",
                            format!("Need {required_cores} HyperCores (type 52568) in station hangar"),
                        )
                    })?;
                if let Some(ci) = snap.items.get_mut(&core_item_id) {
                    if ci.quantity > required_cores {
                        ci.quantity -= required_cores;
                        ci.version += 1;
                    } else {
                        snap.items.remove(&core_item_id);
                    }
                }
                // Move item into HyperNet escrow
                if let Some(it) = snap.items.get_mut(&item_id) {
                    it.owner_id = -52_568;
                    it.location_id = -52_568;
                    it.flag = InventoryFlag::Hangar;
                    it.version += 1;
                }
                snap.next_entity_id += 1;
                let raffle_id = snap.next_entity_id;
                let rec = RaffleStateRecord {
                    raffle_id,
                    creator_id: char_id,
                    station_id,
                    item_id,
                    type_id: item.type_id,
                    ticket_count,
                    ticket_price_cents,
                    hypercores_used: required_cores,
                    created_unix: now,
                    expires_unix: now.saturating_add(duration_secs),
                    sold_tickets: BTreeMap::new(),
                    winning_ticket: None,
                    winner_id: None,
                    item_awarded: false,
                    expired: false,
                };
                save_json_note(
                    snap,
                    raffle_note_id(raffle_id),
                    -74,
                    raffle_id,
                    format!("raffle:{raffle_id}"),
                    &rec,
                    now,
                );
                Ok(rec)
            });
            match res {
                Ok(rec) => {
                    let kv = raffle_to_keyval(&rec);
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnRaffleCreatedServer",
                        "charid",
                        vec![kv.clone()],
                    );
                    Some(Ok(kv))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("raffleProxy", "BuyTicket" | "BuyRandomTickets") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = sweep_expired_raffles(ctx);
            let Some(raffle_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "Raffle ticket purchase requires raffle_id")));
            };
            let Some(second_arg) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "Raffle ticket purchase requires ticket index or count")));
            };
            let now = ctx.now_unix();
            let rng_entropy_draw = ctx.mechanic_rng.try_next_u64().unwrap_or(now);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let nid = raffle_note_id(raffle_id);
                let Some(note) = snap.owner_notes.get(&nid) else {
                    return Err(rpc_err("RaffleNotFound", "Raffle not found"));
                };
                let mut r: RaffleStateRecord = serde_json::from_str(&note.note)
                    .map_err(|_| rpc_err("RaffleNotFound", "Corrupt raffle"))?;
                if r.expired || r.winner_id.is_some() {
                    return Err(rpc_err("RaffleFinished", "Raffle is already finished or expired"));
                }
                let mut tickets_to_buy = Vec::new();
                if method == "BuyTicket" {
                    if second_arg < 0 {
                        return Err(rpc_err("TicketUnavailable", "Ticket index must be non-negative"));
                    }
                    let t_idx = second_arg as u32;
                    if t_idx >= r.ticket_count || r.sold_tickets.contains_key(&t_idx) {
                        return Err(rpc_err("TicketUnavailable", "Requested ticket is unavailable"));
                    }
                    tickets_to_buy.push(t_idx);
                } else {
                    if second_arg <= 0 {
                        return Err(rpc_err("InvalidArgument", "BuyRandomTickets count must be at least 1"));
                    }
                    let count = second_arg as usize;
                    let mut unsold: Vec<u32> = (0..r.ticket_count)
                        .filter(|idx| !r.sold_tickets.contains_key(idx))
                        .collect();
                    if unsold.len() < count {
                        return Err(rpc_err("NotEnoughTickets", "Not enough remaining tickets in raffle"));
                    }
                    // Randomly select `count` unsold ticket indices using MechanicRng Fisher-Yates partial shuffle
                    let sel_seed = rng_entropy_draw
                        ^ (raffle_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        ^ (char_id as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
                        ^ (r.sold_tickets.len() as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
                    let sel_rng = crate::rng::MechanicRng::seeded(sel_seed);
                    for i in 0..count {
                        let rem = unsold.len() - i;
                        let draw = sel_rng.try_next_u64().unwrap_or(i as u64);
                        let swap_idx = i + ((draw as usize) % rem);
                        unsold.swap(i, swap_idx);
                    }
                    tickets_to_buy.extend_from_slice(&unsold[..count]);
                }
                let total_cost = IskCents(r.ticket_price_cents)
                    .checked_mul_qty(tickets_to_buy.len() as i64)
                    .ok_or_else(|| rpc_err("RaffleOverflow", "Ticket cost overflow"))?;
                let buyer = snap
                    .characters
                    .get_mut(&char_id)
                    .ok_or_else(|| rpc_err("CharacterNotFound", "Buyer not found"))?;
                let new_bal = buyer
                    .balance
                    .checked_sub(total_cost)
                    .ok_or_else(|| rpc_err("NotEnoughMoney", "Insufficient ISK to buy HyperNet tickets"))?;
                buyer.balance = new_bal;
                snap.append_journal(
                    char_id,
                    "RaffleTicketPurchase",
                    total_cost,
                    new_bal,
                    format!("Purchased {} ticket(s) in raffle {raffle_id}", tickets_to_buy.len()),
                );
                for t_idx in tickets_to_buy {
                    r.sold_tickets.insert(t_idx, char_id);
                }
                if r.sold_tickets.len() as u32 == r.ticket_count {
                    // Draw winner using MechanicRng seeded with raffle entropy from purchased ticket pool
                    let raffle_entropy = (raffle_id as u64)
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(r.created_unix)
                        .wrapping_add(rng_entropy_draw);
                    let rng = crate::rng::MechanicRng::seeded(raffle_entropy);
                    let purchased_tickets: Vec<u32> = r.sold_tickets.keys().copied().collect();
                    let win_idx = if !purchased_tickets.is_empty() {
                        let draw = rng.try_next_u64().unwrap_or(0);
                        purchased_tickets[(draw as usize) % purchased_tickets.len()]
                    } else {
                        0
                    };
                    let winner_id = r.sold_tickets.get(&win_idx).copied().unwrap_or(char_id);
                    r.winning_ticket = Some(win_idx);
                    r.winner_id = Some(winner_id);
                    let total_pot = IskCents(r.ticket_price_cents)
                        .checked_mul_qty(r.ticket_count as i64)
                        .unwrap_or(IskCents(0));
                    let fee_cents = (total_pot.0.saturating_mul(RAFFLE_SALES_TAX_PERCENT)) / 100;
                    let creator_payout = total_pot
                        .checked_sub(IskCents(fee_cents))
                        .unwrap_or(IskCents(0));
                    if let Some(creator) = snap.characters.get_mut(&r.creator_id) {
                        creator.balance = creator.balance.checked_add(creator_payout).unwrap_or(creator.balance);
                        let c_bal = creator.balance;
                        snap.append_journal(
                            r.creator_id,
                            "RafflePayout",
                            creator_payout,
                            c_bal,
                            format!("HyperNet payout (minus 5% fee) for sold-out raffle {raffle_id}"),
                        );
                    }
                }
                save_json_note(snap, nid, -74, raffle_id, format!("raffle:{raffle_id}"), &r, now);
                Ok(r)
            });
            match res {
                Ok(r) => {
                    let kv = raffle_to_keyval(&r);
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnRaffleUpdatedServer",
                        "charid",
                        vec![kv.clone()],
                    );
                    if let Some(winner_id) = r.winner_id {
                        ctx.notify(
                            NotificationTarget::Character(winner_id),
                            "OnRaffleFinishedServer",
                            "charid",
                            vec![PyRep::Integer(r.raffle_id), PyRep::Integer(winner_id)],
                        );
                    }
                    Some(Ok(kv))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("raffleProxy", "AwardItem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(raffle_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "AwardItem requires raffle_id")));
            };
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let nid = raffle_note_id(raffle_id);
                let Some(note) = snap.owner_notes.get(&nid) else {
                    return Err(rpc_err("RaffleNotFound", "Raffle not found"));
                };
                let mut r: RaffleStateRecord = serde_json::from_str(&note.note)
                    .map_err(|_| rpc_err("RaffleNotFound", "Corrupt raffle"))?;
                if r.winner_id != Some(char_id) {
                    return Err(rpc_err("NotRaffleWinner", "Only the raffle winner can claim the item"));
                }
                if r.item_awarded {
                    return Err(rpc_err("AlreadyAwarded", "Raffle item already claimed"));
                }
                if let Some(it) = snap.items.get_mut(&r.item_id) {
                    it.owner_id = char_id;
                    it.location_id = r.station_id;
                    it.flag = InventoryFlag::Hangar;
                    it.version += 1;
                }
                r.item_awarded = true;
                save_json_note(snap, nid, -74, raffle_id, format!("raffle:{raffle_id}"), &r, now);
                Ok(r.item_id)
            });
            Some(res.map(PyRep::Integer))
        }
        ("raffleProxy", "GetRaffle") => {
            let _ = sweep_expired_raffles(ctx);
            let raffle_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let rep = ctx.db.with_snapshot(|snap| {
                snap.owner_notes
                    .get(&raffle_note_id(raffle_id))
                    .and_then(|n| serde_json::from_str::<RaffleStateRecord>(&n.note).ok())
                    .map(|r| raffle_to_keyval(&r))
                    .unwrap_or(PyRep::None)
            });
            Some(Ok(rep))
        }
        (
            "raffleProxy",
            "Grab"
            | "FilteredGrab"
            | "GetCreatedParticipated"
            | "GetMyActiveTickets"
            | "GetMyRaffleHistory",
        ) => {
            let char_id = session.character_id.unwrap_or(0);
            let _ = sweep_expired_raffles(ctx);
            let get_filter = |k1: &str, k2: &str| -> Option<PyRep> {
                if let Some(v) = kwargs.get(k1).or_else(|| kwargs.get(k2)) {
                    return Some(v.clone());
                }
                if let Some(arg0) = args.first() {
                    if let Some(v) = arg0.dict_get(k1).or_else(|| arg0.dict_get(k2)) {
                        return Some(v.clone());
                    }
                }
                None
            };
            let f_type = get_filter("type_id", "typeID").and_then(|v| v.as_i64());
            let f_sys = get_filter("solar_system_id", "solarSystemID").and_then(|v| v.as_i64());
            let f_reg = get_filter("region_id", "regionID").and_then(|v| v.as_i64());
            let f_min_p = get_filter("min_price", "minPrice").or_else(|| get_filter("min_ticket_price", "minTicketPrice")).and_then(|v| v.as_f64());
            let f_max_p = get_filter("max_price", "maxPrice").or_else(|| get_filter("max_ticket_price", "maxTicketPrice")).and_then(|v| v.as_f64());
            let f_tcnt = get_filter("ticket_count", "ticketCount").and_then(|v| v.as_i64());
            let f_bpt = get_filter("blueprint_type", "blueprintType").and_then(|v| v.as_i64());
            let f_size = get_filter("size", "page_size").or_else(|| args.get(1).cloned()).and_then(|v| v.as_i64()).filter(|&n| n > 0);
            let list = ctx.db.with_snapshot(|snap| {
                let mut recs: Vec<RaffleStateRecord> = snap
                    .owner_notes
                    .values()
                    .filter(|n| n.character_id == -74)
                    .filter_map(|n| serde_json::from_str::<RaffleStateRecord>(&n.note).ok())
                    .filter(|r| {
                        let is_active = !r.expired && r.winner_id.is_none() && (r.sold_tickets.len() as u32) < r.ticket_count;
                        let is_mine = r.creator_id == char_id || r.sold_tickets.values().any(|&cid| cid == char_id);
                        match method {
                            "Grab" | "FilteredGrab" => {
                                if !is_active {
                                    return false;
                                }
                                if f_type.is_some_and(|t| r.type_id as i64 != t) {
                                    return false;
                                }
                                if f_tcnt.is_some_and(|tc| r.ticket_count as i64 != tc) {
                                    return false;
                                }
                                let t_price = (r.ticket_price_cents as f64) / 100.0;
                                if f_min_p.is_some_and(|mp| t_price < mp) || f_max_p.is_some_and(|mp| t_price > mp) {
                                    return false;
                                }
                                let (st_sys, st_reg) = ctx.sde.stations.get(&r.station_id)
                                    .map(|st| (st.solar_system_id as i64, st.region_id as i64))
                                    .or_else(|| snap.structures.get(&r.station_id).map(|s| {
                                        let reg = ctx.sde.solar_systems.get(&s.solar_system_id).map(|sys| sys.region_id as i64).unwrap_or(0);
                                        (s.solar_system_id as i64, reg)
                                    }))
                                    .unwrap_or((30_000_142, 10_000_002));
                                if f_sys.is_some_and(|s| st_sys != s) || f_reg.is_some_and(|rg| st_reg != rg) {
                                    return false;
                                }
                                if let Some(bpt) = f_bpt.filter(|&b| b > 0) {
                                    let is_bpc = snap.items.get(&r.item_id).is_some_and(|i| i.is_blueprint_copy);
                                    if (bpt == 1 && is_bpc) || (bpt == 2 && !is_bpc) {
                                        return false;
                                    }
                                }
                                true
                            }
                            "GetMyActiveTickets" => is_active && r.sold_tickets.values().any(|&cid| cid == char_id),
                            "GetCreatedParticipated" | "GetMyRaffleHistory" => is_mine,
                            _ => true,
                        }
                    })
                    .collect();
                recs.sort_by_key(|r| r.raffle_id);
                if let Some(sz) = f_size {
                    recs.truncate(sz as usize);
                }
                recs.into_iter().map(|r| raffle_to_keyval(&r)).collect()
            });
            Some(Ok(PyRep::List(list)))
        }
        _ => None,
    }
}
