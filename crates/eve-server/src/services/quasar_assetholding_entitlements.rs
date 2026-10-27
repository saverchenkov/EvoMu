//! `quasar_assetholding_entitlements` — Quasar V24.01 Asset Holding, Entitlements & Notices (`Q-6`).
//!
//! Implements backend handlers for:
//! - `eve.assetholding.asset.api.*` (`GetRequest`, `HoldRequest`, `SpawnRequest`, `CreateRequest`);
//! - `eve.assetholding.entitlement.api.*` (`GetRequest`, `RedeemRequest`, `DirectDeliveryRequest`,
//!   `DeleteRequest`, `GetUnredeemedByHolderAndGroupNameRequest`);
//! - `eve.assetholding.entitlement.api.admin.*` (`RedeemRequest`, `ExpireRequest`, `AutoRedeemRequest`,
//!   `RedeliverStaleEntitlementsRequest`);
//! - `eve_public.assetholding.asset.api.*` (`GetRequest`, `FiniteCapacityClaimedNotice`, `DeletedNotice`);
//! - `eve_public.assetholding.entitlement.api.*` (`GetRequest`, `RedeemRequest`, `GrantedNotice`, `RedeemedNotice`)
//!   and `SpawnRequest` / `DirectDeliveryRequest` are ledger-backed in `quasar_entitlement_ledger`;
//! - `eve_public.assetholding.entitlement.api.admin.*` (`ExpireRequest`, `AutoRedeemRequest`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn encode_identifier(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

fn extract_u64_from_field(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match dyn_msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Message(inner)) => extract_u64_from_field(inner, 1),
            Some(DynamicValue::Uint64(n)) => *n,
            Some(DynamicValue::Uint32(n)) => *n as u64,
            Some(DynamicValue::Int64(n)) => *n as u64,
            Some(DynamicValue::Int32(n)) => *n as u64,
            _ => 0,
        },
        Some(DynamicValue::Uint64(n)) => *n,
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Int64(n)) => *n as u64,
        Some(DynamicValue::Int32(n)) => *n as u64,
        _ => 0,
    }
}

pub fn register_assetholding_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
    _descriptors: Arc<QuasarDescriptorRegistry>,
) {
    // 1. eve.assetholding.asset.api.GetRequest -> eve.assetholding.asset.api.GetResponse
    let db_asset1 = Arc::clone(&db);
    router.register_handler(
        "eve.assetholding.asset.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let uuid_bytes = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Bytes(b)) if !b.is_empty() => Some(b.clone()),
                    _ => None,
                },
                _ => None,
            }
            .ok_or_else(|| {
                (
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "Missing asset Identifier.uuid".to_string(),
                )
            })?;
            let ledger = super::quasar_entitlement_ledger::load(&db_asset1);
            let asset = ledger
                .assets
                .get(&super::quasar_entitlement_ledger::hex(&uuid_bytes))
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_NOT_FOUND,
                        "Asset not found".to_string(),
                    )
                })?;
            let attrs = super::quasar_entitlement_ledger::encode_asset_attributes(asset, false);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 2. eve_public.assetholding.asset.api.GetRequest -> eve_public.assetholding.asset.api.GetResponse
    let db_asset2 = Arc::clone(&db);
    router.register_handler(
        "eve_public.assetholding.asset.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let uuid_bytes = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Bytes(b)) if !b.is_empty() => Some(b.clone()),
                    _ => None,
                },
                _ => None,
            }
            .ok_or_else(|| {
                (
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "Missing asset Identifier.uuid".to_string(),
                )
            })?;
            let ledger = super::quasar_entitlement_ledger::load(&db_asset2);
            let asset = ledger
                .assets
                .get(&super::quasar_entitlement_ledger::hex(&uuid_bytes))
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_NOT_FOUND,
                        "Asset not found".to_string(),
                    )
                })?;
            let attrs = super::quasar_entitlement_ledger::encode_asset_attributes(asset, true);
            let mut out = Vec::new();
            encode_bytes_field(2, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.asset.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 12. eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest
    let db_unred = Arc::clone(&db);
    router.register_handler(
        "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let holder_raw = extract_u64_from_field(dyn_msg, 1);
            let req_holder = u32::try_from(holder_raw).map_err(|_| {
                (
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "Invalid holder ID".to_string(),
                )
            })?;
            let is_admin = crate::quasar_gateway::require_admin_role(ctx).is_ok();
            let caller_cid = ctx.effective_character_id();
            let caller_uid = ctx.effective_user_id().and_then(|u| u32::try_from(u).ok());
            if caller_cid.is_none() && caller_uid.is_none() && !is_admin {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required".to_string(),
                ));
            }
            let eff_holder = if req_holder > 0 {
                if !is_admin && caller_cid != Some(req_holder) && caller_uid != Some(req_holder) {
                    return Err((
                        crate::quasar_gateway::STATUS_FORBIDDEN,
                        "Cannot query another holder's entitlements".to_string(),
                    ));
                }
                req_holder
            } else {
                caller_cid.or(caller_uid).ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_BAD_REQUEST,
                        "Holder ID required".to_string(),
                    )
                })?
            };
            let group_filter = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => Some(s.clone()),
                _ => None,
            };
            let ledger = super::quasar_entitlement_ledger::load(&db_unred);
            let mut out = Vec::new();
            for e in ledger.entitlements.values() {
                if e.total > e.redeemed
                    && e.holder == eff_holder
                    && group_filter.as_ref().is_none_or(|g| g.is_empty() || &e.group_name == g)
                {
                    let id_b = super::quasar_entitlement_ledger::public_identifier(e);
                    let mut qty = Vec::new();
                    encode_varint_field(1, e.total, &mut qty);
                    encode_varint_field(2, e.redeemed, &mut qty);
                    let mut entry = Vec::new();
                    encode_bytes_field(1, &id_b, &mut entry);
                    encode_bytes_field(2, &qty, &mut entry);
                    encode_bytes_field(1, &entry, &mut out);
                }
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameResponse"
                ),
                value: out,
            })
        }),
    );

    // Ledger-backed asset spawn, hold, create, entitlement delivery, lookup, redemption, and admin actions.
    super::quasar_entitlement_ledger::register(router, db, sde);
}

/// Helper for broadcasting `FiniteCapacityClaimedNotice`.
pub fn publish_asset_capacity_claimed_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    asset_id: u64,
    previous_remaining: u64,
    current_remaining: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_identifier(1, asset_id, &mut payload);
    encode_varint_field(2, previous_remaining, &mut payload);
    encode_varint_field(3, current_remaining, &mut payload);

    let any = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.asset.api.FiniteCapacityClaimedNotice"
        ),
        value: payload,
    };
    let (_, delivered) =
        gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix)?;
    Ok(delivered)
}

/// Helper for broadcasting `DeletedNotice`.
pub fn publish_asset_deleted_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    asset_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_identifier(1, asset_id, &mut payload);

    let any = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.asset.api.DeletedNotice"
        ),
        value: payload,
    };
    let (_, delivered) =
        gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix)?;
    Ok(delivered)
}
