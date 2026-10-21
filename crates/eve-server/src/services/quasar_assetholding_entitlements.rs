//! `quasar_assetholding_entitlements` — Quasar V24.01 Asset Holding, Entitlements & Notices (`Q-6`).
//!
//! Implements backend handlers for:
//! - `eve.assetholding.asset.api.*` (`GetRequest`, `HoldRequest`, `SpawnRequest`, `CreateRequest`);
//! - `eve.assetholding.entitlement.api.*` (`GetRequest`, `RedeemRequest`, `DirectDeliveryRequest`,
//!   `DeleteRequest`, `GetUnredeemedByHolderAndGroupNameRequest`);
//! - `eve.assetholding.entitlement.api.admin.*` (`RedeemRequest`, `ExpireRequest`, `AutoRedeemRequest`,
//!   `RedeliverStaleEntitlementsRequest`);
//! - `eve_public.assetholding.asset.api.*` (`GetRequest`, `FiniteCapacityClaimedNotice`, `DeletedNotice`);
//! - `eve_public.assetholding.entitlement.api.*` (`GetRequest`, `RedeemRequest`, `GrantedNotice`, `RedeemedNotice`);
//! - `eve_public.assetholding.entitlement.api.admin.*` (`ExpireRequest`, `AutoRedeemRequest`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
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
    _db: Arc<WorldRepository>,
    _sde: Arc<SdeRegistry>,
    _descriptors: Arc<QuasarDescriptorRegistry>,
) {
    // 1. eve.assetholding.asset.api.GetRequest -> eve.assetholding.asset.api.GetResponse
    router.register_handler(
        "eve.assetholding.asset.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let asset_id = extract_u64_from_field(dyn_msg, 1);
            let mut attrs = Vec::new();
            encode_string_field(1, &format!("Asset #{asset_id}"), &mut attrs);
            encode_string_field(2, "Promotional Asset Holding Grant", &mut attrs);
            encode_varint_field(7, 1, &mut attrs); // no_auto_release

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
    router.register_handler(
        "eve_public.assetholding.asset.api.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let attrs = Vec::new();
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

    // 3. eve.assetholding.asset.api.HoldRequest
    router.register_handler(
        "eve.assetholding.asset.api.HoldRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.HoldResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 4. eve.assetholding.asset.api.SpawnRequest
    router.register_handler(
        "eve.assetholding.asset.api.SpawnRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.SpawnResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 5. eve.assetholding.asset.api.CreateRequest
    router.register_handler(
        "eve.assetholding.asset.api.CreateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.asset.api.CreateResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 6. eve.assetholding.entitlement.api.GetRequest -> eve.assetholding.entitlement.api.GetResponse
    router.register_handler(
        "eve.assetholding.entitlement.api.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut attrs = Vec::new();
            encode_varint_field(5, 1, &mut attrs); // no_last_modified_timestamp

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve_public.assetholding.entitlement.api.GetRequest -> eve_public.assetholding.entitlement.api.GetResponse
    router.register_handler(
        "eve_public.assetholding.entitlement.api.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let attrs = Vec::new();
            let mut out = Vec::new();
            encode_bytes_field(2, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 8. eve.assetholding.entitlement.api.RedeemRequest
    router.register_handler(
        "eve.assetholding.entitlement.api.RedeemRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.RedeemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 9. eve_public.assetholding.entitlement.api.RedeemRequest
    router.register_handler(
        "eve_public.assetholding.entitlement.api.RedeemRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.RedeemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 10. eve.assetholding.entitlement.api.DirectDeliveryRequest
    router.register_handler(
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.DirectDeliveryResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 11. eve.assetholding.entitlement.api.DeleteRequest
    router.register_handler(
        "eve.assetholding.entitlement.api.DeleteRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.DeleteResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 12. eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest
    router.register_handler(
        "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut ent = Vec::new();
            encode_identifier(1, 1001, &mut ent);
            encode_bytes_field(1, &ent, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameResponse"
                ),
                value: out,
            })
        }),
    );

    // 13. Admin handlers
    router.register_handler(
        "eve.assetholding.entitlement.api.admin.RedeemRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.admin.RedeemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve.assetholding.entitlement.api.admin.ExpireRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.admin.ExpireResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve.assetholding.entitlement.api.admin.AutoRedeemRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.admin.AutoRedeemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve_public.assetholding.entitlement.api.admin.ExpireRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.admin.ExpireResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.admin.AutoRedeemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
}

/// Helper for broadcasting `GrantedNotice` (`eve_public.assetholding.entitlement.api.GrantedNotice`).
pub fn publish_entitlement_granted_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    entitlement_id: u64,
    quantity: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_identifier(1, entitlement_id, &mut payload);
    encode_varint_field(2, quantity as u64, &mut payload);

    let any = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.GrantedNotice"
        ),
        value: payload,
    };
    let (_, delivered) =
        gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix)?;
    Ok(delivered)
}

/// Helper for broadcasting `RedeemedNotice` (`eve_public.assetholding.entitlement.api.RedeemedNotice`).
pub fn publish_entitlement_redeemed_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    entitlement_id: u64,
    quantity: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_identifier(1, entitlement_id, &mut payload);
    encode_varint_field(2, quantity as u64, &mut payload);

    let any = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.RedeemedNotice"
        ),
        value: payload,
    };
    let (_, delivered) =
        gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix)?;
    Ok(delivered)
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
