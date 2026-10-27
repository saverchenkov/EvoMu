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

/// Read persisted New Eden Store catalog from `WorldStateSnapshot.service_state_blobs["nes_store_catalog"]`.
///
/// DATA GAP: NES store catalog (categories, products, offers) is live Quasar backend data not present in V24 client FSD.
pub fn read_store_catalog(snap: &eve_db::WorldStateSnapshot) -> StoreCatalog {
    snap.service_state_blobs
        .get("nes_store_catalog")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
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
                    let total_cost = offer.price_plex.max(1).saturating_mul(qty as i64);
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
                    if let Some(cid) = recipient_cid {
                        if let Some(ch) = snap.characters.get(&cid).cloned() {
                            let target_station = ch.station_id.or(ch.structure_id).unwrap_or(ch.home_station_id);
                            if target_station > 0 {
                                for pid in &offer.product_ids {
                                    if let Some(prod) = catalog.products.iter().find(|p| p.id == *pid) {
                                        if prod.type_id > 0 && prod.quantity > 0 {
                                            let deliver_qty = i32::try_from(prod.quantity.saturating_mul(qty))
                                                .map_err(|_| format!("{STATUS_BAD_REQUEST}:Delivery quantity overflow"))?;
                                            snap.next_entity_id += 1;
                                            let item_id = snap.next_entity_id;
                                            snap.items.insert(
                                                item_id,
                                                eve_db::InventoryItemRecord {
                                                    item_id,
                                                    type_id: prod.type_id,
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
                                }
                            }
                        }
                    }
                    let omega_notice = if offer.category_id == 1 {
                        let (_, n) = crate::services::quasar_auth_user::activate_user_omega_license_in_snapshot(
                            snap,
                            uid,
                            30 * 86_400 * u64::from(qty),
                            now_unix,
                        );
                        Some(n)
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
            if let Some(n) = maybe_omega {
                pending_purch.write().unwrap_or_else(|e| e.into_inner()).push((NoticeScope::User(uid as i32), n));
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
}
