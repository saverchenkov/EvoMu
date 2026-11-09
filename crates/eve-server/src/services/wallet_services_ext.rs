//! `wallet_services_ext` — Corporation Bills (`billMgr`), New Eden Store (`storeManager`),
//! Station Direct Trade (`trademgr`), HyperNet Relay (`raffleProxy`), Legacy Starbase (`posMgr`)
//! stubs, and Cluster Operations (`TiDi`, Node Balancing, Daily Downtime Jobs).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};

pub use crate::services::hypernet_raffle_ops::{
    ALLOWED_RAFFLE_SIZES, HYPERCORE_TYPE_ID, RAFFLE_DURATION_SECS, RaffleStateRecord,
    compute_required_hypercores, sweep_expired_raffles,
};

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

fn trade_note_id(trade_id: i64) -> i64 {
    -(7_300_000_000_000_i64.saturating_add(trade_id.abs()))
}

pub(super) fn bill_note_id(bill_id: i64) -> i64 {
    -(7_100_000_000_000_i64.saturating_add(bill_id.abs()))
}

pub(super) fn autopay_note_id(corp_id: i64) -> i64 {
    -(7_200_000_000_000_i64.saturating_add(corp_id.abs()))
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

fn read_or_seed_store_catalog(
    ctx: &ServerContext,
    session: &SessionState,
) -> crate::services::quasar_payment_store::StoreCatalog {
    let cat = ctx
        .db
        .with_snapshot(crate::services::quasar_payment_store::read_store_catalog);
    if !cat.categories.is_empty() || session.address.is_empty() {
        return cat;
    }
    ctx.db.with_mut_snapshot(|snap| {
        let existing = crate::services::quasar_payment_store::read_store_catalog(snap);
        if !existing.categories.is_empty() {
            return existing;
        }
        let type_name = |tid: u32| -> String {
            ctx.sde
                .types
                .get(&tid)
                .map(|t| t.name.clone())
                .unwrap_or_default()
        };
        let cat_name = |cid: u32| -> String {
            ctx.sde
                .categories
                .get(&cid)
                .map(|c| c.name.clone())
                .unwrap_or_default()
        };
        let seeded = crate::services::quasar_payment_store::StoreCatalog {
            categories: vec![
                crate::services::quasar_payment_store::StoreCategory {
                    id: 1,
                    name: cat_name(91),
                    description: cat_name(91),
                },
                crate::services::quasar_payment_store::StoreCategory {
                    id: 2,
                    name: cat_name(5),
                    description: cat_name(5),
                },
                crate::services::quasar_payment_store::StoreCategory {
                    id: 3,
                    name: cat_name(30),
                    description: cat_name(30),
                },
            ],
            products: vec![
                crate::services::quasar_payment_store::StoreProduct {
                    id: 102,
                    name: type_name(34599),
                    description: type_name(34599),
                    type_id: 34599,
                    quantity: 1,
                },
                crate::services::quasar_payment_store::StoreProduct {
                    id: 103,
                    name: type_name(40519),
                    description: type_name(40519),
                    type_id: 40519,
                    quantity: 1,
                },
                crate::services::quasar_payment_store::StoreProduct {
                    id: 104,
                    name: type_name(3966),
                    description: type_name(3966),
                    type_id: 3966,
                    quantity: 1,
                },
            ],
            offers: vec![
                crate::services::quasar_payment_store::StoreOffer {
                    id: 1002,
                    name: type_name(34599),
                    description: type_name(34599),
                    category_id: 1,
                    price_plex: 110,
                    product_ids: vec![102],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                crate::services::quasar_payment_store::StoreOffer {
                    id: 1003,
                    name: type_name(40519),
                    description: type_name(40519),
                    category_id: 2,
                    price_plex: 140,
                    product_ids: vec![103],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                crate::services::quasar_payment_store::StoreOffer {
                    id: 1004,
                    name: type_name(3966),
                    description: type_name(3966),
                    category_id: 3,
                    price_plex: 75,
                    product_ids: vec![104],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
            ],
        };
        crate::services::quasar_payment_store::save_store_catalog(snap, &seeded);
        seeded
    })
}

fn handle_store_get_categories(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let catalog = read_or_seed_store_catalog(ctx, session);
    let mut items: Vec<PyRep> = Vec::new();
    for c in catalog.categories {
        let root_id = c.id as i64;
        let sub_id = root_id * 100 + 1;
        items.push(PyRep::keyval([
            ("id", PyRep::Integer(root_id)),
            ("name", PyRep::String(c.name.clone())),
            ("description", PyRep::String(c.description.clone())),
            ("href", PyRep::String(String::new())),
            ("parent", PyRep::None),
            ("tags", PyRep::List(Vec::new())),
        ]));
        items.push(PyRep::keyval([
            ("id", PyRep::Integer(sub_id)),
            ("name", PyRep::String(c.name)),
            ("description", PyRep::String(c.description)),
            ("href", PyRep::String(String::new())),
            ("parent", PyRep::keyval([("id", PyRep::Integer(root_id))])),
            ("tags", PyRep::List(Vec::new())),
        ]));
    }
    Some(Ok(PyRep::List(items)))
}

fn handle_store_get_products(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let catalog = read_or_seed_store_catalog(ctx, session);
    let prods: Vec<PyRep> = catalog
        .products
        .into_iter()
        .map(|p| {
            PyRep::keyval([
                ("id", PyRep::Integer(p.id as i64)),
                ("productID", PyRep::Integer(p.id as i64)),
                ("name", PyRep::String(p.name.clone())),
                ("productName", PyRep::String(p.name)),
                ("description", PyRep::String(p.description)),
                ("typeID", PyRep::Integer(p.type_id as i64)),
                ("typeId", PyRep::Integer(p.type_id as i64)),
                ("quantity", PyRep::Integer(p.quantity as i64)),
                ("href", PyRep::String(String::new())),
                ("imageUrl", PyRep::String(String::new())),
            ])
        })
        .collect();
    Some(Ok(PyRep::List(prods)))
}

fn handle_store_get_offers(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let catalog = read_or_seed_store_catalog(ctx, session);
    let products_by_id: HashMap<_, _> = catalog
        .products
        .iter()
        .cloned()
        .map(|p| (p.id, p))
        .collect();
    let offers: Vec<PyRep> = catalog
        .offers
        .into_iter()
        .filter(|o| !o.targeted)
        .map(|o| {
            let prod_reps: Vec<PyRep> = o
                .product_ids
                .iter()
                .map(|pid| {
                    if let Some(p) = products_by_id.get(pid) {
                        PyRep::keyval([
                            ("id", PyRep::Integer(p.id as i64)),
                            ("productID", PyRep::Integer(p.id as i64)),
                            ("typeID", PyRep::Integer(p.type_id as i64)),
                            ("typeId", PyRep::Integer(p.type_id as i64)),
                            ("quantity", PyRep::Integer(p.quantity as i64)),
                            ("name", PyRep::String(p.name.clone())),
                            ("productName", PyRep::String(p.name.clone())),
                            ("imageUrl", PyRep::String(String::new())),
                        ])
                    } else {
                        PyRep::keyval([
                            ("id", PyRep::Integer(*pid as i64)),
                            ("productID", PyRep::Integer(*pid as i64)),
                            ("typeID", PyRep::Integer(*pid as i64)),
                            ("typeId", PyRep::Integer(*pid as i64)),
                            ("quantity", PyRep::Integer(1)),
                            ("name", PyRep::String(format!("Product {pid}"))),
                            ("productName", PyRep::String(format!("Product {pid}"))),
                            ("imageUrl", PyRep::String(String::new())),
                        ])
                    }
                })
                .collect();
            let root_cat_id = o.category_id as i64;
            let sub_cat_id = root_cat_id * 100 + 1;
            PyRep::keyval([
                ("id", PyRep::Integer(o.id as i64)),
                ("offerID", PyRep::Integer(o.id as i64)),
                ("name", PyRep::String(o.name)),
                ("description", PyRep::String(o.description)),
                ("href", PyRep::String(String::new())),
                ("imageUrl", PyRep::String(String::new())),
                ("label", PyRep::None),
                ("canPurchase", PyRep::Bool(true)),
                ("singlePurchase", PyRep::Bool(false)),
                ("categoryID", PyRep::Integer(root_cat_id)),
                (
                    "categories",
                    PyRep::List(vec![
                        PyRep::keyval([("id", PyRep::Integer(root_cat_id))]),
                        PyRep::keyval([("id", PyRep::Integer(sub_cat_id))]),
                    ]),
                ),
                ("currency", PyRep::String("PLX".into())),
                ("price", PyRep::Integer(o.price_plex)),
                ("plexPrice", PyRep::Integer(o.price_plex)),
                (
                    "offerPricings",
                    PyRep::List(vec![PyRep::keyval([
                        ("currency", PyRep::String("PLX".into())),
                        ("price", PyRep::Integer(o.price_plex)),
                        ("basePrice", PyRep::Integer(o.price_plex)),
                    ])]),
                ),
                ("products", PyRep::List(prod_reps)),
                ("quantity", PyRep::Integer(1)),
            ])
        })
        .collect();
    Some(Ok(PyRep::List(offers)))
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
        // parity: rpc:storeManager.get_categories
        ("storeManager", "get_categories") | ("storeManager", "GetCategories") => {
            handle_store_get_categories(ctx, session)
        }
        // parity: rpc:storeManager.get_products
        ("storeManager", "get_products") | ("storeManager", "GetProducts") => {
            handle_store_get_products(ctx, session)
        }
        // parity: rpc:storeManager.get_offers
        ("storeManager", "get_offers") => handle_store_get_offers(ctx, session),
        // parity: rpc:storeManager.can_purchase_offer, rpc:storeManager.buy_offer
        ("storeManager", "can_purchase_offer") | ("storeManager", "buy_offer") => {
            let (user_id, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let offer_id = match args
                .first()
                .or_else(|| _kwargs.get("offer_id").or_else(|| _kwargs.get("offerID")))
                .and_then(|a| match a {
                    PyRep::Integer(i) => Some(*i),
                    PyRep::String(s) => s.trim_start_matches("VEVC-").parse::<i64>().ok(),
                    _ => None,
                }) {
                Some(id) if id > 0 => id,
                _ => {
                    return Some(Err(rpc_err(
                        "InvalidArguments",
                        "storeManager offer_id must be a positive integer",
                    )))
                }
            };
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
            if !currency.eq_ignore_ascii_case("PLX") && !currency.eq_ignore_ascii_case("PLEX") {
                return Some(Err(rpc_err(
                    "InvalidCurrency",
                    format!("Unsupported store currency: {currency}"),
                )));
            }
            let qty = match args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("quantity").and_then(|v| v.as_i64()))
            {
                Some(q) if q <= 0 => {
                    return Some(Err(rpc_err(
                        "InvalidQuantity",
                        "Purchase quantity must be a positive integer",
                    )))
                }
                Some(q) => q,
                None => 1,
            };

            let catalog = read_or_seed_store_catalog(ctx, session);
            let offer_tags = ctx
                .db
                .with_snapshot(crate::services::quasar_payment_store::read_store_offer_tags);
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
            let has_omega_weekly_tag = offer_tags.get(&offer.id).is_some_and(|tags| {
                tags.iter().any(|t| {
                    let lc = t.to_ascii_lowercase();
                    lc == "omega-weekly-free-sp" || lc == "omega-weekly"
                })
            });
            let free_sp_from_products: i64 = catalog
                .products
                .iter()
                .filter(|p| offer.product_ids.contains(&p.id) && p.type_id == 0 && p.quantity > 1)
                .map(|p| i64::from(p.quantity))
                .sum();
            if offer.price_plex == 0 && (has_omega_weekly_tag || free_sp_from_products > 0) {
                let sp_grant = if free_sp_from_products > 0 { free_sp_from_products } else { 15_000 };
                return Some(super::character_skill_ops::handle_omega_weekly_free_sp_offer(
                    ctx,
                    session,
                    offer_id,
                    sp_grant,
                    method == "buy_offer",
                ));
            }
            if offer.price_plex <= 0 && !ctx.feature_flags.free_fast_checkout_plex {
                return Some(Err(rpc_err(
                    "InvalidOffer",
                    format!("Offer {offer_id} has invalid non-positive PLEX price"),
                )));
            }
            let total_plex = match offer.price_plex.checked_mul(qty) {
                Some(v) if v >= 0 => v,
                _ => {
                    return Some(Err(rpc_err(
                        "InvalidQuantity",
                        "PLEX price overflows",
                    )));
                }
            };
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
                let mut delivered_items: Vec<InventoryItemRecord> = Vec::new();
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
                            let rec = InventoryItemRecord {
                                item_id: delivered_item_id,
                                type_id: prod.type_id,
                                owner_id: char_id,
                                location_id: station_id,
                                flag: InventoryFlag::Hangar,
                                quantity: total_qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            };
                            snap.items.insert(delivered_item_id, rec.clone());
                            delivered_items.push(rec);
                        } else if offer.category_id == 1 {
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                ch.clone_grade = 2;
                            }
                        }
                    }
                }
                Ok((rem_plex, delivered_item_id, delivered_items, tx_id))
            });
            match res {
                Ok((rem_plex, item_id, delivered_items, tx_id)) => {
                    let _ = ctx.notify_account_change(char_id, "plex", char_id, rem_plex as f64);
                    for rec in &delivered_items {
                        ctx.notify_item_mutation(char_id, None, Some(rec));
                    }
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
        ("trademgr", "AddItem") => handle_trade_add_item(ctx, session, args),
        ("trademgr", "OfferMoney") => handle_trade_offer_money(ctx, session, args),
        ("trademgr", "MakeOffer") => handle_trade_make_offer(ctx, session, args),
        ("trademgr", "Abort") => handle_trade_abort(ctx, session, args),
        ("trademgr", "List") => handle_trade_list(ctx, session, args),

        // --- HyperNet Relay (`raffleProxy`, `feat:wallet.hypernet-relay` & `feat:wallet.hypernet-hypercore-sizing-expiry`) ---
        ("raffleProxy", _) => {
            crate::services::hypernet_raffle_ops::handle(ctx, session, service, method, args, _kwargs)
        }

        _ => None,
    }
}

#[inline]
fn handle_trade_add_item(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_trade_action(ctx, session, "AddItem", args)
}

#[inline]
fn handle_trade_offer_money(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_trade_action(ctx, session, "OfferMoney", args)
}

#[inline]
fn handle_trade_make_offer(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_trade_action(ctx, session, "MakeOffer", args)
}

#[inline]
fn handle_trade_abort(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_trade_action(ctx, session, "Abort", args)
}

#[inline]
fn handle_trade_list(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_trade_action(ctx, session, "List", args)
}

fn handle_trade_action(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let trade_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let now = ctx.now_unix();
    let mut added_item_id: Option<i64> = None;
    let mut item_diffs: Vec<(i64, Option<eve_db::InventoryItemRecord>, Option<eve_db::InventoryItemRecord>)> = Vec::new();
    let mut bal_updates: Vec<(i64, IskCents)> = Vec::new();
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
        if matches!(method, "AddItem" | "OfferMoney" | "MakeOffer") {
            let docked_a = snap.characters.get(&tr.char_a).and_then(|c| c.station_id.or(c.structure_id));
            let docked_b = snap.characters.get(&tr.char_b).and_then(|c| c.station_id.or(c.structure_id));
            if docked_a != Some(tr.station_id) || docked_b != Some(tr.station_id) {
                return Err(rpc_err(
                    "NotDockedAtStation",
                    "Both trade participants must remain docked at the trade station",
                ));
            }
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
                    || snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, item_id)
                    || snap.is_item_locked_excluding_trade(item_id, Some(tr.trade_id))
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
                added_item_id = Some(item_id);
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
                            || snap.is_character_boarded_or_saved_ship(tr.char_a, None, iid)
                            || snap.is_item_locked_excluding_trade(iid, Some(tr.trade_id))
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
                            || snap.is_character_boarded_or_saved_ship(tr.char_b, None, iid)
                            || snap.is_item_locked_excluding_trade(iid, Some(tr.trade_id))
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
                        let (pre_item, new_item) = if let Some(it) = snap.items.get_mut(&iid) {
                            if it.owner_id != tr.char_a || it.location_id != tr.station_id {
                                return Err(rpc_err("InvalidTradeItem", "Offered item no longer available"));
                            }
                            if it.singleton && q != it.quantity {
                                return Err(rpc_err("CannotSplitSingleton", "Assembled items cannot be split"));
                            }
                            let pre = it.clone();
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
                                (pre, Some(new_it))
                            } else if q == it.quantity {
                                it.owner_id = tr.char_b;
                                it.flag = InventoryFlag::Hangar;
                                it.version += 1;
                                (pre, None)
                            } else {
                                return Err(rpc_err("InvalidTradeItem", "Offered item quantity exceeded"));
                            }
                        } else {
                            return Err(rpc_err("ItemNotFound", "Offered item missing"));
                        };
                        snap.sp6_lifecycle.self_destructs.remove(&iid);
                        let post_src = snap.items.get(&iid).cloned();
                        item_diffs.push((tr.char_a, Some(pre_item), post_src));
                        if let Some(mut new_it) = new_item {
                            snap.next_entity_id += 1;
                            let new_iid = snap.next_entity_id.max(snap.items.keys().copied().max().unwrap_or(1_000_000) + 1);
                            snap.next_entity_id = new_iid;
                            new_it.item_id = new_iid;
                            snap.items.insert(new_iid, new_it.clone());
                            item_diffs.push((tr.char_b, None, Some(new_it)));
                        }
                    }
                    for &(iid, q) in &tr.items_b {
                        let (pre_item, new_item) = if let Some(it) = snap.items.get_mut(&iid) {
                            if it.owner_id != tr.char_b || it.location_id != tr.station_id {
                                return Err(rpc_err("InvalidTradeItem", "Offered item no longer available"));
                            }
                            if it.singleton && q != it.quantity {
                                return Err(rpc_err("CannotSplitSingleton", "Assembled items cannot be split"));
                            }
                            let pre = it.clone();
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
                                (pre, Some(new_it))
                            } else if q == it.quantity {
                                it.owner_id = tr.char_a;
                                it.flag = InventoryFlag::Hangar;
                                it.version += 1;
                                (pre, None)
                            } else {
                                return Err(rpc_err("InvalidTradeItem", "Offered item quantity exceeded"));
                            }
                        } else {
                            return Err(rpc_err("ItemNotFound", "Offered item missing"));
                        };
                        snap.sp6_lifecycle.self_destructs.remove(&iid);
                        let post_src = snap.items.get(&iid).cloned();
                        item_diffs.push((tr.char_b, Some(pre_item), post_src));
                        if let Some(mut new_it) = new_item {
                            snap.next_entity_id += 1;
                            let new_iid = snap.next_entity_id.max(snap.items.keys().copied().max().unwrap_or(1_000_000) + 1);
                            snap.next_entity_id = new_iid;
                            new_it.item_id = new_iid;
                            snap.items.insert(new_iid, new_it.clone());
                            item_diffs.push((tr.char_a, None, Some(new_it)));
                        }
                    }
                    // Transfer ISK
                    let net_to_b = tr.money_a_cents - tr.money_b_cents;
                    if net_to_b != 0 {
                        if let Some(ca) = snap.characters.get_mut(&tr.char_a) {
                            ca.balance = IskCents(ca.balance.0 - net_to_b);
                            let ba = ca.balance;
                            bal_updates.push((tr.char_a, ba));
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
                            bal_updates.push((tr.char_b, bb));
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
            if tr.completed {
                for (fallback_char, pre, post) in item_diffs {
                    ctx.notify_item_mutation(fallback_char, pre.as_ref(), post.as_ref());
                }
                for (party_id, bal) in bal_updates {
                    ctx.notify_wallet_balance(false, party_id, 1, bal);
                }
                let mut traded_ids = Vec::new();
                for &(iid, _) in tr.items_a.iter().chain(tr.items_b.iter()) {
                    let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, iid);
                    traded_ids.push(iid);
                }
                if !traded_ids.is_empty() {
                    crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &traded_ids);
                }
            }
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
                        if party != char_id {
                            if let Some(iid) = added_item_id {
                                crate::services::notify_emitters_skills_trade_godma::emit_priming_needed_for_trade_items(
                                    ctx,
                                    party,
                                    &[iid],
                                );
                            }
                        }
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

#[cfg(test)]
#[path = "wallet_services_ext_tests.rs"]
mod wallet_services_ext_tests;

