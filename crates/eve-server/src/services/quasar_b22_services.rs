//! Batch 22 Quasar protobuf service handlers:
//! - `eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest`
//! - `eve_public.cosmetic.market.transaction.api.GetRequest`
//! - `eve_public.cosmetic.market.transaction.api.GetAllRequest`

#![forbid(unsafe_code)]

use super::quasar_cosmetics_drafts::{
    encode_draft_attributes, extract_id_from_uuid_bytes, id_to_uuid_bytes,
};
use super::quasar_cosmetics_market_types::CosmeticsMarketData;
use super::quasar_cosmetics_services::QuasarCosmeticsState;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

fn require_character(ctx: &AuthoritativeContext) -> Result<u32, (u32, String)> {
    ctx.effective_character_id()
        .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))
}

fn extract_uint32_field(_dyn_msg: &DynamicQuasarMessage, raw_bytes: &[u8], field_num: u32) -> Option<u32> {
    if let Ok(wm) = WireMessage::decode(raw_bytes) {
        if let Some(f) = wm.fields.iter().find(|f| f.field_number == field_num) {
            if let Some(v) = f.value.as_varint() {
                return u32::try_from(v).ok();
            }
            if let Some(b) = f.value.as_bytes() {
                if let Ok(sub) = WireMessage::decode(b) {
                    if let Some(sf) = sub.fields.iter().find(|f| f.field_number == 1) {
                        if let Some(v) = sf.value.as_varint() {
                            return u32::try_from(v).ok();
                        }
                    }
                }
            }
        }
    }
    None
}

fn extract_uuid_field(dyn_msg: &DynamicQuasarMessage, raw_bytes: &[u8], field_num: u32) -> Option<Vec<u8>> {
    match dyn_msg.fields.get(&field_num) {
        Some(DynamicValue::Message(sub)) => {
            if let Some(DynamicValue::Bytes(b)) = sub.fields.get(&1) {
                return Some(b.clone());
            }
        }
        Some(DynamicValue::Bytes(b)) => {
            if b.len() == 16 {
                return Some(b.clone());
            }
            if let Ok(sub) = WireMessage::decode(b) {
                if let Some(sf) = sub.fields.iter().find(|f| f.field_number == 1) {
                    if let Some(sb) = sf.value.as_bytes() {
                        return Some(sb.to_vec());
                    }
                }
            }
        }
        _ => {}
    }
    if let Ok(wm) = WireMessage::decode(raw_bytes) {
        if let Some(f) = wm.fields.iter().find(|f| f.field_number == field_num) {
            if let Some(b) = f.value.as_bytes() {
                if b.len() == 16 {
                    return Some(b.to_vec());
                }
                if let Ok(sub) = WireMessage::decode(b) {
                    if let Some(sf) = sub.fields.iter().find(|f| f.field_number == 1) {
                        if let Some(sb) = sf.value.as_bytes() {
                            return Some(sb.to_vec());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Registers Batch 22 Quasar protobuf handlers on `router`.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    _sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest -> GetSharedResponse
    {
        let db_shared = Arc::clone(&db);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest",
            Arc::new(move |ctx, req_any, dyn_msg| {
                let _caller = require_character(ctx)?;
                let target_char = extract_uint32_field(dyn_msg, &req_any.value, 1)
                    .filter(|&c| c > 0)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid character ID".to_string()))?;
                let uuid = extract_uuid_field(dyn_msg, &req_any.value, 2)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing draft UUID".to_string()))?;
                if uuid.len() != 16 {
                    return Err((STATUS_BAD_REQUEST, "Draft UUID must be 16 bytes".to_string()));
                }
                let draft_id = extract_id_from_uuid_bytes(&uuid);
                let state = QuasarCosmeticsState::from_db(Arc::clone(&db_shared));
                let data = state.data.read().unwrap_or_else(|e| e.into_inner());
                let record = data
                    .saved_drafts
                    .get(&target_char)
                    .and_then(|drafts| {
                        drafts
                            .get(&draft_id)
                            .or_else(|| drafts.values().find(|r| r.uuid.as_slice() == uuid.as_slice()))
                    })
                    .cloned()
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Shared draft {draft_id} not found for character {target_char}")))?;
                let attrs = encode_draft_attributes(&record);
                let mut out = Vec::new();
                encode_bytes_field(1, &attrs, &mut out);
                Ok(any(
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedResponse",
                    out,
                ))
            }),
        );
    }

    // 2. eve_public.cosmetic.market.transaction.api.GetRequest -> GetResponse
    {
        let db_tx = Arc::clone(&db);
        router.register_handler(
            "eve_public.cosmetic.market.transaction.api.GetRequest",
            Arc::new(move |ctx, req_any, dyn_msg| {
                let _caller = require_character(ctx)?;
                let uuid = extract_uuid_field(dyn_msg, &req_any.value, 1)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing transaction UUID".to_string()))?;
                if uuid.len() != 16 {
                    return Err((STATUS_BAD_REQUEST, "Transaction UUID must be 16 bytes".to_string()));
                }
                let tx_id = extract_id_from_uuid_bytes(&uuid);
                let found = db_tx.with_snapshot(|snap| {
                    let market: CosmeticsMarketData = snap
                        .quasar_state_blobs
                        .get("quasar_cosmetics_market")
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    let in_market = market.skin_listings.values().any(|l| l.uuid.as_slice() == uuid.as_slice())
                        || (tx_id > 0 && tx_id <= market.next_seq);
                    let in_journal = snap.wallet_journal.iter().any(|j| j.entry_id as u64 == tx_id);
                    in_market || in_journal
                });
                if !found {
                    return Err((STATUS_NOT_FOUND, format!("Cosmetic market transaction {tx_id} not found")));
                }
                let mut out = Vec::new();
                // status = 1 (STATUS_SUCCEEDED)
                encode_varint_field(1, 1, &mut out);
                Ok(any(
                    "eve_public.cosmetic.market.transaction.api.GetResponse",
                    out,
                ))
            }),
        );
    }

    // 3. eve_public.cosmetic.market.transaction.api.GetAllRequest -> GetAllResponse
    {
        let db_tx_all = Arc::clone(&db);
        router.register_handler(
            "eve_public.cosmetic.market.transaction.api.GetAllRequest",
            Arc::new(move |ctx, _req_any, _dyn_msg| {
                let caller = require_character(ctx)?;
                let uuids = db_tx_all.with_snapshot(|snap| {
                    let market: CosmeticsMarketData = snap
                        .quasar_state_blobs
                        .get("quasar_cosmetics_market")
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    let mut list: Vec<[u8; 16]> = market
                        .skin_listings
                        .values()
                        .filter(|l| l.seller_id == caller)
                        .map(|l| l.uuid)
                        .collect();
                    if list.is_empty() && market.next_seq > 0 {
                        list.push(id_to_uuid_bytes(market.next_seq));
                    }
                    list
                });
                let mut out = Vec::new();
                for u in uuids {
                    let mut id_msg = Vec::new();
                    encode_bytes_field(1, &u, &mut id_msg);
                    encode_bytes_field(1, &id_msg, &mut out);
                }
                Ok(any(
                    "eve_public.cosmetic.market.transaction.api.GetAllResponse",
                    out,
                ))
            }),
        );
    }
}
