//! Shared types and default data for Quasar Payment, PLEX Vault, and NES Store.

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64, DynamicValue,
    GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::{Arc, RwLock};

pub(crate) fn encode_plex_balance_changed_notice(
    user_id: u64,
    tx_id: u64,
    amount: i64,
    balance_after: i64,
    description: &str,
    now_unix: u64,
) -> GoogleAny {
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
    encode_varint_field(1, encode_zigzag64(amount * 100), &mut amt_curr);
    encode_bytes_field(4, &amt_curr, &mut attr);

    let mut bal_curr = Vec::new();
    encode_varint_field(1, encode_zigzag64(balance_after * 100), &mut bal_curr);
    encode_bytes_field(5, &bal_curr, &mut attr);

    encode_string_field(6, description, &mut attr);
    encode_bytes_field(2, &attr, &mut notice_bytes);

    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"),
        value: notice_bytes,
    }
}

pub(crate) fn queue_plex_balance_changed_notice(
    user_id: u64,
    tx_id: u64,
    amount: i64,
    balance_after: i64,
    description: &str,
    now_unix: u64,
) {
    let any = encode_plex_balance_changed_notice(user_id, tx_id, amount, balance_after, description, now_unix);
    super::quasar_social_chat::ctx_queue_notice(NoticeScope::User(user_id as i32), any, now_unix);
}

pub fn emit_balance_changed_notice(
    router_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    user_id: u64,
    tx_id: u64,
    amount: i64,
    balance_after: i64,
    description: &str,
    now_unix: u64,
) {
    let notice = encode_plex_balance_changed_notice(user_id, tx_id, amount, balance_after, description, now_unix);
    router_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((NoticeScope::User(user_id as i32), notice));
}

pub fn cancel_user_payment_schedule(
    db: &WorldRepository,
    router_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    uid: u64,
    sched_id: u64,
) -> bool {
    let removed_type = db.with_mut_snapshot(|snap| {
        let scheds = &mut snap.quasar_commerce.payment_schedules;
        let stype = scheds
            .iter()
            .find(|s| s.id == sched_id && s.user_id == uid)
            .map(|s| s.schedule_type);
        if stype.is_some() {
            scheds.retain(|s| !(s.id == sched_id && s.user_id == uid));
        }
        stype
    });

    if let Some(sched_type) = removed_type {
        let mut notice_bytes = Vec::new();
        let mut sched_ident = Vec::new();
        encode_varint_field(1, sched_id, &mut sched_ident);
        encode_bytes_field(1, &sched_ident, &mut notice_bytes);
        encode_varint_field(2, u64::from(sched_type), &mut notice_bytes);
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
    removed_type.is_some()
}

pub fn process_due_payment_schedules(
    db: &WorldRepository,
    router_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    now_unix: u64,
) {
    let has_due = db.with_snapshot(|snap| {
        snap.quasar_commerce
            .payment_schedules
            .iter()
            .any(|s| s.next_payment_unix > 0 && s.next_payment_unix <= now_unix)
    });
    if !has_due {
        return;
    }
    let failed_scheds: Vec<(u64, u64, u32)> = db.with_mut_snapshot(|snap| {
        if !snap
            .quasar_commerce
            .payment_schedules
            .iter()
            .any(|s| s.next_payment_unix > 0 && s.next_payment_unix <= now_unix)
        {
            return Vec::new();
        }
        let catalog = read_store_catalog(snap);
        let tokens = snap.quasar_commerce.payment_tokens.clone();
        let next_entity_id = &mut snap.next_entity_id;
        let commerce = &mut snap.quasar_commerce;
        let scheds = &mut commerce.payment_schedules;
        let receipts = &mut commerce.payment_receipts;
        let mut omega_renewals: Vec<(u64, u64)> = Vec::new();
        let mut failed = Vec::new();
        scheds.retain_mut(|s| {
            if s.next_payment_unix == 0 || s.next_payment_unix > now_unix {
                return true;
            }
            if tokens.iter().any(|t| t.user_id == s.user_id && t.is_quick_pay && !t.consumed) {
                let interval_secs = u64::from(s.interval_days.max(1)) * 86_400;
                s.next_payment_unix = now_unix + interval_secs;
                let numeric_offer_id = s
                    .offer_id
                    .strip_prefix("VEVC-")
                    .unwrap_or(&s.offer_id)
                    .parse::<u32>()
                    .ok();
                let offer = numeric_offer_id
                    .and_then(|oid| catalog.offers.iter().find(|o| o.id == oid));
                let amount_in_cents = offer
                    .and_then(|o| o.price_cents)
                    .map(|c| c.min(u64::from(u32::MAX)) as u32)
                    .unwrap_or(0);
                let currency = offer
                    .and_then(|o| o.currency.clone())
                    .unwrap_or_else(|| "USD".to_string());
                *next_entity_id += 1;
                let rid = *next_entity_id as u64;
                receipts.push(PaymentReceiptRecord {
                    receipt_id: rid,
                    user_id: s.user_id,
                    offer_id: s.offer_id.clone(),
                    amount_in_cents,
                    currency,
                    timestamp_unix: now_unix,
                });
                if s.schedule_type == 1 {
                    omega_renewals.push((s.user_id, interval_secs));
                }
                true
            } else {
                failed.push((s.user_id, s.id, s.schedule_type));
                false
            }
        });
        for (uid, interval_secs) in omega_renewals {
            let _ = crate::services::quasar_auth_user::activate_user_omega_license_in_snapshot(
                snap,
                uid,
                interval_secs,
                now_unix,
            );
        }
        failed
    });

    if !failed_scheds.is_empty() {
        for (uid, sched_id, sched_type) in failed_scheds {
            let mut notice_bytes = Vec::new();
            let mut sched_ident = Vec::new();
            encode_varint_field(1, sched_id, &mut sched_ident);
            encode_bytes_field(1, &sched_ident, &mut notice_bytes);
            encode_varint_field(2, u64::from(sched_type), &mut notice_bytes);
            router_notices
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.PaymentFailedNotice"
                        ),
                        value: notice_bytes.clone(),
                    },
                ));
            router_notices
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.CancelledNotice"
                        ),
                        value: notice_bytes.clone(),
                    },
                ));
            crate::services::quasar_social_chat::ctx_queue_notice(
                NoticeScope::User(uid as i32),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.PaymentFailedNotice"
                    ),
                    value: notice_bytes.clone(),
                },
                now_unix,
            );
            crate::services::quasar_social_chat::ctx_queue_notice(
                NoticeScope::User(uid as i32),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.CancelledNotice"
                    ),
                    value: notice_bytes,
                },
                now_unix,
            );
        }
    }
}

pub fn upsert_user_payment_schedule_in_snapshot(
    snap: &mut eve_db::WorldStateSnapshot,
    uid: u64,
    schedule_type: u32,
    next_payment_unix: u64,
    offer_id: &str,
    interval_days: u32,
    now_unix: u64,
) -> u64 {
    let existing_id = snap
        .quasar_commerce
        .payment_schedules
        .iter_mut()
        .find(|s| s.user_id == uid && s.schedule_type == schedule_type)
        .map(|existing| {
            existing.next_payment_unix = next_payment_unix;
            existing.offer_id = offer_id.to_string();
            existing.interval_days = interval_days;
            existing.id
        });
    if let Some(id) = existing_id {
        id
    } else {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id as u64;
        snap.quasar_commerce
            .payment_schedules
            .push(PaymentScheduleRecord {
                id,
                user_id: uid,
                schedule_type,
                next_payment_unix,
                offer_id: offer_id.to_string(),
                interval_days,
                created_unix: now_unix,
            });
        id
    }
}

pub fn publish_schedule_set_notice(
    pending_notices: &RwLock<Vec<(NoticeScope, GoogleAny)>>,
    user_id: u64,
    sched_id: u64,
    schedule_type: u32,
) {
    let mut notice_bytes = Vec::new();
    let mut sched_ident = Vec::new();
    encode_varint_field(1, sched_id, &mut sched_ident);
    encode_bytes_field(1, &sched_ident, &mut notice_bytes);
    encode_varint_field(2, u64::from(schedule_type), &mut notice_bytes);
    pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            NoticeScope::User(user_id as i32),
            GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.SetNotice"
                ),
                value: notice_bytes,
            },
        ));
}

pub fn encode_targeted_offer_list_response(
    offers: &[StoreOffer],
    uid: u64,
    dyn_msg: &eve_proto::quasar::DynamicQuasarMessage,
) -> Vec<u8> {
    let (page_size, offset) = match dyn_msg.get(1) {
        Some(DynamicValue::Message(page)) => {
            let sz = page
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) if *u > 0 => Some(*u as usize),
                    DynamicValue::Int32(i) if *i > 0 => Some(*i as usize),
                    DynamicValue::Uint64(u) if *u > 0 => Some(*u as usize),
                    _ => None,
                })
                .unwrap_or(50);
            let off = page
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::String(s) => s.parse::<usize>().ok(),
                    _ => None,
                })
                .unwrap_or(0);
            (sz, off)
        }
        _ => (50, 0),
    };
    let targeted: Vec<&StoreOffer> = offers.iter().filter(|o| o.targeted).collect();
    let end = offset.saturating_add(page_size).min(targeted.len());
    let slice = if offset < targeted.len() {
        &targeted[offset..end]
    } else {
        &[]
    };
    let mut out = Vec::new();
    for off in slice {
        let (mut u_b, mut o_b, mut entry) = (Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, uid, &mut u_b);
        encode_string_field(1, &off.id.to_string(), &mut o_b);
        encode_bytes_field(1, &u_b, &mut entry);
        encode_bytes_field(2, &o_b, &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    if end < targeted.len() {
        let mut next_page = Vec::new();
        encode_string_field(1, &end.to_string(), &mut next_page);
        encode_bytes_field(2, &next_page, &mut out);
    }
    out
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
        let tokens = &mut snap.quasar_commerce.payment_tokens;
        tokens.retain(|t| !(t.id == token.id && t.user_id == uid));
        tokens.push(token.clone());
    });
    if token.is_quick_pay {
        publish_quick_pay_added_notice(pending_notices, uid, token.id);
    }
}

pub fn register_user_payment_token(
    ctx: &crate::router::ServerContext,
    uid: u64,
    token: PaymentTokenRecord,
) {
    let token_id = token.id;
    let is_qp = token.is_quick_pay;
    ctx.db.with_mut_snapshot(|snap| {
        let tokens = &mut snap.quasar_commerce.payment_tokens;
        tokens.retain(|t| !(t.id == token_id && t.user_id == uid));
        tokens.push(token);
    });
    if is_qp {
        let mut notice_bytes = Vec::new();
        let mut tok_ident = Vec::new();
        encode_varint_field(1, u64::from(token_id), &mut tok_ident);
        encode_bytes_field(1, &tok_ident, &mut notice_bytes);
        let _ = ctx.quasar_gateway.publish_notice(
            NoticeScope::User(uid as i32),
            GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.QuickPayAddedNotice"
                ),
                value: notice_bytes,
            },
            ctx.now_unix(),
        );
    }
}

pub use eve_db::{
    PaymentReceiptRecord, PaymentScheduleRecord, PaymentTokenRecord, PlexTransactionRecord,
    StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
};

pub fn get_plex_transactions(snap: &eve_db::WorldStateSnapshot) -> Vec<PlexTransactionRecord> {
    snap.quasar_commerce.plex_tx_logs.clone()
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
    let logs = &mut snap.quasar_commerce.plex_tx_logs;
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
}

pub fn save_store_catalog(snap: &mut eve_db::WorldStateSnapshot, catalog: &StoreCatalog) {
    snap.quasar_commerce.nes_store_catalog = Some(catalog.clone());
}

pub(crate) fn seed_default_store_catalog_if_empty(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
) {
    if snap.quasar_commerce.nes_store_catalog.is_some() {
        return;
    }
    let type_name = |tid: u32| sde.types.get(&tid).map(|t| t.name.clone()).unwrap_or_default();
    let cat_name = |cid: u32, fallback: &str| {
        sde.categories.get(&cid).map(|c| c.name.clone()).unwrap_or_else(|| fallback.to_string())
    };
    let catalog = StoreCatalog {
        categories: vec![
            StoreCategory { id: 1, name: "Clone State & Omega".into(), description: "Omega Clone State subscriptions".into() },
            StoreCategory { id: 2, name: cat_name(91, "Ship SKINs"), description: "Permanent Ship Cosmetic Licenses".into() },
            StoreCategory { id: 3, name: cat_name(5, "Accessories"), description: "Pilot services and skill extraction".into() },
            StoreCategory { id: 4, name: cat_name(30, "Apparel"), description: "Pilot customizations and vanity items".into() },
        ],
        products: vec![
            StoreProduct { id: 101, name: "30 Days Omega Clone State".into(), description: "30 Days Omega clone subscription".into(), type_id: 0, quantity: 1 },
            StoreProduct { id: 102, name: type_name(34599), description: type_name(34599), type_id: 34599, quantity: 1 },
            StoreProduct { id: 103, name: type_name(40519), description: type_name(40519), type_id: 40519, quantity: 1 },
            StoreProduct { id: 104, name: type_name(3966), description: type_name(3966), type_id: 3966, quantity: 1 },
        ],
        offers: vec![
            StoreOffer { id: 1001, name: "30 Days Omega Clone State".into(), description: "30 Days Omega clone subscription".into(), category_id: 1, price_plex: 500, product_ids: vec![101], targeted: false, price_cents: Some(1_999), currency: Some("USD".into()) },
            StoreOffer { id: 1002, name: type_name(34599), description: type_name(34599), category_id: 2, price_plex: 110, product_ids: vec![102], targeted: false, price_cents: Some(499), currency: Some("USD".into()) },
            StoreOffer { id: 1003, name: type_name(40519), description: type_name(40519), category_id: 3, price_plex: 140, product_ids: vec![103], targeted: false, price_cents: Some(699), currency: Some("USD".into()) },
            StoreOffer { id: 1004, name: type_name(3966), description: type_name(3966), category_id: 4, price_plex: 75, product_ids: vec![104], targeted: true, price_cents: Some(399), currency: Some("USD".into()) },
        ],
    };
    save_store_catalog(snap, &catalog);
}

/// DATA GAP: NES store catalog (categories, products, offers) is live Quasar backend data not present in V24 client FSD.
/// Read persisted New Eden Store catalog from `WorldStateSnapshot.quasar_commerce.nes_store_catalog`.
pub fn read_store_catalog(snap: &eve_db::WorldStateSnapshot) -> StoreCatalog {
    snap.quasar_commerce
        .nes_store_catalog
        .clone()
        .unwrap_or_default()
}

pub fn read_store_offer_tags(
    snap: &eve_db::WorldStateSnapshot,
) -> std::collections::HashMap<u32, Vec<String>> {
    snap.quasar_commerce.nes_store_offer_tags.clone()
}

pub fn save_store_offer_tags(
    snap: &mut eve_db::WorldStateSnapshot,
    tags: &std::collections::HashMap<u32, Vec<String>>,
) {
    snap.quasar_commerce.nes_store_offer_tags = tags.clone();
}

pub fn register_plex_vault_extra_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    free_fast_checkout_plex: Arc<std::sync::atomic::AtomicBool>,
) {
    // 1. eve_public.plex.vault.api.PurchaseRequest -> PurchaseResponse
    let db_purch = Arc::clone(&db);
    let pending_purch = Arc::clone(&router.pending_notices);
    let allow_free_plex = free_fast_checkout_plex;
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
                    if offer.price_plex == 0 && !allow_free_plex.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(format!("{STATUS_BAD_REQUEST}:Offer {offer_id} has invalid zero PLEX price"));
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

                    let Some(bal) = snap.adjust_unified_plex_for_user(uid, -total_cost) else {
                        return Err(format!("{STATUS_BAD_REQUEST}:Insufficient PLEX balance"));
                    };
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
                let list = &mut snap.quasar_commerce.plex_invoices;
                if !list.contains(&hex) {
                    if list.len() >= 1000 {
                        list.remove(0);
                    }
                    list.push(hex);
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
            let parse_dyn_u32 = |v: &DynamicValue| -> Option<u32> {
                match v {
                    DynamicValue::Uint32(n) => Some(*n),
                    DynamicValue::Uint64(n) => Some(*n as u32),
                    DynamicValue::Int32(n) if *n > 0 => Some(*n as u32),
                    DynamicValue::Int64(n) if *n > 0 => Some(*n as u32),
                    DynamicValue::Message(m) => match m.get(1) {
                        Some(DynamicValue::Uint32(n)) => Some(*n),
                        Some(DynamicValue::Uint64(n)) => Some(*n as u32),
                        Some(DynamicValue::Int32(n)) if *n > 0 => Some(*n as u32),
                        Some(DynamicValue::Int64(n)) if *n > 0 => Some(*n as u32),
                        _ => None,
                    },
                    _ => None,
                }
            };
            let quantity = attrs
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Message(item) => item.get(2).and_then(parse_dyn_u32),
                    _ => None,
                })
                .or_else(|| attrs.get(3).and_then(parse_dyn_u32))
                .filter(|&q| q > 0)
                .unwrap_or(1);
            let id = db_adm_prod.with_mut_snapshot(|snap| {
                let mut catalog = read_store_catalog(snap);
                let next_id = catalog.products.iter().map(|p| p.id).max().unwrap_or(100) + 1;
                catalog.products.push(StoreProduct {
                    id: next_id,
                    name: name.clone(),
                    description: name,
                    type_id,
                    quantity,
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
            let (price_cents, currency) = match attrs.get(3) {
                Some(DynamicValue::Repeated(list)) => list
                    .iter()
                    .find_map(|v| match v {
                        DynamicValue::Message(cp) => {
                            let amt = match cp.get(2) {
                                Some(DynamicValue::Int64(c)) if *c > 0 => Some(*c as u64),
                                Some(DynamicValue::Uint64(c)) if *c > 0 => Some(*c),
                                Some(DynamicValue::Int32(c)) if *c > 0 => Some(*c as u64),
                                Some(DynamicValue::Uint32(c)) if *c > 0 => Some(*c as u64),
                                _ => None,
                            }?;
                            let code = match cp.get(1) {
                                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                                _ => "USD".to_string(),
                            };
                            Some((Some(amt), Some(code)))
                        }
                        _ => None,
                    })
                    .unwrap_or((None, None)),
                _ => (None, None),
            };
            let tags: Vec<String> = match attrs.get(16) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => {
                    vec![s.trim().to_string()]
                }
                Some(DynamicValue::Repeated(list)) => list
                    .iter()
                    .filter_map(|v| match v {
                        DynamicValue::String(s) if !s.trim().is_empty() => {
                            Some(s.trim().to_string())
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            fn parse_proto_varint_field1(mut bytes: &[u8]) -> Option<u32> {
                if bytes.len() < 2 || bytes[0] != 0x08 {
                    return None;
                }
                bytes = &bytes[1..];
                let mut val = 0_u64;
                let mut shift = 0_u32;
                for &b in bytes {
                    val |= u64::from(b & 0x7f) << shift;
                    if b & 0x80 == 0 {
                        return u32::try_from(val).ok().filter(|&v| v > 0);
                    }
                    shift = shift.saturating_add(7);
                    if shift >= 64 {
                        break;
                    }
                }
                None
            }
            fn parse_nested_proto_varint_field1(bytes: &[u8]) -> Option<u32> {
                if bytes.len() >= 4 && bytes[0] == 0x0a {
                    let len = usize::from(bytes[1]);
                    if bytes.len() >= 2 + len {
                        return parse_proto_varint_field1(&bytes[2..2 + len]);
                    }
                }
                parse_proto_varint_field1(bytes)
            }
            let extract_cat_id = |v: &DynamicValue| -> Option<u32> {
                match v {
                    DynamicValue::Message(m) => match m.get(1) {
                        Some(DynamicValue::Uint32(u)) => Some(*u),
                        Some(DynamicValue::Uint64(u)) => Some(*u as u32),
                        Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u32),
                        Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u32),
                        _ => None,
                    },
                    DynamicValue::String(s) => parse_proto_varint_field1(s.as_bytes()),
                    DynamicValue::Bytes(b) => parse_proto_varint_field1(b),
                    _ => None,
                }
            };
            let category_id = match attrs.get(17) {
                Some(DynamicValue::Repeated(list)) => list.first().and_then(extract_cat_id).unwrap_or(1),
                Some(single) => extract_cat_id(single).unwrap_or(1),
                None => 1,
            };
            let extract_prod_id = |v: &DynamicValue| -> Option<u32> {
                match v {
                    DynamicValue::Message(pm) => match pm.get(1) {
                        Some(DynamicValue::Message(pid)) => match pid.get(1) {
                            Some(DynamicValue::Uint32(u)) => Some(*u),
                            Some(DynamicValue::Uint64(u)) => Some(*u as u32),
                            Some(DynamicValue::Int32(i)) if *i > 0 => Some(*i as u32),
                            Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u32),
                            _ => None,
                        },
                        Some(DynamicValue::String(s)) => parse_proto_varint_field1(s.as_bytes()),
                        Some(DynamicValue::Bytes(b)) => parse_proto_varint_field1(b),
                        _ => None,
                    },
                    DynamicValue::String(s) => parse_nested_proto_varint_field1(s.as_bytes()),
                    DynamicValue::Bytes(b) => parse_nested_proto_varint_field1(b),
                    _ => None,
                }
            };
            let product_ids: Vec<u32> = match attrs.get(18) {
                Some(DynamicValue::Repeated(list)) => list.iter().filter_map(extract_prod_id).collect(),
                Some(single) => extract_prod_id(single).into_iter().collect(),
                None => Vec::new(),
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
                    price_cents,
                    currency,
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

#[cfg(test)]
#[path = "quasar_payment_store_types_tests.rs"]
mod quasar_payment_store_types_tests;
