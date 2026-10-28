//! Shared types and default data for Quasar Payment, PLEX Vault, and NES Store.

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

pub fn emit_balance_changed_notice(
    router_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    user_id: u64,
    tx_id: u64,
    amount: i64,
    balance_after: i64,
    description: &str,
    now_unix: u64,
) {
    let mut notice_bytes = Vec::new();
    let mut ident = Vec::new();
    encode_varint_field(1, tx_id, &mut ident);
    encode_bytes_field(1, &ident, &mut notice_bytes);

    let mut attr = Vec::new();
    let mut ts_bytes = Vec::new();
    encode_varint_field(1, now_unix, &mut ts_bytes);
    encode_bytes_field(1, &ts_bytes, &mut attr);

    let mut user_ident = Vec::new();
    encode_varint_field(1, user_id, &mut user_ident);
    encode_bytes_field(2, &user_ident, &mut attr);

    let mut amt_curr = Vec::new();
    encode_varint_field(1, (amount.abs() * 100) as u64, &mut amt_curr);
    encode_bytes_field(4, &amt_curr, &mut attr);

    let mut bal_curr = Vec::new();
    encode_varint_field(1, (balance_after * 100) as u64, &mut bal_curr);
    encode_bytes_field(5, &bal_curr, &mut attr);

    encode_string_field(6, description, &mut attr);
    encode_bytes_field(2, &attr, &mut notice_bytes);

    router_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            NoticeScope::User(user_id as i32),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"),
                value: notice_bytes,
            },
        ));
}

pub fn cancel_user_payment_schedule(
    db: &WorldRepository,
    router_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    uid: u64,
    sched_id: u64,
) -> bool {
    let removed = db.with_mut_snapshot(|snap| {
        let mut scheds: Vec<PaymentScheduleRecord> = snap
            .service_state_blobs
            .get("payment_schedules")
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        let initial_len = scheds.len();
        scheds.retain(|s| !(s.id == sched_id && s.user_id == uid));
        if scheds.len() < initial_len {
            if let Ok(serialized) = serde_json::to_string(&scheds) {
                snap.service_state_blobs.insert("payment_schedules".to_string(), serialized);
            }
            true
        } else {
            false
        }
    });

    if removed {
        let mut notice_bytes = Vec::new();
        let mut sched_ident = Vec::new();
        encode_varint_field(1, sched_id, &mut sched_ident);
        encode_bytes_field(1, &sched_ident, &mut notice_bytes);
        router_notices
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push((
                NoticeScope::User(uid as i32),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.CancelledNotice"
                    ),
                    value: notice_bytes,
                },
            ));
    }
    removed
}

pub fn publish_quick_pay_added_notice(
    pending_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    user_id: u64,
    token_id: u32,
) {
    let mut notice_bytes = Vec::new();
    let mut tok_ident = Vec::new();
    encode_varint_field(1, token_id as u64, &mut tok_ident);
    encode_bytes_field(1, &tok_ident, &mut notice_bytes);
    pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            NoticeScope::User(user_id as i32),
            GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.QuickPayAddedNotice"
                ),
                value: notice_bytes,
            },
        ));
}

pub fn add_user_quick_pay_token(
    db: &WorldRepository,
    pending_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    uid: u64,
    token: PaymentTokenRecord,
) {
    db.with_mut_snapshot(|snap| {
        let mut tokens: Vec<PaymentTokenRecord> = snap
            .service_state_blobs
            .get("payment_tokens")
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        tokens.retain(|t| !(t.id == token.id && t.user_id == uid));
        tokens.push(token.clone());
        if let Ok(serialized) = serde_json::to_string(&tokens) {
            snap.service_state_blobs.insert("payment_tokens".to_string(), serialized);
        }
    });
    if token.is_quick_pay {
        publish_quick_pay_added_notice(pending_notices, uid, token.id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlexTransactionRecord {
    pub id: u64,
    pub user_id: u64,
    pub amount: i64,
    pub balance_after: i64,
    pub timestamp_unix: u64,
    pub description: String,
}

pub fn get_plex_transactions(snap: &eve_db::WorldStateSnapshot) -> Vec<PlexTransactionRecord> {
    snap.service_state_blobs
        .get("plex_tx_logs")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

pub fn record_plex_transaction(
    snap: &mut eve_db::WorldStateSnapshot,
    tx_id: u64,
    user_id: u64,
    amount: i64,
    balance_after: i64,
    description: &str,
    now_unix: u64,
) {
    let mut logs = get_plex_transactions(snap);
    if logs.len() >= 1000 {
        logs.remove(0);
    }
    logs.push(PlexTransactionRecord {
        id: tx_id,
        user_id,
        amount,
        balance_after,
        timestamp_unix: now_unix,
        description: description.to_string(),
    });
    if let Ok(serialized) = serde_json::to_string(&logs) {
        snap.service_state_blobs.insert("plex_tx_logs".to_string(), serialized);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCategory {
    pub id: u32,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreProduct {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub type_id: u32,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreOffer {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub category_id: u32,
    pub price_plex: i64,
    pub product_ids: Vec<u32>,
    pub targeted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreCatalog {
    pub categories: Vec<StoreCategory>,
    pub products: Vec<StoreProduct>,
    pub offers: Vec<StoreOffer>,
}

pub fn save_store_catalog(snap: &mut eve_db::WorldStateSnapshot, catalog: &StoreCatalog) {
    if let Ok(serialized) = serde_json::to_string(catalog) {
        snap.service_state_blobs.insert("nes_store_catalog".to_string(), serialized);
    }
}

/// DATA GAP: NES store catalog (categories, products, offers) is live Quasar backend data not present in V24 client FSD.
/// Read persisted New Eden Store catalog from `WorldStateSnapshot.service_state_blobs["nes_store_catalog"]`.
pub fn read_store_catalog(snap: &eve_db::WorldStateSnapshot) -> StoreCatalog {
    snap.service_state_blobs
        .get("nes_store_catalog")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

pub fn read_store_offer_tags(
    snap: &eve_db::WorldStateSnapshot,
) -> std::collections::HashMap<u32, Vec<String>> {
    snap.service_state_blobs
        .get("nes_store_offer_tags")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

pub fn save_store_offer_tags(
    snap: &mut eve_db::WorldStateSnapshot,
    tags: &std::collections::HashMap<u32, Vec<String>>,
) {
    if let Ok(serialized) = serde_json::to_string(tags) {
        snap.service_state_blobs
            .insert("nes_store_offer_tags".to_string(), serialized);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentScheduleRecord {
    pub id: u64,
    pub user_id: u64,
    pub schedule_type: u32,
    pub next_payment_unix: u64,
    pub offer_id: String,
    pub interval_days: u32,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentTokenRecord {
    pub id: u32,
    pub user_id: u64,
    pub token_str: String,
    pub is_quick_pay: bool,
    #[serde(default)]
    pub consumed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentReceiptRecord {
    pub receipt_id: u64,
    pub user_id: u64,
    pub offer_id: String,
    pub amount_in_cents: u32,
    pub currency: String,
    pub timestamp_unix: u64,
}

pub fn register_plex_vault_extra_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    // 1. eve_public.plex.vault.api.PurchaseRequest -> PurchaseResponse
    let db_purch = Arc::clone(&db);
    let pending_purch = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.plex.vault.api.PurchaseRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx
                .effective_user_id()
                .ok_or_else(|| (STATUS_FORBIDDEN, "User authentication required".to_string()))?
                as u64;
            let offer_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::String(s) => s.parse::<u32>().ok(),
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) if *i > 0 => Some(*i as u32),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int64(i) if *i > 0 => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::String(s) => s.parse::<u32>().ok(),
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) if *i > 0 => Some(*i as u32),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int64(i) if *i > 0 => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid store offer ID".to_string()))?;

            let qty = match dyn_msg.get(2) {
                None => 1u32,
                Some(DynamicValue::Uint32(q)) if *q > 0 => *q,
                Some(DynamicValue::Uint64(q)) if *q > 0 => *q as u32,
                Some(DynamicValue::Int32(q)) if *q > 0 => *q as u32,
                Some(DynamicValue::Int64(q)) if *q > 0 => *q as u32,
                _ => return Err((STATUS_BAD_REQUEST, "Purchase quantity must be positive".to_string())),
            };

            let gift_char_id = match dyn_msg.get(3) {
                Some(DynamicValue::Message(gm)) => match gm.get(3) {
                    Some(DynamicValue::Message(cm)) => match cm.get(1) {
                        Some(DynamicValue::Uint64(c)) if *c > 0 => Some(*c as i64),
                        Some(DynamicValue::Int64(c)) if *c > 0 => Some(*c),
                        Some(DynamicValue::Uint32(c)) if *c > 0 => Some(*c as i64),
                        Some(DynamicValue::Int32(c)) if *c > 0 => Some(*c as i64),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            let active_cid = gift_char_id.or_else(|| ctx.effective_character_id().map(|c| c as i64));
            let now_unix = ctx.now_unix();

            let (tx_id, total_plex, new_bal, desc, maybe_omega) = db_purch
                .try_with_mut_snapshot(|snap| {
                    if let Some(gift_cid) = gift_char_id {
                        if !snap.characters.contains_key(&gift_cid) {
                            return Err(format!("{STATUS_NOT_FOUND}:Gift recipient character {gift_cid} not found"));
                        }
                    }
                    let catalog = read_store_catalog(snap);
                    let offer = catalog
                        .offers
                        .iter()
                        .find(|o| o.id == offer_id)
                        .cloned()
                        .ok_or_else(|| format!("{STATUS_NOT_FOUND}:Offer {offer_id} not found"))?;
                    if offer.price_plex < 0 {
                        return Err(format!("{STATUS_BAD_REQUEST}:Offer {offer_id} has invalid negative PLEX price"));
                    }
                    let total_cost = offer.price_plex.saturating_mul(qty as i64);

                    // Validate recipient character and station BEFORE deducting PLEX
                    let recipient_cid = active_cid.or_else(|| {
                        let mut chars: Vec<_> = snap
                            .characters
                            .values()
                            .filter(|c| c.user_id == uid)
                            .map(|c| c.character_id)
                            .collect();
                        chars.sort_unstable();
                        chars.first().copied()
                    });
                    let delivery_target = if !offer.product_ids.is_empty() {
                        let Some(cid) = recipient_cid else {
                            return Err(format!("{STATUS_BAD_REQUEST}:No valid recipient character for store delivery"));
                        };
                        let Some(ch) = snap.characters.get(&cid).cloned() else {
                            return Err(format!("{STATUS_NOT_FOUND}:Recipient character {cid} not found"));
                        };
                        let target_station = ch.station_id.or(ch.structure_id).unwrap_or(ch.home_station_id);
                        if target_station <= 0 {
                            return Err(format!("{STATUS_BAD_REQUEST}:Target character {cid} has no valid delivery station"));
                        }
                        let mut items_to_deliver = Vec::new();
                        for pid in &offer.product_ids {
                            if let Some(prod) = catalog.products.iter().find(|p| p.id == *pid) {
                                if prod.type_id > 0 && prod.quantity > 0 {
                                    let deliver_qty = i32::try_from(prod.quantity.saturating_mul(qty))
                                        .map_err(|_| format!("{STATUS_BAD_REQUEST}:Delivery quantity overflow"))?;
                                    items_to_deliver.push((prod.type_id, deliver_qty));
                                }
                            }
                        }
                        Some((cid, target_station, items_to_deliver))
                    } else if let Some(cid) = active_cid {
                        if !snap.characters.contains_key(&cid) {
                            return Err(format!("{STATUS_NOT_FOUND}:Recipient character {cid} not found"));
                        }
                        None
                    } else {
                        None
                    };

                    let cur = snap.account_plex_vault.entry(uid).or_insert(0);
                    if *cur < total_cost {
                        return Err(format!("{STATUS_BAD_REQUEST}:Insufficient PLEX balance"));
                    }
                    *cur -= total_cost;
                    let bal = *cur;
                    snap.next_entity_id += 1;
                    let tx_id = snap.next_entity_id as u64;
                    let desc = format!("NES Purchase: {}", offer.name);
                    record_plex_transaction(snap, tx_id, uid, -total_cost, bal, &desc, now_unix);

                    if let Some((cid, target_station, items_to_deliver)) = delivery_target {
                        for (type_id, deliver_qty) in items_to_deliver {
                            snap.next_entity_id += 1;
                            let item_id = snap.next_entity_id;
                            snap.items.insert(
                                item_id,
                                eve_db::InventoryItemRecord {
                                    item_id,
                                    type_id,
                                    owner_id: cid,
                                    location_id: target_station,
                                    flag: eve_db::InventoryFlag::Hangar,
                                    quantity: deliver_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    let omega_notice = if offer.category_id == 1 {
                        let omega_target_uid = recipient_cid
                            .and_then(|cid| snap.characters.get(&cid).map(|c| c.user_id))
                            .unwrap_or(uid);
                        let (_, n) = crate::services::quasar_auth_user::activate_user_omega_license_in_snapshot(
                            snap,
                            omega_target_uid,
                            30 * 86_400 * u64::from(qty),
                            now_unix,
                        );
                        Some((omega_target_uid, n))
                    } else {
                        None
                    };
                    Ok((tx_id, total_cost, bal, desc, omega_notice))
                })
                .map_err(|e| {
                    if let Some((c, m)) = e.split_once(':') {
                        if let Ok(code) = c.parse::<u32>() {
                            return (code, m.to_string());
                        }
                    }
                    (STATUS_BAD_REQUEST, e)
                })?;

            emit_balance_changed_notice(&pending_purch, uid, tx_id, -total_plex, new_bal, &desc, now_unix);
            if let Some((omega_target_uid, n)) = maybe_omega {
                pending_purch.write().unwrap_or_else(|e| e.into_inner()).push((NoticeScope::User(omega_target_uid as i32), n));
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.PurchaseResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 2. eve.plex.vault.assetholding.operation.api.GetRequest -> GetResponse
    let db_op = Arc::clone(&db);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_uid = ctx.effective_user_id().map(|u| u as u64);
            let is_admin = require_admin_role(ctx).is_ok();
            if caller_uid.is_none() && !is_admin {
                return Err((STATUS_FORBIDDEN, "User authentication required".to_string()));
            }
            let op_id = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Bytes(b)) if !b.is_empty() => {
                        std::str::from_utf8(b)
                            .ok()
                            .and_then(|s| s.parse::<u64>().ok())
                            .or_else(|| (b.len() >= 8).then(|| u64::from_be_bytes(b[b.len() - 8..].try_into().unwrap())))
                    }
                    Some(DynamicValue::Uint64(v)) => Some(*v),
                    Some(DynamicValue::Int64(v)) if *v > 0 => Some(*v as u64),
                    _ => None,
                },
                Some(DynamicValue::Uint64(v)) => Some(*v),
                _ => None,
            }
            .filter(|&id| id > 0)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing operation Identifier".to_string()))?;

            let tx = db_op
                .with_snapshot(|snap| get_plex_transactions(snap).into_iter().find(|t| t.id == op_id))
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("PLEX operation {op_id} not found")))?;
            if !is_admin && caller_uid != Some(tx.user_id) {
                return Err((STATUS_FORBIDDEN, "Cannot query another user's PLEX operation".to_string()));
            }

            let (mut u_b, mut c_b, mut attr_b, mut out) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            encode_varint_field(1, tx.user_id, &mut u_b);
            encode_bytes_field(1, &u_b, &mut attr_b);
            encode_varint_field(1, (tx.amount.abs() * 100) as u64, &mut c_b);
            encode_bytes_field(2, &c_b, &mut attr_b);
            encode_bytes_field(1, &attr_b, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 3. eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest -> RecordInvoiceResponse
    let db_inv = Arc::clone(&db);
    router.register_handler(
        "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let inv_uuid = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Bytes(b)) if !b.is_empty() => Some(b.clone()),
                    Some(DynamicValue::String(s)) if !s.is_empty() => Some(s.as_bytes().to_vec()),
                    _ => None,
                },
                _ => None,
            }
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing invoice Identifier.uuid".to_string()))?;
            let hex: String = inv_uuid.iter().map(|b| format!("{b:02x}")).collect();
            db_inv.with_mut_snapshot(|snap| {
                let mut list: Vec<String> = snap
                    .service_state_blobs
                    .get("plex_invoices")
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
                if !list.contains(&hex) {
                    if list.len() >= 1000 {
                        list.remove(0);
                    }
                    list.push(hex);
                }
                if let Ok(s) = serde_json::to_string(&list) {
                    snap.service_state_blobs.insert("plex_invoices".into(), s);
                }
            });
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 4. eve_public.store.offer.api.GetAllByTagRequest -> GetAllByTagResponse
    let db_tag_all = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.offer.api.GetAllByTagRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let tag = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.trim().to_string(),
                _ => String::new(),
            };
            if tag.is_empty() {
                return Err((STATUS_BAD_REQUEST, "Store offer tag is required".into()));
            }
            let matching_ids = db_tag_all.with_snapshot(|snap| {
                let cat = read_store_catalog(snap);
                let tags_map = read_store_offer_tags(snap);
                cat.offers
                    .into_iter()
                    .filter(|o| {
                        !o.targeted
                            && tags_map
                                .get(&o.id)
                                .is_some_and(|ts| ts.iter().any(|t| t.eq_ignore_ascii_case(&tag)))
                    })
                    .map(|o| o.id)
                    .collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for oid in matching_ids {
                let mut off_bytes = Vec::new();
                encode_string_field(1, &oid.to_string(), &mut off_bytes);
                encode_bytes_field(1, &off_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.offer.api.GetAllByTagResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve.store.admin.api.{CreateCategoryRequest,CreateProductRequest,CreateOfferRequest}
    let db_adm_cat = Arc::clone(&db);
    router.register_handler(
        "eve.store.admin.api.CreateCategoryRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let attrs = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => m,
                _ => return Err((STATUS_BAD_REQUEST, "Missing category attributes".into())),
            };
            let name = match attrs.get(1) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => return Err((STATUS_BAD_REQUEST, "Category name is required".into())),
            };
            let cat_enum = match attrs.get(6) {
                Some(DynamicValue::Int32(v)) if *v > 0 => *v as u64,
                Some(DynamicValue::Uint32(v)) if *v > 0 => *v as u64,
                _ => 1,
            };
            let id = db_adm_cat.with_mut_snapshot(|snap| {
                let mut catalog = read_store_catalog(snap);
                let next_id = catalog.categories.iter().map(|c| c.id).max().unwrap_or(0) + 1;
                catalog.categories.push(StoreCategory {
                    id: next_id,
                    name: name.clone(),
                    description: name,
                });
                save_store_catalog(snap, &catalog);
                next_id
            });
            let mut cid_b = Vec::new();
            encode_varint_field(1, u64::from(id), &mut cid_b);
            encode_varint_field(2, cat_enum, &mut cid_b);
            let mut out = Vec::new();
            encode_bytes_field(1, &cid_b, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.store.admin.api.CreateCategoryResponse"
                ),
                value: out,
            })
        }),
    );

    let db_adm_prod = Arc::clone(&db);
    router.register_handler(
        "eve.store.admin.api.CreateProductRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let cat_enum = match dyn_msg.get(1) {
                Some(DynamicValue::Int32(v)) if *v > 0 => *v as u64,
                Some(DynamicValue::Uint32(v)) if *v > 0 => *v as u64,
                _ => 1,
            };
            let attrs = match dyn_msg.get(2) {
                Some(DynamicValue::Message(m)) => m,
                _ => return Err((STATUS_BAD_REQUEST, "Missing product attributes".into())),
            };
            let name = match attrs.get(1) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => return Err((STATUS_BAD_REQUEST, "Product name is required".into())),
            };
            let type_id = match attrs.get(2) {
                Some(DynamicValue::Message(item)) => match item.get(1) {
                    Some(DynamicValue::Message(tid)) => match tid.get(1) {
                        Some(DynamicValue::Uint32(v)) => *v,
                        Some(DynamicValue::Uint64(v)) => *v as u32,
                        Some(DynamicValue::Int32(v)) if *v > 0 => *v as u32,
                        Some(DynamicValue::Int64(v)) if *v > 0 => *v as u32,
                        _ => 0,
                    },
                    _ => 0,
                },
                _ => 0,
            };
            let id = db_adm_prod.with_mut_snapshot(|snap| {
                let mut catalog = read_store_catalog(snap);
                let next_id = catalog.products.iter().map(|p| p.id).max().unwrap_or(100) + 1;
                catalog.products.push(StoreProduct {
                    id: next_id,
                    name: name.clone(),
                    description: name,
                    type_id,
                    quantity: 1,
                });
                save_store_catalog(snap, &catalog);
                next_id
            });
            let mut pid_b = Vec::new();
            encode_varint_field(1, u64::from(id), &mut pid_b);
            encode_varint_field(2, cat_enum, &mut pid_b);
            let mut out = Vec::new();
            encode_bytes_field(1, &pid_b, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.store.admin.api.CreateProductResponse"
                ),
                value: out,
            })
        }),
    );

    let db_adm_off = Arc::clone(&db);
    let pending_adm_off = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.store.admin.api.CreateOfferRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let attrs = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => m,
                _ => return Err((STATUS_BAD_REQUEST, "Missing offer attributes".into())),
            };
            let name = match attrs.get(1) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => return Err((STATUS_BAD_REQUEST, "Offer name is required".into())),
            };
            let price_plex = match attrs.get(5) {
                Some(DynamicValue::Message(pp)) => match pp.get(1) {
                    Some(DynamicValue::Int64(c)) => *c / 100,
                    Some(DynamicValue::Uint64(c)) => (*c as i64) / 100,
                    Some(DynamicValue::Int32(c)) => (*c as i64) / 100,
                    Some(DynamicValue::Uint32(c)) => (*c as i64) / 100,
                    _ => 0,
                },
                _ => 0,
            };
            let targeted = matches!(attrs.get(7), Some(DynamicValue::Bool(true)) | Some(DynamicValue::Uint64(1)));
            let tags: Vec<String> = match attrs.get(16) {
                Some(DynamicValue::Repeated(list)) => list
                    .iter()
                    .filter_map(|v| match v {
                        DynamicValue::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let category_id = match attrs.get(17) {
                Some(DynamicValue::Repeated(list)) => list
                    .first()
                    .and_then(|v| match v {
                        DynamicValue::Message(m) => match m.get(1) {
                            Some(DynamicValue::Uint32(u)) => Some(*u),
                            Some(DynamicValue::Uint64(u)) => Some(*u as u32),
                            Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u32),
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap_or(1),
                _ => 1,
            };
            let product_ids: Vec<u32> = match attrs.get(18) {
                Some(DynamicValue::Repeated(list)) => list
                    .iter()
                    .filter_map(|v| match v {
                        DynamicValue::Message(pm) => match pm.get(1) {
                            Some(DynamicValue::Message(pid)) => match pid.get(1) {
                                Some(DynamicValue::Uint32(u)) => Some(*u),
                                Some(DynamicValue::Uint64(u)) => Some(*u as u32),
                                Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u32),
                                _ => None,
                            },
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let id = db_adm_off.with_mut_snapshot(|snap| {
                let mut catalog = read_store_catalog(snap);
                let next_id = catalog.offers.iter().map(|o| o.id).max().unwrap_or(1000) + 1;
                catalog.offers.push(StoreOffer {
                    id: next_id,
                    name: name.clone(),
                    description: name,
                    category_id,
                    price_plex,
                    product_ids,
                    targeted,
                });
                save_store_catalog(snap, &catalog);
                if !tags.is_empty() {
                    let mut tags_map = read_store_offer_tags(snap);
                    tags_map.insert(next_id, tags);
                    save_store_offer_tags(snap, &tags_map);
                }
                next_id
            });
            if targeted {
                let uid = ctx.effective_user_id().unwrap_or(1) as u64;
                let (mut u_b, mut c_b, mut oid_b, mut nb) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                encode_varint_field(1, uid, &mut u_b);
                encode_string_field(1, &id.to_string(), &mut c_b);
                encode_bytes_field(1, &u_b, &mut oid_b);
                encode_bytes_field(2, &c_b, &mut oid_b);
                encode_bytes_field(1, &oid_b, &mut nb);
                pending_adm_off.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::Broadcast,
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.targetaudience.offer.api.PromotedNotice"),
                        value: nb,
                    },
                ));
            }
            let mut off_b = Vec::new();
            encode_string_field(1, &id.to_string(), &mut off_b);
            let mut out = Vec::new();
            encode_bytes_field(1, &off_b, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.store.admin.api.CreateOfferResponse"
                ),
                value: out,
            })
        }),
    );

    // 6. eve.payment.purchase.api.CreditCardRequest -> fail-closed (real-money backend disabled)
    router.register_handler(
        "eve.payment.purchase.api.CreditCardRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            // DATA GAP: feat:quasar.payment acceptance: (real-money backend; stub as disabled)
            Err((
                STATUS_FORBIDDEN,
                "Real-money payment backend is disabled".into(),
            ))
        }),
    );
}
