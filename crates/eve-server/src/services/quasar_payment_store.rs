//! `quasar_payment_store` — Quasar Payment, PLEX, Store & Fulfillment Services (`Q-3`).

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::pb::{self, enum_name};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleDuration, GoogleTimestamp,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::Arc;

use crate::services::quasar_payment_store_types::{
    offer_matches_catalog, paginate_slice, parse_catalog_field, parse_page_request,
};
pub use crate::services::quasar_payment_store_types::*;

const OMEGA_LICENSE_TYPE: &str = "eve_clonestate_omega";

const SUPPRESS_AUTO_STORE_SEED_SLOT_KEY: &str = "quasar_suppress_auto_store_seed";

#[derive(Default)]
struct SuppressAutoStoreSeedDepth(std::sync::atomic::AtomicUsize);

struct SuppressAutoStoreSeedGuard(Arc<SuppressAutoStoreSeedDepth>);

impl Drop for SuppressAutoStoreSeedGuard {
    fn drop(&mut self) {
        self.0
            .0
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Runs `f` with automatic boot-time store catalog seeding suppressed for `db`.
pub(crate) fn suppress_auto_store_seed_for_db<D, R>(
    db: D,
    suppress: bool,
    f: impl FnOnce(D) -> R,
) -> R
where
    D: std::borrow::Borrow<WorldRepository>,
{
    let _guard = if suppress {
        let slot = db.borrow().get_or_init_transient_runtime_slot(
            SUPPRESS_AUTO_STORE_SEED_SLOT_KEY,
            SuppressAutoStoreSeedDepth::default,
        );
        slot.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Some(SuppressAutoStoreSeedGuard(slot))
    } else {
        None
    };
    f(db)
}

/// Returns whether automatic startup store catalog seeding is currently suppressed for `db`.
pub(crate) fn is_auto_store_seed_suppressed_for(db: &WorldRepository) -> bool {
    db.get_transient_runtime_slot::<SuppressAutoStoreSeedDepth>(SUPPRESS_AUTO_STORE_SEED_SLOT_KEY)
        .flatten()
        .is_some_and(|s| s.0.load(std::sync::atomic::Ordering::Acquire) > 0)
}

fn primary_offer_catalog_name(offer: &StoreOffer) -> &'static str {
    if matches!(offer.price_cents, Some(c) if c > 0) {
        "CATALOG_REAL_MONEY"
    } else {
        "CATALOG_NES"
    }
}

fn build_store_price(
    mut p: pb::ProtoBuilder,
    price_plex: i64,
    price_cents: Option<u64>,
    currency: Option<&str>,
) -> pb::ProtoBuilder {
    p = if let Some(cents) = price_cents.filter(|&c| c > 0) {
        let cur = currency
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("USD");
        let cents_i64 = i64::try_from(cents).unwrap_or(i64::MAX);
        p.set("currency_price.code", cur)
            .set("currency_price.amount_in_cents", cents_i64)
    } else {
        p.set("no_currency_price", true)
    };
    if price_plex > 0 || price_cents.is_none() {
        p.set("plex_price.total_in_cents", price_plex * 100)
    } else {
        p.set("no_plex_price", true)
    }
}

/// Encodes `eve_public.store.product.api.GetResponse` with `license` attributes for Omega
/// clone-state products (`type_id == 0`) or `item` attributes for inventory/cosmetic products.
fn encode_product_get_response(product: &StoreProduct) -> Result<GoogleAny, pb::PbError> {
    let mut msg = pb::message("eve_public.store.product.api.GetResponse")
        .set("attributes.name", &product.name);
    if is_omega_license_product(product) {
        let days = i64::from(product.quantity.max(1)) * 30;
        msg = msg
            .set(
                "attributes.license.duration",
                GoogleDuration {
                    seconds: days * 86_400,
                    nanos: 0,
                },
            )
            .set("attributes.license.type", OMEGA_LICENSE_TYPE);
    } else {
        msg = msg
            .set("attributes.item.type.sequential", u64::from(product.type_id))
            .set("attributes.item.soulbound", false);
    }
    msg.to_any()
}

/// Encodes `eve_public.store.offer.api.GetResponse` with pricing (`offer_price` + `base_price`),
/// category/product identifiers, and subscription `schedule` when the offer grants Omega time.
fn encode_offer_get_response(
    cat: &StoreCatalog,
    offer: &StoreOffer,
    tags: &[String],
) -> Result<GoogleAny, pb::PbError> {
    let catalog_name = primary_offer_catalog_name(offer);
    let total_omega_days: i64 = offer
        .product_ids
        .iter()
        .filter_map(|pid| cat.products.iter().find(|p| p.id == *pid))
        .filter(|p| is_omega_license_product(p))
        .map(|p| i64::from(p.quantity.max(1)) * 30)
        .sum();
    let is_omega_sub = total_omega_days > 0
        || offer.category_id == 1
        || tags.iter().any(|t| {
            t.eq_ignore_ascii_case("omega") || t.eq_ignore_ascii_case("UpsellStarterPack")
        });

    let mut msg = pb::message("eve_public.store.offer.api.GetResponse")
        .set("attributes.name", &offer.name)
        .set("attributes.single_purchase", false)
        .set("attributes.targeted", offer.targeted)
        .set("attributes.no_enddate", true)
        .set("attributes.can_purchase", true)
        .set_with("attributes.offer_price", |p| {
            build_store_price(p, offer.price_plex, offer.price_cents, offer.currency.as_deref())
        })
        .set_with("attributes.base_price", |p| {
            build_store_price(p, offer.price_plex, offer.price_cents, offer.currency.as_deref())
        })
        .add_with("attributes.categories", |c| {
            c.set("sequential", offer.category_id)
                .set("catalog", enum_name(catalog_name))
        });

    for &pid in &offer.product_ids {
        let qty = cat
            .products
            .iter()
            .find(|p| p.id == pid)
            .filter(|p| !is_omega_license_product(p) && p.type_id > 0 && p.quantity > 1)
            .map_or(1i32, |p| i32::try_from(p.quantity).unwrap_or(i32::MAX));
        msg = msg.add_with("attributes.products", |prod| {
            prod.set("product.sequential", pid)
                .set("product.catalog", enum_name(catalog_name))
                .set("quantity", qty)
        });
    }

    if is_omega_sub {
        let billing_days = if total_omega_days > 0 { total_omega_days } else { 30 };
        let months = (billing_days / 30).max(1);
        let monthly_plex = if offer.price_plex > 0 {
            (offer.price_plex / months).max(1)
        } else {
            0
        };
        let monthly_cents = offer.price_cents.map(|c| c / (months as u64));
        msg = msg.set_with("attributes.schedule", |s| {
            s.set(
                "billing_cycle_duration",
                GoogleDuration {
                    seconds: billing_days * 86_400,
                    nanos: 0,
                },
            )
            .set("scheduled_offer.store_offer", offer.id.to_string())
            .set_with("billing_price", |p| {
                build_store_price(p, offer.price_plex, offer.price_cents, offer.currency.as_deref())
            })
            .set_with("monthly_price", |p| {
                build_store_price(p, monthly_plex, monthly_cents, offer.currency.as_deref())
            })
        });
    } else {
        msg = msg.set("attributes.no_schedule", true);
    }

    for t in tags {
        msg = msg.add("attributes.tags", t);
    }
    msg.to_any()
}

fn encode_offer_id_list_response(
    type_name: &str,
    offers: &[&StoreOffer],
    next_tok: Option<String>,
) -> Result<GoogleAny, pb::PbError> {
    let mut msg = pb::message(type_name);
    for off in offers {
        msg = msg.add_with("offers", |o| o.set("store_offer", off.id.to_string()));
    }
    if let Some(tok) = next_tok {
        msg = msg.set("next_page.token", tok);
    }
    msg.to_any()
}

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

            pb::message("eve_public.plex.vault.api.BalanceResponse")
                .set("balance.total_in_cents", bal * 100)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
            msg.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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

            pb::message("eve_public.store.category.api.GetResponse")
                .set("attributes.name", &category.name)
                .set("attributes.no_parent", true)
                .set("attributes.no_subcategories", true)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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

            encode_product_get_response(product)
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 6. eve_public.store.offer.api.GetAllRequest -> GetAllResponse
    let db_off_all = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.offer.api.GetAllRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let catalog = parse_catalog_field(dyn_msg, 1);
            let (page_size, offset) = parse_page_request(dyn_msg, 2, usize::MAX);
            let cat = db_off_all.with_snapshot(read_store_catalog);
            let matching: Vec<&StoreOffer> = cat
                .offers
                .iter()
                .filter(|o| !o.targeted && offer_matches_catalog(o, catalog))
                .collect();
            let (slice, next_tok) = paginate_slice(&matching, offset, page_size);
            encode_offer_id_list_response(
                "eve_public.store.offer.api.GetAllResponse",
                slice,
                next_tok,
            )
            .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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

            let tags = offer_tags.get(&offer.id).map(Vec::as_slice).unwrap_or(&[]);
            encode_offer_get_response(&cat, offer, tags)
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 7. eve_public.store.offer.api.GetAllTargetedRequest -> GetAllTargetedResponse
    let db_off_tgt = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.offer.api.GetAllTargetedRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let catalog = parse_catalog_field(dyn_msg, 1);
            let (page_size, offset) = parse_page_request(dyn_msg, 2, usize::MAX);
            let cat = db_off_tgt.with_snapshot(read_store_catalog);
            let matching: Vec<&StoreOffer> = cat
                .offers
                .iter()
                .filter(|o| o.targeted && offer_matches_catalog(o, catalog))
                .collect();
            let (slice, next_tok) = paginate_slice(&matching, offset, page_size);
            encode_offer_id_list_response(
                "eve_public.store.offer.api.GetAllTargetedResponse",
                slice,
                next_tok,
            )
            .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 8. eve_public.store.targetaudience.offer.api.ListRequest -> ListResponse
    let db_ta_list = Arc::clone(&db);
    router.register_handler(
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx
                .effective_user_id()
                .filter(|&u| u > 0)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?
                as u64;
            let now_unix = ctx.now_unix();
            let out = db_ta_list.with_snapshot(|snap| {
                encode_eligible_targeted_offer_list_response(snap, uid, dyn_msg, now_unix)
            });
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
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_uid = ctx
                .effective_user_id()
                .filter(|&u| u > 0)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?
                as u64;
            let extract_offer_key = |v: &DynamicValue| -> Option<String> {
                match v {
                    DynamicValue::Uint32(u) if *u > 0 => Some(u.to_string()),
                    DynamicValue::Int32(i) if *i > 0 => Some(i.to_string()),
                    DynamicValue::Uint64(u) if *u > 0 => Some(u.to_string()),
                    DynamicValue::Int64(i) if *i > 0 => Some(i.to_string()),
                    DynamicValue::String(s) => {
                        let trimmed = s.trim();
                        (!trimmed.is_empty() && trimmed != "0").then(|| trimmed.to_string())
                    }
                    _ => None,
                }
            };
            let (req_uid, offer_key) = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => {
                    let parsed_uid = match m.get(1) {
                        Some(DynamicValue::Message(u_msg)) => {
                            u_msg.get(1).and_then(|v| match v {
                                DynamicValue::Uint64(u) => Some(*u),
                                DynamicValue::Int64(i) if *i > 0 => Some(*i as u64),
                                DynamicValue::Sint64(i) if *i > 0 => Some(*i as u64),
                                DynamicValue::Uint32(u) => Some(*u as u64),
                                DynamicValue::Int32(i) if *i > 0 => Some(*i as u64),
                                DynamicValue::Sint32(i) if *i > 0 => Some(*i as u64),
                                _ => None,
                            })
                        }
                        _ => None,
                    };
                    let parsed_offer = m.get(1).and_then(&extract_offer_key).or_else(|| {
                        m.get(2).and_then(|val| match val {
                            DynamicValue::Message(entry) => entry.get(1).and_then(&extract_offer_key),
                            _ => None,
                        })
                    });
                    (parsed_uid, parsed_offer)
                }
                Some(other) => (None, extract_offer_key(other)),
                None => (None, None),
            };
            let offer_key = offer_key
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid targeted offer ID".to_string()))?;
            let is_admin = require_admin_role(ctx).is_ok();
            let eval_uid = match req_uid.filter(|&u| u > 0) {
                Some(target_uid) if target_uid != caller_uid && !is_admin => {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Cannot access another user's targeted offer".to_string(),
                    ));
                }
                Some(target_uid) => target_uid,
                None => caller_uid,
            };
            let now_unix = ctx.now_unix();
            let stripped_key = offer_key
                .strip_prefix("VEVC-")
                .or_else(|| offer_key.strip_prefix("vevc-"))
                .unwrap_or(&offer_key);
            let offer = db_ta_get
                .with_snapshot(|snap| {
                    let cat = read_store_catalog(snap);
                    cat.offers.into_iter().find(|o| {
                        let id_str = o.id.to_string();
                        let matches_key = id_str == offer_key
                            || id_str == stripped_key
                            || o.name == offer_key
                            || find_targeted_offer_record_for_user(snap, o, eval_uid).is_some_and(
                                |rec| {
                                    rec.catalogue_entry_name == offer_key
                                        || rec.catalogue_entry_name == stripped_key
                                },
                            );
                        matches_key
                            && is_user_eligible_for_target_audience_offer(
                                snap, o, eval_uid, now_unix,
                            )
                    })
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, "Targeted offer not found".to_string()))?;

            pb::message("eve_public.store.targetaudience.offer.api.GetResponse")
                .add_with("offer.title.entries", |e| {
                    e.set("id", enum_name("EN")).set("text", &offer.name)
                })
                .add_with("offer.description.entries", |e| {
                    e.set("id", enum_name("EN")).set("text", &offer.description)
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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

            pb::message("eve_public.payment.schedule.api.GetResponse")
                .set("attributes.type", sched.schedule_type as i32)
                .set(
                    "attributes.next_payment",
                    GoogleTimestamp::from_unix(sched.next_payment_unix as i64, 0),
                )
                .set("attributes.offer.store_offer", &sched.offer_id)
                .set("attributes.recurring_interval_days", sched.interval_days)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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

            pb::message("eve_public.payment.schedule.api.GetActiveByTypeResponse")
                .set("schedule.sequential", sched.id)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
            msg.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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

            pb::message("eve_public.payment.token.api.GetResponse")
                .set("token.credit_card.alias", &token.token_str)
                .set("token.credit_card.expiry", "12/2030")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
                match pb::message("eve_public.payment.token.api.QuickPayRemovedNotice")
                    .set("token.sequential", tid)
                    .to_any()
                {
                    Ok(notice) => router_pending_dis
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((NoticeScope::User(uid as i32), notice)),
                    Err(e) => tracing::warn!(
                        user_id = uid,
                        token_id = tid,
                        error = %e,
                        "failed to encode QuickPayRemovedNotice"
                    ),
                }
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
            pb::message("eve_public.payment.purchase.api.CostResponse")
                .set("cost.catalog_amount_in_cents", amount_u32)
                .set("cost.tax_amount_in_cents", 0u32)
                .set("cost.total_amount_in_cents", amount_u32)
                .set("cost.tax_rate_points", 0u32)
                .set("cost.currency", currency)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    let db_purchase = Arc::clone(&db);
    let router_pending_purchase = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.purchase.api.TokenRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let uid = ctx
                .effective_user_id()
                .filter(|&u| u > 0)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?
                as u64;
            let raw_fields = parse_raw_varints(&any.value);
            let offer_id_opt = if let Some(DynamicValue::Message(order)) = dyn_msg.get(1) {
                if let Some(DynamicValue::Message(off)) = order.get(1) {
                    off.get(1).and_then(parse_quasar_positive_u64_id)
                } else {
                    order.get(1).and_then(parse_quasar_positive_u64_id)
                }
            } else {
                dyn_msg
                    .get(1)
                    .and_then(parse_quasar_positive_u64_id)
                    .or_else(|| raw_fields.get(&1).copied().filter(|&id| id > 0))
            };
            let offer_id =
                offer_id_opt.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing offer ID".to_string()))?;

            let token_id_opt = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Message(m) => m.get(1).and_then(parse_quasar_positive_u32_checked),
                    other => parse_quasar_positive_u32_checked(other),
                })
                .or_else(|| {
                    raw_fields
                        .get(&2)
                        .copied()
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|&v| v > 0)
                });
            let tok_id =
                token_id_opt.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid token ID".to_string()))?;

            ensure_quasar_wire_u32_quantity(&any.value, true)
                .map_err(|msg| (STATUS_BAD_REQUEST, msg))?;
            let qty = parse_quasar_clamped_token_quantity(dyn_msg, raw_fields.get(&3).copied())
                .map_err(|msg| (STATUS_BAD_REQUEST, msg))?;

            let now_unix = ctx.now_unix();
            let idem_key = ctx
                .idempotency_key
                .clone()
                .filter(|k| !k.is_empty())
                .or_else(|| extract_quasar_store_idempotency_key(dyn_msg));
            let input = StorePurchaseInput {
                channel: StorePurchaseChannel::PaymentTokenQuasar,
                buyer_user_id: uid,
                active_char_id: ctx.effective_character_id().map(|c| c as i64),
                gift_char_id: None,
                session_station_id: None,
                offer_id,
                quantity: qty,
                now_unix,
                payment: StorePurchasePaymentSpec::PaymentTokenById {
                    token_id: tok_id,
                    create_omega_schedule: true,
                },
                fast_checkout_mode: false,
                tx_description_mode: StoreTxDescriptionMode::NesOfferNameUpper,
                idempotency_key: idem_key.as_deref(),
            };

            let outcome = db_purchase
                .try_with_mut_snapshot(|snap| purchase_store_offer_in_snapshot(snap, &input))
                .map_err(store_purchase_error_to_quasar)?;

            emit_store_purchase_notices(&router_pending_purchase, &outcome, false, now_unix);

            pb::message("eve_public.payment.purchase.api.TokenResponse")
                .set(
                    "receipt.payment_identifier.sequential",
                    outcome.tx_id.min(u64::from(u32::MAX)) as u32,
                )
                .set("receipt.payment_method", enum_name("PM_CREDIT_CARD"))
                .set("receipt.description", "Payment completed successfully")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
            msg.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
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
