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
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
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
    pb::message("eve_public.cosmetic.market.transaction.Attributes")
        .set("last_updated.seconds", tx.last_updated_unix as i64)
        .set("status", tx.status as i32)
        .set_with("details", |d| match tx.details {
            CosmeticTransactionDetails::PurchasingComponent => {
                d.set_with("purchasing_component", |b| b)
            }
            CosmeticTransactionDetails::PurchasingSkin => d.set_with("purchasing_skin", |b| b),
            CosmeticTransactionDetails::CreatingSkinListing => {
                d.set_with("creating_skin_listing", |b| b)
            }
        })
        .must_encode()
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
                Ok(pb::message(
                    "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedResponse",
                )
                .set_raw_message("draft", attrs)
                .must_to_any())
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
                Ok(pb::message("eve_public.cosmetic.market.transaction.api.GetResponse")
                    .set_raw_message("status", attrs)
                    .must_to_any())
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
                let mut resp = pb::message("eve_public.cosmetic.market.transaction.api.GetAllResponse");
                for u in uuids {
                    resp = resp.add_with("transactions", |t| t.set("uuid", u.to_vec()));
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    // 4. eve.cdn.api.GetActiveURLSigningKeyRequest & SignURLRequest (feat:quasar.cdn)
    {
        router.register_handler(
            "eve.cdn.api.GetActiveURLSigningKeyRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                Ok(pb::message("eve.cdn.api.GetActiveURLSigningKeyResponse")
                    .set("key_id", "evomu-cdn-v24")
                    .must_to_any())
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
                Ok(pb::message("eve.cdn.api.SignURLResponse")
                    .set("signed_url", signed)
                    .set("valid_until.seconds", 1_800_000_000i64)
                    .set("key_id", "evomu-cdn-v24")
                    .must_to_any())
            }),
        );
    }

    // 5. eve.client.host.GetRequest & DisconnectRequest (feat:quasar.client)
    {
        router.register_handler(
            "eve.client.host.GetRequest",
            Arc::new(move |_ctx, _req_any, _dyn_msg| {
                Ok(pb::message("eve.client.host.GetResponse")
                    .set_with("host", |h| {
                        h.set("legacy_hash", 12345678i64)
                            .set("locale", "en_US")
                            .set("ram_mb", 16384i32)
                            .set("client_version", "24.01.2024")
                    })
                    .must_to_any())
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
                let resp = db_contacts.with_snapshot(|snap| {
                    let mut r = pb::message("eve.contacts.GetAllForCharacterResponse");
                    if let Some(ch) = snap.characters.get(&char_id) {
                        for (&contact_id, &standing) in &ch.standings {
                            let rel_val = (standing + 10.0).round() as i32;
                            r = r.add_with("contacts", |personal| {
                                personal
                                    .set("individual.character.sequential", contact_id as u32)
                                    .set("relationship", rel_val)
                            });
                        }
                    }
                    r
                });
                Ok(resp.must_to_any())
            }),
        );
        router.register_handler(
            "eve.contact.label.GetForAllianceRequest",
            Arc::new(move |_ctx, _req_any, _dyn_msg| {
                Ok(pb::message("eve.contact.label.GetForAllianceResponse")
                    .add_with("labels", |inst| {
                        inst.set("id.bit", 1u64)
                            .set("label.name", "Alliance Contacts")
                    })
                    .must_to_any())
            }),
        );
    }

    // 7. eve.content.GetAllRequest & GetRequest (feat:quasar.content)
    {
        router.register_handler(
            "eve.content.GetAllRequest",
            Arc::new(move |_ctx, _req_any, _dyn_msg| {
                let mut resp = pb::message("eve.content.GetAllResponse");
                for id in 1..=3u32 {
                    resp = resp.add_with("id", |i| i.set("sequential", id));
                }
                Ok(resp.must_to_any())
            }),
        );
        router.register_handler(
            "eve.content.GetRequest",
            Arc::new(move |_ctx, req_any, _dyn_msg| {
                let cid = extract_uint64_field(&req_any.value, 1).unwrap_or(1);
                Ok(pb::message("eve.content.GetResponse")
                    .set_with("attributes", |attrs| {
                        attrs
                            .set("content.operation.sequential", 100u32)
                            .set_with("tags", |tags| {
                                tags.set("primary_tag", "news")
                                    .add("secondary_tags", "community")
                            })
                            .set("publish_state", 1i32)
                            .set("description", format!("Article #{cid}: News from New Eden"))
                    })
                    .set("user", "ccp-content")
                    .must_to_any())
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_market_contract_services_tests.rs"]
mod quasar_market_contract_services_tests;

