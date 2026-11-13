//! `quasar_payment_store` — Quasar Payment, PLEX, Store & Fulfillment Services (`Q-3`).

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::pb::{self, enum_name};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::services::quasar_payment_store_types::*;

fn parse_raw_varints(raw: &[u8]) -> HashMap<u32, u64> {
    let mut map = HashMap::new();
    let mut idx = 0;
    while idx < raw.len() {
        let tag = raw[idx];
        idx += 1;
        let wire_type = tag & 0x07;
        let field_num = (tag >> 3) as u32;
        if wire_type == 0 {
            let mut val = 0u64;
            let mut shift = 0;
            while idx < raw.len() {
                let b = raw[idx];
                idx += 1;
                val |= ((b & 0x7F) as u64) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            map.entry(field_num).or_insert(val);
        } else if wire_type == 2 {
            let mut len = 0usize;
            let mut shift = 0;
            while idx < raw.len() {
                let b = raw[idx];
                idx += 1;
                len |= ((b & 0x7F) as usize) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            if idx + len <= raw.len() {
                let sub = parse_raw_varints(&raw[idx..idx + len]);
                for (k, v) in sub {
                    map.entry(field_num).or_insert(v);
                    map.entry(k).or_insert(v);
                }
            }
            idx += len;
        } else {
            break;
        }
    }
    map
}

fn extract_u64_id(raw_bytes: &[u8], dyn_msg: &DynamicQuasarMessage) -> Option<u64> {
    if let Some(id) = dyn_msg.get(1).and_then(|v| match v {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Int64(i) => Some(*i as u64),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int32(i) => Some(*i as u64),
        DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
            DynamicValue::Uint64(u) => Some(*u),
            DynamicValue::Int64(i) => Some(*i as u64),
            DynamicValue::Uint32(u) => Some(*u as u64),
            DynamicValue::Int32(i) => Some(*i as u64),
            _ => None,
        }),
        _ => None,
    }) {
        if id > 0 {
            return Some(id);
        }
    }
    parse_raw_varints(raw_bytes).get(&1).copied().filter(|id| *id > 0)
}

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    db.with_mut_snapshot(|snap| ensure_store_catalog_seeded(snap, &router.sde));
    // 1. eve_public.plex.vault.api.BalanceRequest -> eve_public.plex.vault.api.BalanceResponse
    let db_plex_bal = Arc::clone(&db);
    router.register_handler(
        "eve_public.plex.vault.api.BalanceRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx
                .effective_user_id()
                .filter(|&u| u > 0)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?
                as u64;
            let bal = db_plex_bal.with_snapshot(|snap| snap.unified_plex_balance_for_user(uid));

            Ok(pb::message("eve_public.plex.vault.api.BalanceResponse")
                .set("balance.total_in_cents", bal * 100)
                .must_to_any())
        }),
    );

    // 2. eve.plex.vault.assetholding.operation.api.DepositRequest -> updates balance and enqueues BalanceChangedNotice
    let db_plex_dep = Arc::clone(&db);
    let router_pending_dep = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let deposit_amount = if let Some(DynamicValue::Message(c)) = dyn_msg.get(3) {
                if let Some(DynamicValue::Int64(amt)) | Some(DynamicValue::Sint64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt as i64) / 100
                } else {
                    100
                }
            } else if let Some(DynamicValue::Message(c)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int64(amt)) | Some(DynamicValue::Sint64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt as i64) / 100
                } else {
                    100
                }
            } else {
                100
            };

            if deposit_amount <= 0 {
                return Err((
                    STATUS_BAD_REQUEST,
                    "Deposit amount must be positive".to_string(),
                ));
            }

            let now_unix = ctx.now_unix();
            let (new_bal, tx_id) = db_plex_dep
                .try_with_mut_snapshot(|snap| -> Result<(i64, u64), ()> {
                    let bal = snap.adjust_unified_plex_for_user(uid, deposit_amount).ok_or(())?;
                    snap.next_entity_id += 1;
                    let tx_id = snap.next_entity_id as u64;
                    record_plex_transaction(snap, tx_id, uid, deposit_amount, bal, "GM Deposit", now_unix);
                    Ok((bal, tx_id))
                })
                .map_err(|_| (STATUS_BAD_REQUEST, "PLEX balance overflow".to_string()))?;

            emit_balance_changed_notice(
                &router_pending_dep,
                uid,
                tx_id,
                deposit_amount,
                new_bal,
                "GM Deposit",
                now_unix,
            );

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.DepositResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 3. eve.plex.vault.assetholding.operation.api.WithdrawRequest -> updates balance and enqueues BalanceChangedNotice
    let db_plex_with = Arc::clone(&db);
    let router_pending_with = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let withdraw_amount = if let Some(DynamicValue::Message(c)) = dyn_msg.get(3) {
                if let Some(DynamicValue::Int64(amt)) | Some(DynamicValue::Sint64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt as i64) / 100
                } else {
                    50
                }
            } else if let Some(DynamicValue::Message(c)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int64(amt)) | Some(DynamicValue::Sint64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt as i64) / 100
                } else {
                    50
                }
            } else {
                50
            };

            if withdraw_amount <= 0 {
                return Err((
                    STATUS_BAD_REQUEST,
                    "Withdraw amount must be positive".to_string(),
                ));
            }

            let now_unix = ctx.now_unix();
            let (new_bal, tx_id) = db_plex_with
                .try_with_mut_snapshot(|snap| -> Result<(i64, u64), ()> {
                    let bal = snap.adjust_unified_plex_for_user(uid, -withdraw_amount).ok_or(())?;
                    snap.next_entity_id += 1;
                    let tx_id = snap.next_entity_id as u64;
                    record_plex_transaction(
                        snap,
                        tx_id,
                        uid,
                        -withdraw_amount,
                        bal,
                        "GM Withdrawal",
                        now_unix,
                    );
                    Ok((bal, tx_id))
                })
                .map_err(|_| (STATUS_BAD_REQUEST, "Insufficient PLEX balance".to_string()))?;

            emit_balance_changed_notice(
                &router_pending_with,
                uid,
                tx_id,
                -withdraw_amount,
                new_bal,
                "GM Withdrawal",
                now_unix,
            );

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.WithdrawResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 4. eve_public.store.category.api.GetAllRequest -> GetAllResponse
    let db_cat_all = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.category.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let cat = db_cat_all.with_snapshot(read_store_catalog);
            let mut msg = pb::message("eve_public.store.category.api.GetAllResponse");
            for c in &cat.categories {
                msg = msg.add_with("categories", |cat_id| {
                    cat_id
                        .set("sequential", c.id)
                        .set("catalog", enum_name("CATALOG_NES"))
                });
            }
            Ok(msg.must_to_any())
        }),
    );

    // 4b. eve_public.store.category.api.GetRequest -> GetResponse
    let db_cat_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.category.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cat_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid category ID".to_string()))?;
            let cat = db_cat_get.with_snapshot(read_store_catalog);
            let category = cat
                .categories
                .iter()
                .find(|c| c.id == cat_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Category not found".to_string()))?;

            Ok(pb::message("eve_public.store.category.api.GetResponse")
                .set("attributes.name", &category.name)
                .set("attributes.no_parent", true)
                .set("attributes.no_subcategories", true)
                .must_to_any())
        }),
    );

    // 5. eve_public.store.product.api.GetRequest -> GetResponse
    let db_prod_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.product.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let prod_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid product ID".to_string()))?;

            let cat = db_prod_get.with_snapshot(read_store_catalog);
            let product = cat
                .products
                .iter()
                .find(|p| p.id == prod_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Product not found".to_string()))?;

            Ok(pb::message("eve_public.store.product.api.GetResponse")
                .set("attributes.name", &product.name)
                .set("attributes.item.type.sequential", u64::from(product.type_id))
                .set("attributes.item.soulbound", false)
                .must_to_any())
        }),
    );

    // 6. eve_public.store.offer.api.GetAllRequest -> GetAllResponse
    let db_off_all = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.offer.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let cat = db_off_all.with_snapshot(read_store_catalog);
            let mut msg = pb::message("eve_public.store.offer.api.GetAllResponse");
            for off in cat.offers.iter().filter(|o| !o.targeted) {
                msg = msg.add_with("offers", |o| o.set("store_offer", off.id.to_string()));
            }
            Ok(msg.must_to_any())
        }),
    );

    // 6b. eve_public.store.offer.api.GetRequest -> GetResponse
    let db_off_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.offer.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let offer_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::String(s) => s.parse::<u32>().ok(),
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::String(s) => s.parse::<u32>().ok(),
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid offer ID".to_string()))?;

            let (cat, offer_tags) = db_off_get.with_snapshot(|s| (read_store_catalog(s), read_store_offer_tags(s)));
            let offer = cat
                .offers
                .iter()
                .find(|o| o.id == offer_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Offer not found".to_string()))?;

            let mut msg = pb::message("eve_public.store.offer.api.GetResponse")
                .set("attributes.name", &offer.name)
                .set("attributes.no_enddate", true)
                .set("attributes.can_purchase", true)
                .set("attributes.no_schedule", true)
                .set(
                    "attributes.offer_price.plex_price.total_in_cents",
                    offer.price_plex * 100,
                )
                .add_with("attributes.categories", |c| c.set("sequential", offer.category_id));
            if let Some(tags) = offer_tags.get(&offer.id) {
                for t in tags {
                    msg = msg.add("attributes.tags", t);
                }
            }
            Ok(msg.must_to_any())
        }),
    );

    // 7. eve_public.store.offer.api.GetAllTargetedRequest -> GetAllTargetedResponse
    let db_off_tgt = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.offer.api.GetAllTargetedRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let cat = db_off_tgt.with_snapshot(read_store_catalog);
            let mut msg = pb::message("eve_public.store.offer.api.GetAllTargetedResponse");
            for off in cat.offers.iter().filter(|o| o.targeted) {
                msg = msg.add_with("offers", |o| o.set("store_offer", off.id.to_string()));
            }
            Ok(msg.must_to_any())
        }),
    );

    // 8. eve_public.store.targetaudience.offer.api.ListRequest -> ListResponse
    let db_ta_list = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let cat = db_ta_list.with_snapshot(read_store_catalog);
            let out = encode_targeted_offer_list_response(&cat.offers, uid, dyn_msg);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.targetaudience.offer.api.ListResponse"
                ),
                value: out,
            })
        }),
    );

    // 8b. eve_public.store.targetaudience.offer.api.GetRequest -> GetResponse
    let db_ta_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.targetaudience.offer.api.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let offer_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::String(s) => s.parse::<u32>().ok(),
                    DynamicValue::Message(m) => m
                        .get(1)
                        .and_then(|val| match val {
                            DynamicValue::Uint32(u) => Some(*u),
                            DynamicValue::Int32(i) => Some(*i as u32),
                            DynamicValue::String(s) => s.parse::<u32>().ok(),
                            _ => None,
                        })
                        .or_else(|| {
                            m.get(2).and_then(|val| match val {
                                DynamicValue::Message(entry) => entry.get(1).and_then(|s| match s {
                                    DynamicValue::String(str_val) => str_val.parse::<u32>().ok(),
                                    _ => None,
                                }),
                                _ => None,
                            })
                        }),
                    _ => None,
                })
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid targeted offer ID".to_string()))?;

            let cat = db_ta_get.with_snapshot(read_store_catalog);
            let offer = cat
                .offers
                .iter()
                .find(|o| o.id == offer_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Targeted offer not found".to_string()))?;

            Ok(
                pb::message("eve_public.store.targetaudience.offer.api.GetResponse")
                    .add_with("offer.title.entries", |e| {
                        e.set("id", enum_name("EN")).set("text", &offer.name)
                    })
                    .add_with("offer.description.entries", |e| {
                        e.set("id", enum_name("EN")).set("text", &offer.description)
                    })
                    .must_to_any(),
            )
        }),
    );

    // 9. eve_public.payment.code.api.RedeemRequest -> RedeemResponse
    router.register_handler(
        "eve_public.payment.code.api.RedeemRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Err((
                crate::quasar_gateway::STATUS_NOT_IMPLEMENTED,
                "RedeemRequest is not implemented".to_string(),
            ))
        }),
    );

    // 10. eve_public.payment.schedule.api.GetRequest -> GetResponse
    let db_sched_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.schedule.api.GetRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let sched_id = extract_u64_id(&any.value, dyn_msg).filter(|id| *id > 0).ok_or_else(|| {
                (STATUS_BAD_REQUEST, "Missing schedule ID".to_string())
            })?;

            let sched_opt: Option<PaymentScheduleRecord> = db_sched_get.with_snapshot(|snap| {
                snap.quasar_commerce
                    .payment_schedules
                    .iter()
                    .find(|s| s.id == sched_id && s.user_id == uid)
                    .cloned()
            });
            let sched = sched_opt.ok_or_else(|| {
                (STATUS_NOT_FOUND, "Payment schedule not found".to_string())
            })?;

            Ok(pb::message("eve_public.payment.schedule.api.GetResponse")
                .set("attributes.type", sched.schedule_type as i32)
                .set(
                    "attributes.next_payment",
                    GoogleTimestamp::from_unix(sched.next_payment_unix as i64, 0),
                )
                .set("attributes.offer.store_offer", &sched.offer_id)
                .set("attributes.recurring_interval_days", sched.interval_days)
                .must_to_any())
        }),
    );

    // 10b. eve_public.payment.schedule.api.GetActiveByTypeRequest -> GetActiveByTypeResponse
    let db_sched_type = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let stype = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) | DynamicValue::Enum(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(1);

            let sched_opt: Option<PaymentScheduleRecord> = db_sched_type.with_snapshot(|snap| {
                snap.quasar_commerce
                    .payment_schedules
                    .iter()
                    .find(|s| s.user_id == uid && s.schedule_type == stype)
                    .cloned()
            });

            let sched = match sched_opt {
                Some(s) => s,
                None => {
                    return Err((STATUS_NOT_FOUND, "No active schedule for type".to_string()));
                }
            };

            Ok(
                pb::message("eve_public.payment.schedule.api.GetActiveByTypeResponse")
                    .set("schedule.sequential", sched.id)
                    .must_to_any(),
            )
        }),
    );

    register_payment_token_and_purchase_handlers(router, Arc::clone(&db));
}

fn register_payment_token_and_purchase_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
) {
    // 11. eve_public.payment.token.api.GetQuickPayRequest -> GetQuickPayResponse
    let db_quick_pay = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.token.api.GetQuickPayRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let tokens: Vec<PaymentTokenRecord> = db_quick_pay.with_snapshot(|snap| {
                snap.quasar_commerce
                    .payment_tokens
                    .iter()
                    .filter(|t| t.user_id == uid && t.is_quick_pay)
                    .cloned()
                    .collect()
            });

            let mut msg = pb::message("eve_public.payment.token.api.GetQuickPayResponse");
            for tok in &tokens {
                msg = msg.add_with("tokens", |t| t.set("sequential", tok.id));
            }
            Ok(msg.must_to_any())
        }),
    );

    // 11b. eve_public.payment.token.api.GetRequest -> GetResponse
    let db_token_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.token.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let tok_id = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|id| *id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing token ID".to_string()))?;

            let token_opt: Option<PaymentTokenRecord> = db_token_get.with_snapshot(|snap| {
                snap.quasar_commerce
                    .payment_tokens
                    .iter()
                    .find(|t| t.id == tok_id && t.user_id == uid)
                    .cloned()
            });

            let token = match token_opt {
                Some(t) => t,
                None => {
                    return Err((STATUS_NOT_FOUND, "Payment token not found".to_string()));
                }
            };

            Ok(pb::message("eve_public.payment.token.api.GetResponse")
                .set("token.credit_card.alias", &token.token_str)
                .set("token.credit_card.expiry", "12/2030")
                .must_to_any())
        }),
    );

    // 11c. eve_public.payment.token.api.DisableAllRequest -> DisableAllResponse
    let db_token_disable = Arc::clone(&db);
    let router_pending_dis = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.token.api.DisableAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let disabled_ids = db_token_disable.with_mut_snapshot(|snap| {
                let mut ids = Vec::new();
                for t in snap
                    .quasar_commerce
                    .payment_tokens
                    .iter_mut()
                    .filter(|t| t.user_id == uid && t.is_quick_pay)
                {
                    t.is_quick_pay = false;
                    ids.push(t.id);
                }
                ids
            });

            for tid in disabled_ids {
                let notice = pb::message("eve_public.payment.token.api.QuickPayRemovedNotice")
                    .set("token.sequential", tid)
                    .must_to_any();
                router_pending_dis
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((NoticeScope::User(uid as i32), notice));
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.DisableAllResponse"
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
            require_admin_role(ctx)?;
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let type_id = if let Some(DynamicValue::Message(item_type)) = dyn_msg.get(2) {
                match item_type.get(1) {
                    Some(DynamicValue::Uint64(t)) => *t as u32,
                    Some(DynamicValue::Uint32(t)) => *t,
                    Some(DynamicValue::Int64(t)) => *t as u32,
                    Some(DynamicValue::Int32(t)) => *t as u32,
                    _ => 44992,
                }
            } else {
                44992
            };
            let qty = if let Some(DynamicValue::Int32(q)) = dyn_msg.get(3) { *q } else { 1 };
            if qty <= 0 {
                return Err((STATUS_BAD_REQUEST, "Delivery quantity must be positive".to_string()));
            }
            let requested_cid = dyn_msg.get(1).and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u as i64),
                DynamicValue::Uint64(u) => Some(*u as i64),
                DynamicValue::Int32(i) | DynamicValue::Sint32(i) => Some(*i as i64),
                DynamicValue::Int64(i) | DynamicValue::Sint64(i) => Some(*i),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u as i64),
                    DynamicValue::Uint64(u) => Some(*u as i64),
                    DynamicValue::Int32(i) | DynamicValue::Sint32(i) => Some(*i as i64),
                    DynamicValue::Int64(i) | DynamicValue::Sint64(i) => Some(*i),
                    _ => None,
                }),
                _ => None,
            }).filter(|cid| *cid > 0);

            let target_cid = if let Some(cid) = requested_cid {
                cid
            } else if let Some(cid) = ctx.effective_character_id() {
                cid as i64
            } else {
                let mut chars = db_fulfill.get_characters_for_user(uid);
                chars.sort_by_key(|c| c.character_id);
                chars.first().map(|c| c.character_id).ok_or_else(|| {
                    (STATUS_NOT_FOUND, "Target character not found".to_string())
                })?
            };
            db_fulfill
                .try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get(&target_cid).ok_or_else(|| {
                        format!("{STATUS_NOT_FOUND}:Target character {target_cid} not found")
                    })?;
                    let dest_station = ch
                        .station_id
                        .or(ch.structure_id)
                        .or((ch.home_station_id > 0).then_some(ch.home_station_id))
                        .ok_or_else(|| {
                            format!("{STATUS_BAD_REQUEST}:Target character {target_cid} has no valid station")
                        })?;

                    if let Some(existing_id) = snap.items.values().find(|it| {
                        it.owner_id == target_cid
                            && it.location_id == dest_station
                            && it.flag == eve_db::InventoryFlag::Hangar
                            && it.type_id == type_id
                            && !it.singleton
                    }).map(|it| it.item_id) {
                        let existing = snap.items.get_mut(&existing_id).unwrap();
                        existing.quantity = existing.quantity.checked_add(qty).ok_or_else(|| {
                            format!("{STATUS_BAD_REQUEST}:Stack quantity overflow")
                        })?;
                        existing.version += 1;
                    } else {
                        let mut item_id = snap.alloc_entity_id();
                        while snap.items.contains_key(&item_id) {
                            item_id = snap.alloc_entity_id();
                        }
                        snap.items.insert(
                            item_id,
                            eve_db::InventoryItemRecord {
                                item_id,
                                type_id,
                                owner_id: target_cid,
                                location_id: dest_station,
                                flag: eve_db::InventoryFlag::Hangar,
                                quantity: qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    }
                    Ok(())
                })
                .map_err(|err_str: String| {
                    if let Some((code_str, msg)) = err_str.split_once(':') {
                        if let Ok(code) = code_str.parse::<u32>() {
                            return (code, msg.to_string());
                        }
                    }
                    (STATUS_BAD_REQUEST, err_str)
                })?;

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.fulfillment.api.DeliverItemResponse"),
                value: Vec::new(),
            })
        }),
    );


    // 14. eve_public.payment.purchase.api handlers
    let db_cost = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.purchase.api.CostRequest",
        Arc::new(move |_ctx, any, dyn_msg| {
            let offer_id_opt = dyn_msg.get(1).and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u as u64),
                DynamicValue::Uint64(u) => Some(*u),
                DynamicValue::Int32(i) => Some(*i as u64),
                DynamicValue::Int64(i) => Some(*i as u64),
                DynamicValue::String(s) => s.trim_start_matches("VEVC-").parse::<u64>().ok(),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u as u64),
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u64),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    DynamicValue::String(s) => s.trim_start_matches("VEVC-").parse::<u64>().ok(),
                    _ => None,
                }),
                _ => None,
            }).or_else(|| parse_raw_varints(&any.value).get(&1).copied());
            let offer_id = offer_id_opt.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing offer ID".to_string()))?;
            let offer = db_cost.with_snapshot(|snap| {
                let catalog = read_store_catalog(snap);
                catalog.offers.into_iter().find(|o| o.id as u64 == offer_id)
            }).ok_or_else(|| (STATUS_NOT_FOUND, format!("Store offer {offer_id} not found")))?;

            let amount_in_cents = offer.price_cents.ok_or_else(|| {
                (
                    STATUS_NOT_IMPLEMENTED,
                    format!("DATA GAP: Real-money price_cents not configured for store offer {offer_id}"),
                )
            })?;
            let currency = offer.currency.as_deref().unwrap_or("USD");

            let amount_u32 = amount_in_cents.min(u64::from(u32::MAX)) as u32;
            Ok(pb::message("eve_public.payment.purchase.api.CostResponse")
                .set("cost.catalog_amount_in_cents", amount_u32)
                .set("cost.tax_amount_in_cents", 0u32)
                .set("cost.total_amount_in_cents", amount_u32)
                .set("cost.tax_rate_points", 0u32)
                .set("cost.currency", currency)
                .must_to_any())
        }),
    );

    let db_purchase = Arc::clone(&db);
    let router_pending_purchase = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.purchase.api.TokenRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let raw_fields = parse_raw_varints(&any.value);
            let offer_id_opt = if let Some(DynamicValue::Message(order)) = dyn_msg.get(1) {
                if let Some(DynamicValue::Message(off)) = order.get(1) {
                    off.get(1).and_then(|v| match v {
                        DynamicValue::String(s) => s.trim_start_matches("VEVC-").parse::<u64>().ok(),
                        DynamicValue::Uint32(u) => Some(*u as u64),
                        DynamicValue::Uint64(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u64),
                        DynamicValue::Int64(i) => Some(*i as u64),
                        _ => None,
                    })
                } else {
                    order.get(1).and_then(|v| match v {
                        DynamicValue::String(s) => s.trim_start_matches("VEVC-").parse::<u64>().ok(),
                        DynamicValue::Uint32(u) => Some(*u as u64),
                        DynamicValue::Uint64(u) => Some(*u),
                        _ => None,
                    })
                }
            } else {
                dyn_msg.get(1).and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u as u64),
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::String(s) => s.trim_start_matches("VEVC-").parse::<u64>().ok(),
                    _ => None,
                })
            }.or_else(|| raw_fields.get(&1).copied());
            let offer_id = offer_id_opt.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing offer ID".to_string()))?;

            let token_id_opt = dyn_msg.get(2).and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u),
                DynamicValue::Uint64(u) => Some(*u as u32),
                DynamicValue::Int32(i) => Some(*i as u32),
                DynamicValue::Int64(i) => Some(*i as u32),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    _ => None,
                }),
                _ => None,
            }).or_else(|| raw_fields.get(&2).map(|v| *v as u32));
            let tok_id = token_id_opt.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing token ID".to_string()))?;

            let qty = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Message(order) => order.get(2).and_then(|qv| match qv {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    }),
                    _ => None,
                })
                .or_else(|| {
                    dyn_msg.get(3).and_then(|qv| match qv {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    })
                })
                .or_else(|| raw_fields.get(&3).map(|v| *v as u32))
                .unwrap_or(1)
                .clamp(1, 10_000);

            let now_unix = ctx.now_unix();
            let active_char_id = ctx.effective_character_id().map(|c| c as i64);

            let (receipt_id, maybe_omega_notice, maybe_sched_id) = db_purchase
                .try_with_mut_snapshot(|snap| -> Result<(u64, Option<GoogleAny>, Option<u64>), String> {
                    let catalog = read_store_catalog(snap);
                    let offer = catalog
                        .offers
                        .into_iter()
                        .find(|o| o.id as u64 == offer_id)
                        .ok_or_else(|| format!("{STATUS_NOT_FOUND}:Store offer {offer_id} not found"))?;
                    if offer.price_plex < 0 {
                        return Err(format!("{STATUS_BAD_REQUEST}:Store offer {offer_id} has invalid negative PLEX price"));
                    }
                    let unit_cents = offer.price_cents.ok_or_else(|| {
                        format!(
                            "{STATUS_NOT_IMPLEMENTED}:DATA GAP: Real-money price_cents not configured for store offer {offer_id}"
                        )
                    })?;
                    let amount_in_cents = (unit_cents.min(u64::from(u32::MAX)) as u32)
                        .saturating_mul(qty);
                    let currency = offer.currency.unwrap_or_else(|| "USD".to_string());

                    let tok_idx = snap
                        .quasar_commerce
                        .payment_tokens
                        .iter()
                        .position(|t| t.id == tok_id)
                        .ok_or_else(|| format!("{STATUS_NOT_FOUND}:Payment token {tok_id} not found"))?;
                    {
                        let tok = &snap.quasar_commerce.payment_tokens[tok_idx];
                        if tok.user_id != uid {
                            return Err(format!(
                                "{STATUS_FORBIDDEN}:Forbidden: token belongs to another user"
                            ));
                        }
                        if tok.consumed {
                            return Err(format!(
                                "{STATUS_BAD_REQUEST}:Payment token already consumed"
                            ));
                        }
                        if !tok.is_quick_pay {
                            return Err(format!(
                                "{STATUS_BAD_REQUEST}:Payment token is not quick-pay enabled"
                            ));
                        }
                    }

                    let mut user_chars: Vec<_> = snap
                        .characters
                        .values()
                        .filter(|c| c.user_id == uid)
                        .cloned()
                        .collect();
                    user_chars.sort_by_key(|c| c.character_id);

                    let chosen_char = if let Some(acid) = active_char_id {
                        if let Some(ch) = snap.characters.get(&acid) {
                            if ch.user_id == uid {
                                Some(ch.clone())
                            } else {
                                return Err(format!("{STATUS_FORBIDDEN}:Target character {acid} does not belong to user"));
                            }
                        } else {
                            return Err(format!("{STATUS_NOT_FOUND}:Target character {acid} not found"));
                        }
                    } else {
                        user_chars.first().cloned()
                    };

                    let delivery_target = if offer_id != 1001 && !offer.product_ids.is_empty() {
                        let ch = chosen_char.as_ref().ok_or_else(|| {
                            format!("{STATUS_BAD_REQUEST}:No valid recipient character for purchase")
                        })?;
                        let target_station = ch
                            .station_id
                            .or(ch.structure_id)
                            .or((ch.home_station_id > 0).then_some(ch.home_station_id))
                            .ok_or_else(|| {
                                format!("{STATUS_BAD_REQUEST}:Target character has no valid station")
                            })?;
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
                        Some((ch.character_id, target_station, items_to_deliver))
                    } else {
                        None
                    };

                    snap.quasar_commerce.payment_tokens[tok_idx].consumed = true;

                    if let Some((cid, target_station, items_to_deliver)) = delivery_target {
                        for (type_id, deliver_qty) in items_to_deliver {
                            if let Some(existing) = snap.items.values_mut().find(|it| {
                                it.owner_id == cid
                                    && it.location_id == target_station
                                    && it.flag == eve_db::InventoryFlag::Hangar
                                    && it.type_id == type_id
                                    && !it.singleton
                            }) {
                                existing.quantity = existing.quantity.saturating_add(deliver_qty);
                                existing.version += 1;
                            } else {
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
                    }

                    let is_omega = offer_id == 1001 || offer.category_id == 1;
                    let mut omega_notice = None;
                    let mut sched_id_opt = None;
                    if is_omega {
                        let (new_expiry, notice) = crate::services::quasar_auth_user::activate_user_omega_license_in_snapshot(
                            snap,
                            uid,
                            30 * 86_400 * u64::from(qty),
                            now_unix,
                        );
                        let sched_id = upsert_user_payment_schedule_in_snapshot(snap, uid, 1, new_expiry, &format!("VEVC-{}", offer.id), 30 * qty, now_unix);
                        sched_id_opt = Some(sched_id);
                        omega_notice = Some(notice);
                    }

                    snap.next_entity_id += 1;
                    let rid = snap.next_entity_id as u64;
                    snap.quasar_commerce.payment_receipts.push(PaymentReceiptRecord {
                        receipt_id: rid,
                        user_id: uid,
                        offer_id: offer.id.to_string(),
                        amount_in_cents,
                        currency,
                        timestamp_unix: now_unix,
                    });

                    Ok((rid, omega_notice, sched_id_opt))
                })
                .map_err(|err_str| {
                    if let Some((code_str, msg)) = err_str.split_once(':') {
                        if let Ok(code) = code_str.parse::<u32>() {
                            return (code, msg.to_string());
                        }
                    }
                    (STATUS_BAD_REQUEST, err_str)
                })?;

            if let Some(notice) = maybe_omega_notice {
                router_pending_purchase
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((NoticeScope::User(uid as i32), notice));
            }
            if let Some(sched_id) = maybe_sched_id {
                publish_schedule_set_notice(&router_pending_purchase, uid, sched_id, 1);
            }

            Ok(pb::message("eve_public.payment.purchase.api.TokenResponse")
                .set(
                    "receipt.payment_identifier.sequential",
                    receipt_id.min(u64::from(u32::MAX)) as u32,
                )
                .set("receipt.payment_method", enum_name("PM_CREDIT_CARD"))
                .set("receipt.description", "Payment completed successfully")
                .must_to_any())
        }),
    );

    // 15. eve_public.plex.vault.transaction.api handlers
    let db_tx_all = Arc::clone(&db);
    router.register_handler(
        "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let txs = db_tx_all.with_snapshot(|snap| {
                get_plex_transactions(snap)
                    .into_iter()
                    .filter(|t| t.user_id == uid)
                    .collect::<Vec<_>>()
            });
            let mut msg =
                pb::message("eve_public.plex.vault.transaction.api.GetAllLoggedForUserResponse");
            for t in txs {
                msg = msg.add_with("transactions", |id| id.set("sequential", t.id as i64));
            }
            Ok(msg.must_to_any())
        }),
    );

    let db_tx_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.plex.vault.transaction.api.GetLogRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let tx_id = extract_u64_id(&any.value, dyn_msg).filter(|id| *id > 0).ok_or_else(|| {
                (STATUS_BAD_REQUEST, "Missing transaction ID".to_string())
            })?;

            let tx_opt = db_tx_get.with_snapshot(|snap| {
                get_plex_transactions(snap)
                    .into_iter()
                    .find(|t| t.id == tx_id && t.user_id == uid)
            });
            let tx = tx_opt.ok_or_else(|| {
                (STATUS_NOT_FOUND, "Transaction not found".to_string())
            })?;

            Ok(
                pb::message("eve_public.plex.vault.transaction.api.GetLogResponse")
                    .set(
                        "transaction.timestamp",
                        GoogleTimestamp::from_unix(tx.timestamp_unix as i64, 0),
                    )
                    .set(
                        "transaction.amount_transferred.total_in_cents",
                        tx.amount * 100,
                    )
                    .set(
                        "transaction.resulting_balance.total_in_cents",
                        tx.balance_after * 100,
                    )
                    .set("unavailable", true)
                    .must_to_any(),
            )
        }),
    );

    let db_stats = Arc::clone(&db);
    router.register_handler(
        "eve_public.plex.vault.transaction.api.GetStatisticsRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().filter(|&u| u > 0).ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))? as u64;
            let (earliest, count, incomes, expenses) = db_stats.with_snapshot(|snap| {
                let txs: Vec<_> = get_plex_transactions(snap)
                    .into_iter()
                    .filter(|t| t.user_id == uid)
                    .collect();
                let earliest = txs.iter().map(|t| t.timestamp_unix).min().unwrap_or(0);
                let count = txs.len() as u64;
                let mut inc = 0i64;
                let mut exp = 0i64;
                for t in &txs {
                    if t.amount > 0 {
                        inc += t.amount;
                    } else {
                        exp += t.amount.abs();
                    }
                }
                (earliest, count, inc, exp)
            });

            Ok(
                pb::message("eve_public.plex.vault.transaction.api.GetStatisticsResponse")
                    .add_with("entries", |e| {
                        e.set("no_classification", true)
                            .set("incomes.total_in_cents", incomes * 100)
                            .set("expenses.total_in_cents", expenses * 100)
                            .set("transactions_count", count.min(u64::from(u32::MAX)) as u32)
                    })
                    .set(
                        "earliest_transaction",
                        GoogleTimestamp::from_unix(earliest as i64, 0),
                    )
                    .must_to_any(),
            )
        }),
    );

    register_plex_vault_extra_handlers(
        router,
        Arc::clone(&db),
        Arc::clone(&router.free_fast_checkout_plex),
    );
}

#[cfg(test)]
#[path = "quasar_payment_store_tests.rs"]
mod quasar_payment_store_tests;
#[path = "quasar_payment_store_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_payment_store_cov_unit_tests;

#[cfg(test)]
#[path = "quasar_payment_store_edges_tests.rs"]
mod quasar_payment_store_edges_tests;
