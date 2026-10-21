//! `quasar_payment_store` — Quasar Payment, PLEX, Store & Fulfillment Services (`Q-3`).

use super::quasar_auth_user::get_identity_store;
use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let store = get_identity_store();

    // 1. eve_public.plex.vault.api.BalanceRequest -> eve_public.plex.vault.api.BalanceResponse
    let store_plex_bal = Arc::clone(&store);
    router.register_handler(
        "eve_public.plex.vault.api.BalanceRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let bal = store_plex_bal
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .plex_balances
                .get(&uid)
                .copied()
                .unwrap_or(500);

            let mut curr = Vec::new();
            encode_varint_field(1, (bal * 100) as u64, &mut curr); // field 1: total_in_cents

            let mut out = Vec::new();
            encode_bytes_field(1, &curr, &mut out); // field 1: balance
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceResponse"
                ),
                value: out,
            })
        }),
    );

    // 2. eve.plex.vault.assetholding.operation.api.DepositRequest -> updates balance and enqueues BalanceChangedNotice
    let store_plex_dep = Arc::clone(&store);
    let router_pending_dep = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let deposit_amount = if let Some(DynamicValue::Message(c)) = dyn_msg.get(3) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt / 100) as i64
                } else {
                    100
                }
            } else if let Some(DynamicValue::Message(c)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else {
                    100
                }
            } else {
                100
            };

            let new_bal = {
                let mut guard = store_plex_dep.write().unwrap_or_else(|e| e.into_inner());
                let entry = guard.plex_balances.entry(uid).or_insert(500);
                *entry += deposit_amount;
                *entry
            };

            let mut notice_bytes = Vec::new();
            let mut curr = Vec::new();
            encode_varint_field(1, (new_bal * 100) as u64, &mut curr);
            encode_bytes_field(3, &curr, &mut notice_bytes); // resulting_balance

            router_pending_dep
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"
                        ),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.DepositResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 3. eve.plex.vault.assetholding.operation.api.WithdrawRequest -> updates balance and enqueues BalanceChangedNotice
    let store_plex_with = Arc::clone(&store);
    let router_pending_with = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let withdraw_amount = if let Some(DynamicValue::Message(c)) = dyn_msg.get(3) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt / 100) as i64
                } else {
                    50
                }
            } else if let Some(DynamicValue::Message(c)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else {
                    50
                }
            } else {
                50
            };

            let new_bal = {
                let mut guard = store_plex_with.write().unwrap_or_else(|e| e.into_inner());
                let entry = guard.plex_balances.entry(uid).or_insert(500);
                *entry = (*entry - withdraw_amount).max(0);
                *entry
            };

            let mut notice_bytes = Vec::new();
            let mut curr = Vec::new();
            encode_varint_field(1, (new_bal * 100) as u64, &mut curr);
            encode_bytes_field(3, &curr, &mut notice_bytes);

            router_pending_with
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"
                        ),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.WithdrawResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 4. eve_public.store.category.api.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve_public.store.category.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for cat_id in 1..=4 {
                let mut cat_bytes = Vec::new();
                encode_varint_field(1, cat_id, &mut cat_bytes);
                encode_bytes_field(1, &cat_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve_public.store.product.api.GetRequest -> GetResponse
    router.register_handler(
        "eve_public.store.product.api.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut prod = Vec::new();
            encode_varint_field(1, 44992, &mut prod); // PLEX type_id
            encode_string_field(2, "500 PLEX Pack", &mut prod);
            let mut out = Vec::new();
            encode_bytes_field(1, &prod, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.product.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 6. eve_public.store.offer.api.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve_public.store.offer.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for offer_id in 1001..=1005 {
                let mut off_bytes = Vec::new();
                encode_varint_field(1, offer_id, &mut off_bytes);
                encode_bytes_field(1, &off_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.offer.api.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve_public.store.offer.api.GetAllTargetedRequest -> GetAllTargetedResponse
    router.register_handler(
        "eve_public.store.offer.api.GetAllTargetedRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut off_bytes = Vec::new();
            encode_varint_field(1, 2001, &mut off_bytes);
            encode_bytes_field(1, &off_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.offer.api.GetAllTargetedResponse"
                ),
                value: out,
            })
        }),
    );

    // 8. eve_public.store.targetaudience.offer.api.ListRequest -> ListResponse
    router.register_handler(
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut entry = Vec::new();
            encode_varint_field(1, 3001, &mut entry);
            encode_bytes_field(1, &entry, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.targetaudience.offer.api.ListResponse"
                ),
                value: out,
            })
        }),
    );

    // 9. eve_public.payment.code.api.RedeemRequest -> RedeemResponse
    let store_redeem = Arc::clone(&store);
    router.register_handler(
        "eve_public.payment.code.api.RedeemRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let code = if let Some(DynamicValue::Message(m)) = dyn_msg.get(1) {
                if let Some(DynamicValue::String(s)) = m.get(1) {
                    s.clone()
                } else {
                    "PROMO-V24-START".to_string()
                }
            } else if let Some(DynamicValue::String(s)) = dyn_msg.get(1) {
                s.clone()
            } else {
                "PROMO-V24-START".to_string()
            };
            store_redeem
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .redeemed_codes
                .insert(code);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.code.api.RedeemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 10. eve_public.payment.schedule.api.GetRequest -> enqueues SetNotice
    let router_pending_sched = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.schedule.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1);
            let mut notice_bytes = Vec::new();
            encode_varint_field(1, 101, &mut notice_bytes);
            router_pending_sched
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.SetNotice"
                        ),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.GetResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 11. eve_public.payment.token.api.GetQuickPayRequest -> enqueues QuickPayAddedNotice
    let router_pending_tok = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.token.api.GetQuickPayRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1);
            let mut notice_bytes = Vec::new();
            encode_string_field(1, "tok_quickpay_1", &mut notice_bytes);
            router_pending_tok
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.QuickPayAddedNotice"
                        ),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.GetQuickPayResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 12. eve.fulfillment.api.DeliverItemRequest -> DeliverItemResponse
    let db_fulfill = Arc::clone(&db);
    router.register_handler(
        "eve.fulfillment.api.DeliverItemRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let type_id = if let Some(DynamicValue::Message(item_type)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int32(t)) = item_type.get(1) {
                    *t as u32
                } else {
                    44992 // PLEX
                }
            } else {
                44992
            };
            let qty = if let Some(DynamicValue::Int32(q)) = dyn_msg.get(3) {
                *q
            } else {
                1
            };
            let target_cid = ctx
                .effective_character_id()
                .map(|cid| cid as i64)
                .unwrap_or_else(|| {
                    let chars = db_fulfill.get_characters_for_user(uid);
                    chars.first().map(|c| c.character_id).unwrap_or(1001)
                });
            let _ = db_fulfill.add_or_stack_item(
                target_cid,
                60003760, // station
                eve_db::InventoryFlag::Hangar,
                type_id,
                qty as i64,
            );

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.fulfillment.api.DeliverItemResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 13. eve.isk.wallet.api.TransferByOwnerRequest -> TransferByOwnerResponse
    router.register_handler(
        "eve.isk.wallet.api.TransferByOwnerRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.isk.wallet.api.TransferByOwnerResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
}
