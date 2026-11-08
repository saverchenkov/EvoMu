//! Batch 22 Quasar protobuf service handlers:
//! - `eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest`
//! - `eve_public.cosmetic.ship.skin.thirdparty.market.transaction` (`GetRequest` / `GetAllRequest`)

#![forbid(unsafe_code)]

use super::quasar_cosmetics_drafts::{encode_draft_attributes, extract_id_from_uuid_bytes};
use super::quasar_cosmetics_market_types::{
    CosmeticTransactionDetails, CosmeticTransactionRecord, CosmeticsMarketData,
};
use super::quasar_cosmetics_services::QuasarCosmeticsState;
use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
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

fn hex_uuid(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn encode_transaction_attributes(tx: &CosmeticTransactionRecord) -> Vec<u8> {
    let mut ts = Vec::new();
    encode_varint_field(1, tx.last_updated_unix, &mut ts);

    let mut details = Vec::new();
    let detail_field = match tx.details {
        CosmeticTransactionDetails::PurchasingComponent => 1,
        CosmeticTransactionDetails::PurchasingSkin => 2,
        CosmeticTransactionDetails::CreatingSkinListing => 3,
    };
    encode_bytes_field(detail_field, &[], &mut details);

    let mut attrs = Vec::new();
    encode_bytes_field(1, &ts, &mut attrs);
    encode_varint_field(2, u64::from(tx.status), &mut attrs);
    encode_bytes_field(3, &details, &mut attrs);
    attrs
}

fn extract_uint64_field(raw_bytes: &[u8], field_num: u32) -> Option<u64> {
    if let Ok(wm) = WireMessage::decode(raw_bytes) {
        if let Some(f) = wm.fields.iter().find(|f| f.field_number == field_num) {
            if let Some(v) = f.value.as_varint() {
                return Some(v);
            }
            if let Some(b) = f.value.as_bytes() {
                if let Ok(sub) = WireMessage::decode(b) {
                    if let Some(sf) = sub.fields.iter().find(|f| f.field_number == 1) {
                        if let Some(v) = sf.value.as_varint() {
                            return Some(v);
                        }
                    }
                }
            }
        }
    }
    None
}

fn extract_uint32_field(_dyn_msg: &DynamicQuasarMessage, raw_bytes: &[u8], field_num: u32) -> Option<u32> {
    extract_uint64_field(raw_bytes, field_num).and_then(|v| u32::try_from(v).ok())
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
                let caller = require_character(ctx)?;
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
                drop(data);
                let _ = caller;
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
                let caller = require_character(ctx)?;
                let uuid = extract_uuid_field(dyn_msg, &req_any.value, 1)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing transaction UUID".to_string()))?;
                if uuid.len() != 16 {
                    return Err((STATUS_BAD_REQUEST, "Transaction UUID must be 16 bytes".to_string()));
                }
                let tx_key = hex_uuid(&uuid);
                let tx_opt = db_tx.with_snapshot(|snap| {
                    let market: CosmeticsMarketData = snap
                        .quasar_state_blobs
                        .get("quasar_cosmetics_market")
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    market.transactions.get(&tx_key).cloned()
                });
                let Some(tx) = tx_opt else {
                    return Err((STATUS_NOT_FOUND, "Cosmetic market transaction not found".to_string()));
                };
                let is_participant = tx.buyer_id == Some(caller) || tx.seller_id == Some(caller);
                if !is_participant && require_admin_role(ctx).is_err() {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Transaction belongs to another character".to_string(),
                    ));
                }
                let attrs = encode_transaction_attributes(&tx);
                let mut out = Vec::new();
                encode_bytes_field(1, &attrs, &mut out);
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
                    market
                        .transactions
                        .values()
                        .filter(|tx| tx.buyer_id == Some(caller) || tx.seller_id == Some(caller))
                        .map(|tx| tx.uuid)
                        .collect::<Vec<[u8; 16]>>()
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

    // 4. eve.cdn.api.GetActiveURLSigningKeyRequest & SignURLRequest (feat:quasar.cdn)
    {
        router.register_handler(
            "eve.cdn.api.GetActiveURLSigningKeyRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut out = Vec::new();
                encode_string_field(1, "evomu-cdn-v24", &mut out);
                Ok(any("eve.cdn.api.GetActiveURLSigningKeyResponse", out))
            }),
        );
        router.register_handler(
            "eve.cdn.api.SignURLRequest",
            Arc::new(move |_ctx, req_any, dyn_msg| {
                let url = match dyn_msg.get(1) {
                    Some(DynamicValue::String(s)) => s.clone(),
                    _ => {
                        WireMessage::decode(&req_any.value)
                            .ok()
                            .and_then(|wm| {
                                wm.fields
                                    .iter()
                                    .find(|f| f.field_number == 1)
                                    .and_then(|f| f.value.as_str().ok().flatten().map(|s| s.to_string()))
                            })
                            .unwrap_or_else(|| "https://cdn.evomu.org/res/asset.fb".to_string())
                    }
                };
                let signed = if url.contains('?') {
                    format!("{url}&sig=evomu_v24_signed")
                } else {
                    format!("{url}?sig=evomu_v24_signed")
                };
                let mut out = Vec::new();
                encode_string_field(1, &signed, &mut out);
                let mut ts = Vec::new();
                encode_varint_field(1, 1_800_000_000, &mut ts);
                encode_bytes_field(2, &ts, &mut out);
                encode_string_field(3, "evomu-cdn-v24", &mut out);
                Ok(any("eve.cdn.api.SignURLResponse", out))
            }),
        );
    }

    // 5. eve.client.host.GetRequest & DisconnectRequest (feat:quasar.client)
    {
        router.register_handler(
            "eve.client.host.GetRequest",
            Arc::new(move |_ctx, _req_any, _dyn_msg| {
                let mut host_attr = Vec::new();
                encode_varint_field(1, 12345678, &mut host_attr);
                encode_string_field(2, "en_US", &mut host_attr);
                encode_varint_field(4, 16384, &mut host_attr);
                encode_string_field(10, "24.01.2024", &mut host_attr);
                let mut out = Vec::new();
                encode_bytes_field(1, &host_attr, &mut out);
                Ok(any("eve.client.host.GetResponse", out))
            }),
        );
        router.register_handler(
            "DisconnectRequest",
            Arc::new(move |_ctx, _any, _dyn| {
                Ok(any("google.protobuf.Empty", Vec::new()))
            }),
        );
    }

    // 6. eve.contacts.GetAllForCharacterRequest & label.GetForAllianceRequest (feat:quasar.contact)
    {
        let db_contacts = Arc::clone(&db);
        router.register_handler(
            "eve.contacts.GetAllForCharacterRequest",
            Arc::new(move |ctx, _req_any, _dyn_msg| {
                let Some(char_id) = ctx.effective_character_id().filter(|&v| v > 0).map(i64::from) else {
                    return Err((STATUS_UNAUTHORIZED, "Active character session required".into()));
                };
                let mut out = Vec::new();
                db_contacts.with_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get(&char_id) {
                        for (&contact_id, &standing) in &ch.standings {
                            let mut personal = Vec::new();
                            let mut char_id_msg = Vec::new();
                            encode_varint_field(1, contact_id as u64, &mut char_id_msg);
                            let mut ind_msg = Vec::new();
                            encode_bytes_field(1, &char_id_msg, &mut ind_msg);
                            encode_bytes_field(1, &ind_msg, &mut personal);
                            let rel_val = (standing + 10.0).round() as u64;
                            encode_varint_field(4, rel_val, &mut personal);
                            encode_bytes_field(1, &personal, &mut out);
                        }
                    }
                });
                Ok(any("eve.contacts.GetAllForCharacterResponse", out))
            }),
        );
        router.register_handler(
            "eve.contact.label.GetForAllianceRequest",
            Arc::new(move |_ctx, _req_any, _dyn_msg| {
                let mut out = Vec::new();
                let mut inst = Vec::new();
                let mut id_msg = Vec::new();
                encode_varint_field(1, 1, &mut id_msg);
                encode_bytes_field(1, &id_msg, &mut inst);
                let mut attr = Vec::new();
                encode_string_field(1, "Alliance Contacts", &mut attr);
                encode_bytes_field(2, &attr, &mut inst);
                encode_bytes_field(1, &inst, &mut out);
                Ok(any("eve.contact.label.GetForAllianceResponse", out))
            }),
        );
    }

    // 7. eve.content.GetAllRequest & GetRequest (feat:quasar.content)
    {
        router.register_handler(
            "eve.content.GetAllRequest",
            Arc::new(move |_ctx, _req_any, _dyn_msg| {
                let mut out = Vec::new();
                for id in 1..=3u64 {
                    let mut id_msg = Vec::new();
                    encode_varint_field(1, id, &mut id_msg);
                    encode_bytes_field(1, &id_msg, &mut out);
                }
                Ok(any("eve.content.GetAllResponse", out))
            }),
        );
        router.register_handler(
            "eve.content.GetRequest",
            Arc::new(move |_ctx, req_any, _dyn_msg| {
                let cid = extract_uint64_field(&req_any.value, 1).unwrap_or(1);
                let mut attrs = Vec::new();
                let mut details = Vec::new();
                let mut op_id = Vec::new();
                encode_varint_field(1, 100, &mut op_id);
                encode_bytes_field(1, &op_id, &mut details);
                encode_bytes_field(1, &details, &mut attrs);
                let mut tags = Vec::new();
                encode_string_field(1, "news", &mut tags);
                encode_string_field(2, "community", &mut tags);
                encode_bytes_field(2, &tags, &mut attrs);
                encode_varint_field(3, 1, &mut attrs);
                encode_string_field(4, &format!("Article #{cid}: News from New Eden"), &mut attrs);

                let mut out = Vec::new();
                encode_bytes_field(1, &attrs, &mut out);
                encode_string_field(2, "ccp-content", &mut out);
                Ok(any("eve.content.GetResponse", out))
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_market_contract_services_tests.rs"]
mod quasar_market_contract_services_tests;

