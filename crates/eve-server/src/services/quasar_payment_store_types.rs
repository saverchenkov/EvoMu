//! Shared types and default data for Quasar Payment, PLEX Vault, and NES Store.

use crate::quasar_gateway::NoticeScope;
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

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
