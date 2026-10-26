//! `wallet_services_ext` — Corporation Bills (`billMgr`), New Eden Store (`storeManager`),
//! Station Direct Trade (`trademgr`), HyperNet Relay (`raffleProxy`), Legacy Starbase (`posMgr`)
//! stubs, and Cluster Operations (`TiDi`, Node Balancing, Daily Downtime Jobs).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::{BTreeMap, HashMap};

pub const HYPERCORE_TYPE_ID: TypeId = 52_568;
pub const ALLOWED_RAFFLE_SIZES: [u32; 4] = [8, 16, 48, 512];
pub const RAFFLE_DURATION_SECS: u64 = 72 * 3_600;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorpBillRecord {
    pub bill_id: i64,
    pub bill_type_id: i64,
    pub debtor_id: i64,
    pub creditor_id: i64,
    pub amount_cents: i64,
    pub due_unix: u64,
    pub interest: f64,
    pub external_id: i64,
    pub external_id2: i64,
    pub paid: bool,
    pub paid_by_division: Option<u16>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CorpAutoPaySettings {
    pub division_id: u16,
    pub enabled_bill_types: BTreeMap<i64, bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TradeSessionState {
    pub trade_id: i64,
    pub station_id: i64,
    pub char_a: i64,
    pub char_b: i64,
    pub items_a: Vec<(i64, i32)>,
    pub items_b: Vec<(i64, i32)>,
    pub money_a_cents: i64,
    pub money_b_cents: i64,
    pub accept_a: bool,
    pub accept_b: bool,
    pub completed: bool,
    pub cancelled: bool,
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

pub(super) fn bill_note_id(bill_id: i64) -> i64 {
    -(7_100_000_000_000_i64.saturating_add(bill_id.abs()))
}

pub(super) fn autopay_note_id(corp_id: i64) -> i64 {
    -(7_200_000_000_000_i64.saturating_add(corp_id.abs()))
}

fn trade_note_id(trade_id: i64) -> i64 {
    -(7_300_000_000_000_i64.saturating_add(trade_id.abs()))
}

fn raffle_note_id(raffle_id: i64) -> i64 {
    -(7_400_000_000_000_i64.saturating_add(raffle_id.abs()))
}

pub(super) fn save_json_note<T: serde::Serialize>(
    snap: &mut eve_db::WorldStateSnapshot,
    note_id: i64,
    owner_tag: i64,
    item_id: i64,
    label: String,
    val: &T,
    now_unix: u64,
) {
    if let Ok(note) = serde_json::to_string(val) {
        snap.owner_notes.insert(
            note_id,
            eve_db::OwnerNoteRecord {
                note_id,
                character_id: owner_tag,
                item_id,
                label,
                note,
                timestamp_unix: now_unix,
            },
        );
    }
}

/// Computes required HyperCores (`type_id = 52568`) proportional to estimated item value and node count (`8, 16, 48, 512`).
/// DERIVED scaling formula: `max(1, ceil(est_value_isk * (ticket_count / 512)^0.25 / 5_000_000))`.
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
/// and returning the escrowed item to the creator while HyperCores remain consumed (`feat:wallet.hypernet-hypercore-sizing-expiry`).
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
            // Refund ticket buyers
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
            // Return escrowed item to creator's station hangar
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

fn raffle_to_keyval(r: &RaffleStateRecord) -> PyRep {
    let remaining = r.ticket_count.saturating_sub(r.sold_tickets.len() as u32);
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

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // --- New Eden Store (`storeManager`, `feat:wallet.nes-store` & `feat:removed.aurum-currency`) ---
        ("storeManager", "get_categories") => {
            let catalog = ctx
                .db
                .with_snapshot(crate::services::quasar_payment_store::read_store_catalog);
            let items: Vec<PyRep> = catalog
                .categories
                .into_iter()
                .map(|c| {
                    PyRep::keyval([
                        ("id", PyRep::Integer(c.id as i64)),
                        ("name", PyRep::String(c.name)),
                        ("description", PyRep::String(c.description)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(items)))
        }
        ("storeManager", "get_products") => {
            let catalog = ctx
                .db
                .with_snapshot(crate::services::quasar_payment_store::read_store_catalog);
            let prods: Vec<PyRep> = catalog
                .products
                .into_iter()
                .map(|p| {
                    PyRep::keyval([
                        ("id", PyRep::Integer(p.id as i64)),
                        ("productID", PyRep::Integer(p.id as i64)),
                        ("name", PyRep::String(p.name)),
                        ("description", PyRep::String(p.description)),
                        ("typeID", PyRep::Integer(p.type_id as i64)),
                        ("quantity", PyRep::Integer(p.quantity as i64)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(prods)))
        }
        ("storeManager", "get_offers") => {
            let catalog = ctx
                .db
                .with_snapshot(crate::services::quasar_payment_store::read_store_catalog);
            let offers: Vec<PyRep> = catalog
                .offers
                .into_iter()
                .filter(|o| !o.targeted)
                .map(|o| {
                    let prod_reps: Vec<PyRep> = o
                        .product_ids
                        .iter()
                        .map(|pid| PyRep::Integer(*pid as i64))
                        .collect();
                    PyRep::keyval([
                        ("id", PyRep::Integer(o.id as i64)),
                        ("offerID", PyRep::Integer(o.id as i64)),
                        ("name", PyRep::String(o.name)),
                        ("description", PyRep::String(o.description)),
                        ("categoryID", PyRep::Integer(o.category_id as i64)),
                        ("currency", PyRep::String("PLX".into())),
                        ("price", PyRep::Integer(o.price_plex)),
                        ("plexPrice", PyRep::Integer(o.price_plex)),
                        ("products", PyRep::List(prod_reps)),
                        ("quantity", PyRep::Integer(1)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(offers)))
        }
        ("storeManager", "can_purchase_offer") | ("storeManager", "buy_offer") => {
            let (user_id, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let offer_id = args
                .first()
                .and_then(|a| match a {
                    PyRep::Integer(i) => Some(*i),
                    PyRep::String(s) => s.trim_start_matches("VEVC-").parse::<i64>().ok(),
                    _ => None,
                })
                .unwrap_or(0);
            let currency = args
                .get(1)
                .and_then(|a| a.as_str())
                .or_else(|| _kwargs.get("currency").and_then(|v| v.as_str()))
                .unwrap_or("PLX");
            if currency.eq_ignore_ascii_case("AUR") || currency.eq_ignore_ascii_case("AURUM") {
                return Some(Err(rpc_err(
                    "AurumCurrencyRemoved",
                    "Legacy Aurum (AUR) currency was removed; New Eden Store offers use PLEX (PLX)",
                )));
            }
            let qty = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("quantity").and_then(|v| v.as_i64()))
                .unwrap_or(1)
                .max(1);

            let catalog = ctx
                .db
                .with_snapshot(crate::services::quasar_payment_store::read_store_catalog);
            let offer = match catalog.offers.iter().find(|o| o.id as i64 == offer_id) {
                Some(o) => o.clone(),
                None => {
                    return Some(Err(rpc_err(
                        "OfferNotFound",
                        format!("NES offer {offer_id} not found"),
                    )))
                }
            };
            if offer.targeted && user_id == 0 {
                return Some(Err(rpc_err(
                    "TargetedOfferNotEligible",
                    format!("Character {char_id} is not eligible for targeted offer {offer_id}"),
                )));
            }
            let total_plex = offer.price_plex.saturating_mul(qty);
            if method == "can_purchase_offer" {
                let can_buy = ctx
                    .db
                    .with_snapshot(|snap| snap.plex_vault_balance(char_id) >= total_plex);
                return Some(Ok(PyRep::Bool(can_buy)));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let ch = snap.characters.get(&char_id).ok_or_else(|| rpc_err("CharacterNotFound", "character not found"))?;
                let station_id = session.station_id.or(session.structure_id).unwrap_or(ch.home_station_id);
                let cur_plex = snap.plex_vault_balance(char_id);
                if cur_plex < total_plex {
                    return Err(rpc_err(
                        "InsufficientPlexBalance",
                        format!("Need {total_plex} PLEX in vault, have {cur_plex}"),
                    ));
                }
                let rem_plex = snap
                    .adjust_plex_vault(char_id, -total_plex)
                    .ok_or_else(|| rpc_err("InsufficientPlexBalance", "Insufficient PLEX in vault"))?;
                snap.next_entity_id += 1;
                let tx_id = snap.next_entity_id as u64;
                crate::services::quasar_payment_store::record_plex_transaction(
                    snap,
                    tx_id,
                    user_id,
                    -total_plex,
                    rem_plex,
                    &format!("NES purchase: {}", offer.name),
                    ctx.now_unix(),
                );

                let mut delivered_item_id = 0_i64;
                for pid in &offer.product_ids {
                    if let Some(prod) = catalog.products.iter().find(|p| p.id == *pid) {
                        if prod.type_id > 0 {
                            let total_qty = i32::try_from(
                                (prod.quantity as i64)
                                    .checked_mul(qty)
                                    .ok_or_else(|| rpc_err("InvalidQuantity", "Quantity overflow"))?,
                            )
                            .map_err(|_| rpc_err("InvalidQuantity", "Quantity overflow"))?;
                            snap.next_entity_id += 1;
                            delivered_item_id = snap.next_entity_id;
                            snap.items.insert(
                                delivered_item_id,
                                InventoryItemRecord {
                                    item_id: delivered_item_id,
                                    type_id: prod.type_id,
                                    owner_id: char_id,
                                    location_id: station_id,
                                    flag: InventoryFlag::Hangar,
                                    quantity: total_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        } else {
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                ch.clone_grade = 2;
                            }
                        }
                    }
                }
                Ok((rem_plex, delivered_item_id, tx_id))
            });
            match res {
                Ok((rem_plex, item_id, tx_id)) => {
                    let _ = ctx.notify_account_change(char_id, "plex", char_id, rem_plex as f64);
                    crate::services::quasar_payment_store::emit_balance_changed_notice(
                        &ctx.quasar_gateway.pending_notices,
                        user_id,
                        tx_id,
                        -total_plex,
                        rem_plex,
                        &format!("NES purchase: {}", offer.name),
                        ctx.now_unix(),
                    );
                    if offer.category_id == 1 {
                        crate::services::quasar_auth_user::activate_user_omega_license(
                            &ctx.db,
                            &ctx.quasar_gateway.pending_notices,
                            user_id,
                            30 * 86_400 * (qty as u64),
                            ctx.now_unix(),
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("success", PyRep::Bool(true)),
                        ("offerID", PyRep::Integer(offer_id)),
                        ("plexSpent", PyRep::Integer(total_plex)),
                        ("plexRemaining", PyRep::Integer(rem_plex)),
                        ("deliveredItemID", PyRep::Integer(item_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // --- Station Direct Trade (`trademgr`, `feat:wallet.trade-window`) ---
        ("trademgr", "InitiateTrade") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(rpc_err(
                    "NotDockedAtStation",
                    "Character must be docked to initiate a direct trade",
                )));
            };
            let Some(partner_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "InitiateTrade requires target charID")));
            };
            if partner_id == char_id {
                return Some(Err(rpc_err("InvalidArgument", "Cannot trade with yourself")));
            }
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let partner = snap
                    .characters
                    .get(&partner_id)
                    .ok_or_else(|| rpc_err("CharacterNotFound", "Trade partner not found"))?;
                if partner.station_id.or(partner.structure_id) != Some(station_id) {
                    return Err(rpc_err(
                        "PartnerNotInStation",
                        "Trade partner must be docked at the same station or structure",
                    ));
                }
                snap.next_entity_id += 1;
                let trade_id = snap.next_entity_id;
                let state = TradeSessionState {
                    trade_id,
                    station_id,
                    char_a: char_id,
                    char_b: partner_id,
                    items_a: Vec::new(),
                    items_b: Vec::new(),
                    money_a_cents: 0,
                    money_b_cents: 0,
                    accept_a: false,
                    accept_b: false,
                    completed: false,
                    cancelled: false,
                };
                save_json_note(
                    snap,
                    trade_note_id(trade_id),
                    -73,
                    trade_id,
                    format!("trade:{trade_id}"),
                    &state,
                    now,
                );
                Ok(trade_id)
            });
            match res {
                Ok(trade_id) => {
                    for party in [char_id, partner_id] {
                        ctx.notify(
                            NotificationTarget::Character(party),
                            "OnTradeInitiate",
                            "charid",
                            vec![
                                PyRep::Integer(char_id),
                                PyRep::Integer(partner_id),
                                PyRep::Integer(trade_id),
                            ],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("tradeContainerID", PyRep::Integer(trade_id)),
                        ("myID", PyRep::Integer(char_id)),
                        ("herID", PyRep::Integer(partner_id)),
                        ("stationID", PyRep::Integer(station_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("trademgr", "AddItem")
        | ("trademgr", "OfferMoney")
        | ("trademgr", "MakeOffer")
        | ("trademgr", "Abort")
        | ("trademgr", "List") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let trade_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let nid = if trade_id > 0 {
                    trade_note_id(trade_id)
                } else {
                    snap.owner_notes
                        .values()
                        .filter(|n| n.character_id == -73)
                        .filter_map(|n| serde_json::from_str::<TradeSessionState>(&n.note).ok())
                        .find(|t| !t.completed && !t.cancelled && (t.char_a == char_id || t.char_b == char_id))
                        .map(|t| trade_note_id(t.trade_id))
                        .ok_or_else(|| rpc_err("TradeNotFound", "No active trade session found"))?
                };
                let Some(note) = snap.owner_notes.get(&nid) else {
                    return Err(rpc_err("TradeNotFound", "Trade session not found"));
                };
                let mut tr: TradeSessionState = serde_json::from_str(&note.note)
                    .map_err(|_| rpc_err("TradeNotFound", "Corrupt trade session"))?;
                if tr.completed || tr.cancelled {
                    return Err(rpc_err("TradeClosed", "Trade session is already closed"));
                }
                let is_a = tr.char_a == char_id;
                if !is_a && tr.char_b != char_id {
                    return Err(rpc_err("AccessDenied", "Character is not a participant in this trade"));
                }

                match method {
                    "List" => {}
                    "Abort" => {
                        tr.cancelled = true;
                        save_json_note(snap, nid, -73, tr.trade_id, format!("trade:{}", tr.trade_id), &tr, now);
                    }
                    "AddItem" => {
                        let item_id = args
                            .get(1)
                            .or_else(|| args.first())
                            .and_then(|a| a.as_i64())
                            .unwrap_or(0);
                        let item = snap
                            .items
                            .get(&item_id)
                            .ok_or_else(|| rpc_err("ItemNotFound", "Item not found"))?;
                        let active_ship_id = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.active_ship_id);
                        let has_contents = snap
                            .items
                            .values()
                            .any(|child| child.item_id != item_id && child.location_id == item_id && child.quantity > 0);
                        if item.owner_id != char_id
                            || item.location_id != tr.station_id
                            || item.flag != InventoryFlag::Hangar
                            || Some(item_id) == active_ship_id
                            || snap.is_item_locked_in_contract_or_job(item_id)
                            || has_contents
                        {
                            return Err(rpc_err(
                                "InvalidTradeItem",
                                "Item must be an unlocked, empty item in station hangar (not active ship)",
                            ));
                        }
                        if snap.item_custom_names.get(&item_id).is_some_and(|s| s.starts_with("courier_cargo:")) {
                            return Err(rpc_err("InvalidTradeItem", "Courier mission items cannot be traded"));
                        }
                        let raw_qty = args.get(2).and_then(|a| a.as_i64()).unwrap_or(item.quantity as i64);
                        let Some(qty) = i32::try_from(raw_qty).ok().filter(|&q| q >= 1) else {
                            return Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: "Quantity must be a positive integer between 1 and i32::MAX".into(),
                            });
                        };
                        if item.singleton && qty != item.quantity {
                            return Err(BridgeRpcError {
                                code: "CannotSplitSingleton".into(),
                                message: "Assembled items cannot be split".into(),
                            });
                        }
                        let items_list = if is_a {
                            &mut tr.items_a
                        } else {
                            &mut tr.items_b
                        };
                        if let Some(existing_entry) = items_list.iter_mut().find(|(id, _)| *id == item_id) {
                            let Some(new_total) = existing_entry.1.checked_add(qty).filter(|&t| t <= item.quantity) else {
                                return Err(BridgeRpcError {
                                    code: "InsufficientQuantity".into(),
                                    message: "Offered quantity exceeds available item stack".into(),
                                });
                            };
                            existing_entry.1 = new_total;
                        } else {
                            if qty > item.quantity {
                                return Err(BridgeRpcError {
                                    code: "InsufficientQuantity".into(),
                                    message: "Offered quantity exceeds available item stack".into(),
                                });
                            }
                            items_list.push((item_id, qty));
                        }
                        tr.accept_a = false;
                        tr.accept_b = false;
                        save_json_note(snap, nid, -73, tr.trade_id, format!("trade:{}", tr.trade_id), &tr, now);
                    }
                    "OfferMoney" => {
                        let isk = args
                            .get(1)
                            .or_else(|| args.first())
                            .and_then(|a| a.as_f64())
                            .unwrap_or(0.0);
                        if !isk.is_finite() || isk < 0.0 {
                            return Err(rpc_err("InvalidArgument", "Offered ISK must be non-negative"));
                        }
                        let cents = IskCents::from_f64_isk(isk);
                        let bal = snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0));
                        if bal.0 < cents.0 {
                            return Err(rpc_err("NotEnoughMoney", "Insufficient ISK for trade offer"));
                        }
                        if is_a {
                            tr.money_a_cents = cents.0;
                        } else {
                            tr.money_b_cents = cents.0;
                        }
                        tr.accept_a = false;
                        tr.accept_b = false;
                        save_json_note(snap, nid, -73, tr.trade_id, format!("trade:{}", tr.trade_id), &tr, now);
                    }
                    "MakeOffer" => {
                        let accept = args
                            .get(1)
                            .or_else(|| args.first())
                            .and_then(|a| a.as_bool())
                            .unwrap_or(true);
                        if is_a {
                            tr.accept_a = accept;
                        } else {
                            tr.accept_b = accept;
                        }
                        if tr.accept_a && tr.accept_b {
                            // Verify both wallets and items atomically
                            let bal_a = snap.characters.get(&tr.char_a).map(|c| c.balance.0).unwrap_or(0);
                            let bal_b = snap.characters.get(&tr.char_b).map(|c| c.balance.0).unwrap_or(0);
                            if bal_a < tr.money_a_cents || bal_b < tr.money_b_cents {
                                return Err(rpc_err("NotEnoughMoney", "Participant lacks ISK to complete trade"));
                            }
                            let active_ship_a = snap.characters.get(&tr.char_a).map(|c| c.active_ship_id);
                            let active_ship_b = snap.characters.get(&tr.char_b).map(|c| c.active_ship_id);
                            for &(iid, q) in &tr.items_a {
                                let Some(it) = snap.items.get(&iid) else {
                                    return Err(rpc_err("ItemNotFound", "Offered item missing"));
                                };
                                let has_contents = snap
                                    .items
                                    .values()
                                    .any(|child| child.item_id != iid && child.location_id == iid && child.quantity > 0);
                                if it.owner_id != tr.char_a
                                    || it.location_id != tr.station_id
                                    || it.flag != InventoryFlag::Hangar
                                    || it.quantity < q
                                    || Some(iid) == active_ship_a
                                    || snap.is_item_locked_in_contract_or_job(iid)
                                    || has_contents
                                {
                                    return Err(rpc_err("InvalidTradeItem", "Offered item no longer available"));
                                }
                                if it.singleton && q != it.quantity {
                                    return Err(rpc_err("CannotSplitSingleton", "Assembled items cannot be split"));
                                }
                            }
                            for &(iid, q) in &tr.items_b {
                                let Some(it) = snap.items.get(&iid) else {
                                    return Err(rpc_err("ItemNotFound", "Offered item missing"));
                                };
                                let has_contents = snap
                                    .items
                                    .values()
                                    .any(|child| child.item_id != iid && child.location_id == iid && child.quantity > 0);
                                if it.owner_id != tr.char_b
                                    || it.location_id != tr.station_id
                                    || it.flag != InventoryFlag::Hangar
                                    || it.quantity < q
                                    || Some(iid) == active_ship_b
                                    || snap.is_item_locked_in_contract_or_job(iid)
                                    || has_contents
                                {
                                    return Err(rpc_err("InvalidTradeItem", "Offered item no longer available"));
                                }
                                if it.singleton && q != it.quantity {
                                    return Err(rpc_err("CannotSplitSingleton", "Assembled items cannot be split"));
                                }
                            }
                            // Transfer items A -> B and B -> A
                            for &(iid, q) in &tr.items_a {
                                let new_item = if let Some(it) = snap.items.get_mut(&iid) {
                                    if it.owner_id != tr.char_a || it.location_id != tr.station_id {
                                        return Err(rpc_err("InvalidTradeItem", "Offered item no longer available"));
                                    }
                                    if it.singleton && q != it.quantity {
                                        return Err(rpc_err("CannotSplitSingleton", "Assembled items cannot be split"));
                                    }
                                    if q < it.quantity {
                                        it.quantity = it.quantity.checked_sub(q).ok_or_else(|| {
                                            rpc_err("InvalidTradeItem", "Underflow calculating remaining quantity")
                                        })?;
                                        it.version += 1;
                                        let mut new_it = it.clone();
                                        new_it.owner_id = tr.char_b;
                                        new_it.location_id = tr.station_id;
                                        new_it.flag = InventoryFlag::Hangar;
                                        new_it.quantity = q;
                                        new_it.version = 1;
                                        Some(new_it)
                                    } else if q == it.quantity {
                                        it.owner_id = tr.char_b;
                                        it.flag = InventoryFlag::Hangar;
                                        it.version += 1;
                                        None
                                    } else {
                                        return Err(rpc_err("InvalidTradeItem", "Offered item quantity exceeded"));
                                    }
                                } else {
                                    return Err(rpc_err("ItemNotFound", "Offered item missing"));
                                };
                                if let Some(mut new_it) = new_item {
                                    snap.next_entity_id += 1;
                                    let new_iid = snap.next_entity_id.max(snap.items.keys().copied().max().unwrap_or(1_000_000) + 1);
                                    snap.next_entity_id = new_iid;
                                    new_it.item_id = new_iid;
                                    snap.items.insert(new_iid, new_it);
                                }
                            }
                            for &(iid, q) in &tr.items_b {
                                let new_item = if let Some(it) = snap.items.get_mut(&iid) {
                                    if it.owner_id != tr.char_b || it.location_id != tr.station_id {
                                        return Err(rpc_err("InvalidTradeItem", "Offered item no longer available"));
                                    }
                                    if it.singleton && q != it.quantity {
                                        return Err(rpc_err("CannotSplitSingleton", "Assembled items cannot be split"));
                                    }
                                    if q < it.quantity {
                                        it.quantity = it.quantity.checked_sub(q).ok_or_else(|| {
                                            rpc_err("InvalidTradeItem", "Underflow calculating remaining quantity")
                                        })?;
                                        it.version += 1;
                                        let mut new_it = it.clone();
                                        new_it.owner_id = tr.char_a;
                                        new_it.location_id = tr.station_id;
                                        new_it.flag = InventoryFlag::Hangar;
                                        new_it.quantity = q;
                                        new_it.version = 1;
                                        Some(new_it)
                                    } else if q == it.quantity {
                                        it.owner_id = tr.char_a;
                                        it.flag = InventoryFlag::Hangar;
                                        it.version += 1;
                                        None
                                    } else {
                                        return Err(rpc_err("InvalidTradeItem", "Offered item quantity exceeded"));
                                    }
                                } else {
                                    return Err(rpc_err("ItemNotFound", "Offered item missing"));
                                };
                                if let Some(mut new_it) = new_item {
                                    snap.next_entity_id += 1;
                                    let new_iid = snap.next_entity_id.max(snap.items.keys().copied().max().unwrap_or(1_000_000) + 1);
                                    snap.next_entity_id = new_iid;
                                    new_it.item_id = new_iid;
                                    snap.items.insert(new_iid, new_it);
                                }
                            }
                            // Transfer ISK
                            let net_to_b = tr.money_a_cents - tr.money_b_cents;
                            if net_to_b != 0 {
                                if let Some(ca) = snap.characters.get_mut(&tr.char_a) {
                                    ca.balance = IskCents(ca.balance.0 - net_to_b);
                                    let ba = ca.balance;
                                    snap.append_journal(
                                        tr.char_a,
                                        "PlayerTrading",
                                        IskCents(net_to_b.abs()),
                                        ba,
                                        format!("Direct station trade {} with {}", tr.trade_id, tr.char_b),
                                    );
                                }
                                if let Some(cb) = snap.characters.get_mut(&tr.char_b) {
                                    cb.balance = IskCents(cb.balance.0 + net_to_b);
                                    let bb = cb.balance;
                                    snap.append_journal(
                                        tr.char_b,
                                        "PlayerTrading",
                                        IskCents(net_to_b.abs()),
                                        bb,
                                        format!("Direct station trade {} with {}", tr.trade_id, tr.char_a),
                                    );
                                }
                            }
                            tr.completed = true;
                        }
                        save_json_note(snap, nid, -73, tr.trade_id, format!("trade:{}", tr.trade_id), &tr, now);
                    }
                    _ => {}
                }
                Ok(tr)
            });
            match res {
                Ok(tr) => {
                    for party in [tr.char_a, tr.char_b] {
                        match method {
                            "Abort" => {
                                ctx.notify(
                                    NotificationTarget::Character(party),
                                    "OnTradeCancel",
                                    "charid",
                                    vec![PyRep::Integer(tr.trade_id)],
                                );
                            }
                            "OfferMoney" => {
                                ctx.notify(
                                    NotificationTarget::Character(party),
                                    "OnTradeOfferReset",
                                    "charid",
                                    vec![PyRep::Integer(tr.trade_id)],
                                );
                                ctx.notify(
                                    NotificationTarget::Character(party),
                                    "OnTradeMoneyOffer",
                                    "charid",
                                    vec![
                                        PyRep::Integer(tr.trade_id),
                                        PyRep::Real(IskCents(tr.money_a_cents).to_wire_f64()),
                                        PyRep::Real(IskCents(tr.money_b_cents).to_wire_f64()),
                                    ],
                                );
                            }
                            "AddItem" => {
                                ctx.notify(
                                    NotificationTarget::Character(party),
                                    "OnTradeOfferReset",
                                    "charid",
                                    vec![PyRep::Integer(tr.trade_id)],
                                );
                                ctx.notify(
                                    NotificationTarget::Character(party),
                                    "OnTradeOffer",
                                    "charid",
                                    vec![
                                        PyRep::Integer(tr.trade_id),
                                        PyRep::Bool(tr.accept_a),
                                        PyRep::Bool(tr.accept_b),
                                    ],
                                );
                            }
                            "MakeOffer" => {
                                ctx.notify(
                                    NotificationTarget::Character(party),
                                    "OnTradeOffer",
                                    "charid",
                                    vec![
                                        PyRep::Integer(tr.trade_id),
                                        PyRep::Bool(tr.accept_a),
                                        PyRep::Bool(tr.accept_b),
                                    ],
                                );
                                if tr.completed {
                                    ctx.notify(
                                        NotificationTarget::Character(party),
                                        "OnTradeComplete",
                                        "charid",
                                        vec![PyRep::Integer(tr.trade_id)],
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("tradeContainerID", PyRep::Integer(tr.trade_id)),
                        ("myAccept", PyRep::Bool(tr.accept_a)),
                        ("herAccept", PyRep::Bool(tr.accept_b)),
                        ("myMoney", PyRep::Real(IskCents(tr.money_a_cents).to_wire_f64())),
                        ("herMoney", PyRep::Real(IskCents(tr.money_b_cents).to_wire_f64())),
                        ("completed", PyRep::Bool(tr.completed)),
                        ("cancelled", PyRep::Bool(tr.cancelled)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // --- HyperNet Relay (`raffleProxy`, `feat:wallet.hypernet-relay` & `feat:wallet.hypernet-hypercore-sizing-expiry`) ---
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
            let tid = args.first().and_then(|a| a.as_i64()).unwrap_or(34) as TypeId;
            let base = ctx
                .sde
                .types
                .get(&tid)
                .map(|t| t.base_price.max(100.0))
                .unwrap_or(1_000.0);
            Some(Ok(PyRep::Tuple(vec![PyRep::Real(base), PyRep::Real(base)])))
        }
        ("raffleProxy", "CreateRaffle") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let station_id = session.station_id.or(session.structure_id).unwrap_or(60_003_760);
            let (item_id, ticket_count, ticket_price_isk) = match args.first() {
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
                ),
                Some(PyRep::Dict(kv)) => {
                    let fk = |k: &str| kv.iter().find(|(ek, _)| ek.as_str() == Some(k)).map(|(_, ev)| ev);
                    (
                        fk("item_id").or_else(|| fk("itemID")).and_then(|v| v.as_i64()).unwrap_or(0),
                        fk("ticket_count").or_else(|| fk("ticketCount")).and_then(|v| v.as_i64()).unwrap_or(8) as u32,
                        fk("ticket_price").or_else(|| fk("ticketPrice")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                    )
                }
                _ => (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(8) as u32,
                    args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0),
                ),
            };
            let est_value_isk = ticket_price_isk * (ticket_count as f64);
            let required_cores = match compute_required_hypercores(est_value_isk, ticket_count) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let ticket_price_cents = IskCents::from_f64_isk(ticket_price_isk).0;
            let now = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
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
                if item.owner_id != char_id
                    || item.location_id != station_id
                    || item.flag != InventoryFlag::Hangar
                    || item.quantity < 1
                    || Some(item_id) == active_ship_id
                    || snap.is_item_locked_in_contract_or_job(item_id)
                    || has_contents
                {
                    return Err(rpc_err(
                        "InvalidRaffleItem",
                        "Raffle item must be an unlocked, empty item in station hangar (not active ship)",
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
                    expires_unix: now.saturating_add(RAFFLE_DURATION_SECS),
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
        ("raffleProxy", "BuyTicket") | ("raffleProxy", "BuyRandomTickets") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = sweep_expired_raffles(ctx);
            let Some(raffle_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(rpc_err("InvalidArgument", "Raffle ticket purchase requires raffle_id")));
            };
            let second_arg = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let now = ctx.now_unix();
            let rng_entropy_draw = ctx.mechanic_rng.try_next_u64().unwrap_or(now as u64);
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
                    let t_idx = second_arg as u32;
                    if t_idx >= r.ticket_count || r.sold_tickets.contains_key(&t_idx) {
                        return Err(rpc_err("TicketUnavailable", "Requested ticket is unavailable"));
                    }
                    tickets_to_buy.push(t_idx);
                } else {
                    let count = second_arg.max(1) as usize;
                    for idx in 0..r.ticket_count {
                        if !r.sold_tickets.contains_key(&idx) {
                            tickets_to_buy.push(idx);
                            if tickets_to_buy.len() == count {
                                break;
                            }
                        }
                    }
                    if tickets_to_buy.len() < count {
                        return Err(rpc_err("NotEnoughTickets", "Not enough remaining tickets in raffle"));
                    }
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
                        .wrapping_add(r.created_unix as u64)
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
                    let fee_cents = (total_pot.0 / 100) * 5;
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
            "Grab" | "FilteredGrab" | "GetCreatedParticipated" | "GetMyActiveTickets" | "GetMyRaffleHistory",
        ) => {
            let char_id = session.character_id.unwrap_or(0);
            let _ = sweep_expired_raffles(ctx);
            let get_filter = |k1: &str, k2: &str| -> Option<PyRep> {
                if let Some(v) = _kwargs.get(k1).or_else(|| _kwargs.get(k2)) {
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
