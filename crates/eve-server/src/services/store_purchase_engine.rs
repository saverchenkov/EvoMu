//! Shared New Eden Store & FastCheckout purchase validation and atomic snapshot settlement.

use super::quasar_payment_store_types::{
    compute_omega_license_duration_secs, deliver_store_hangar_items_in_snapshot,
    is_fast_checkout_catalog_offer, is_user_eligible_for_targeted_offer, read_store_catalog,
    read_store_offer_tags, record_plex_transaction, resolve_omega_weekly_free_sp_grant,
    upsert_user_payment_schedule_in_snapshot, PaymentReceiptRecord, StoreCatalog, StoreOffer,
};
use crate::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED,
};
use eve_db::service_domains::CommittedStorePurchaseRecord;
use eve_db::WorldStateSnapshot;
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue, GoogleAny};
use eve_proto::PyRep;
use std::collections::{BTreeMap, HashMap};

pub(crate) const MAX_STORE_PURCHASE_IDEMPOTENCY_PER_USER: usize = 64;
pub(crate) const MAX_STORE_PURCHASE_IDEMPOTENCY_ENTRIES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorePurchaseChannel {
    PlexVaultQuasar,
    PaymentTokenQuasar,
    FastCheckout,
    StoreManager,
}

impl StorePurchaseChannel {
    fn tag(self) -> &'static str {
        match self {
            Self::PlexVaultQuasar => "plex_vault",
            Self::PaymentTokenQuasar => "payment_token",
            Self::FastCheckout => "fast_checkout",
            Self::StoreManager => "store_manager",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreTxDescriptionMode {
    NesOfferNameUpper,
    NesOfferNameLower,
    FastCheckoutFixed,
}

impl StoreTxDescriptionMode {
    fn format(self, offer_name: &str) -> String {
        match self {
            Self::NesOfferNameUpper => format!("NES Purchase: {offer_name}"),
            Self::NesOfferNameLower => format!("NES purchase: {offer_name}"),
            Self::FastCheckoutFixed => "FastCheckout Purchase".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum StorePurchasePaymentSpec {
    PlexVault {
        allow_free_when_zero: bool,
    },
    PaymentTokenById {
        token_id: u32,
        create_omega_schedule: bool,
    },
    FastCheckoutAuto {
        free_fast_checkout_mode: bool,
        allow_default_free_plex_packs: bool,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct StorePurchaseInput<'a> {
    pub channel: StorePurchaseChannel,
    pub buyer_user_id: u64,
    pub active_char_id: Option<i64>,
    pub gift_char_id: Option<i64>,
    pub session_station_id: Option<i64>,
    pub offer_id: u64,
    pub quantity: u32,
    pub now_unix: u64,
    pub payment: StorePurchasePaymentSpec,
    pub fast_checkout_mode: bool,
    pub tx_description_mode: StoreTxDescriptionMode,
    pub idempotency_key: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(crate) struct StorePurchaseOutcome {
    pub tx_id: u64,
    pub buyer_user_id: u64,
    pub plex_spent: i64,
    pub plex_credited: i64,
    pub net_plex_delta: i64,
    pub new_plex_balance: i64,
    pub is_free_mode_settlement: bool,
    pub tx_description: String,
    pub omega_target_user_id: u64,
    pub omega_notice: Option<GoogleAny>,
    pub schedule_id: Option<u64>,
    pub delivered_mutations: Vec<(
        i64,
        Option<eve_db::InventoryItemRecord>,
        Option<eve_db::InventoryItemRecord>,
    )>,
    pub delivered_item_id: i64,
    pub delivered_items_count: i64,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum StorePurchaseError {
    OfferNotFound(String),
    InvalidOffer(String),
    InvalidQuantity(String),
    Overflow(String),
    TargetedNotEligible(String),
    UnfulfillableVirtualOffer(String),
    GiftRecipientNotFound(String),
    NoDeliveryCharacter(String),
    TargetCharacterNotFound(String),
    TargetCharacterForbidden(String),
    MissingDeliveryStation(String),
    PaymentUnavailable(String),
    PaymentTokenRequired(String),
    PaymentTokenNotFound(String),
    PaymentTokenForbidden(String),
    PaymentTokenInvalid(String),
    InsufficientPlex { required: i64, available: i64 },
    IdempotencyConflict(String),
    PersistenceFailure(String),
}

impl eve_db::FromWalError for StorePurchaseError {
    fn from_wal_error(err: String) -> Self {
        Self::PersistenceFailure(err)
    }
}

pub(crate) fn store_purchase_error_to_quasar(err: StorePurchaseError) -> (u32, String) {
    match err {
        StorePurchaseError::OfferNotFound(m)
        | StorePurchaseError::GiftRecipientNotFound(m)
        | StorePurchaseError::TargetCharacterNotFound(m)
        | StorePurchaseError::PaymentTokenNotFound(m) => (STATUS_NOT_FOUND, m),
        StorePurchaseError::TargetedNotEligible(m)
        | StorePurchaseError::TargetCharacterForbidden(m)
        | StorePurchaseError::PaymentTokenForbidden(m) => (STATUS_FORBIDDEN, m),
        StorePurchaseError::PaymentUnavailable(m) => (STATUS_NOT_IMPLEMENTED, m),
        StorePurchaseError::PersistenceFailure(m) => (STATUS_INTERNAL_ERROR, m),
        StorePurchaseError::InsufficientPlex { required, available } => (
            STATUS_BAD_REQUEST,
            format!("Insufficient PLEX balance: need {required}, have {available}"),
        ),
        StorePurchaseError::InvalidOffer(m)
        | StorePurchaseError::InvalidQuantity(m)
        | StorePurchaseError::Overflow(m)
        | StorePurchaseError::UnfulfillableVirtualOffer(m)
        | StorePurchaseError::NoDeliveryCharacter(m)
        | StorePurchaseError::MissingDeliveryStation(m)
        | StorePurchaseError::PaymentTokenRequired(m)
        | StorePurchaseError::PaymentTokenInvalid(m)
        | StorePurchaseError::IdempotencyConflict(m) => (STATUS_BAD_REQUEST, m),
    }
}

pub(crate) fn encode_idempotency_bytes_hex(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        None
    } else {
        Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

pub(crate) fn extract_quasar_store_idempotency_key(
    dyn_msg: &DynamicQuasarMessage,
) -> Option<String> {
    fn visit(msg: &DynamicQuasarMessage, depth: u8) -> Option<String> {
        if depth > 4 {
            return None;
        }
        if msg.full_name.ends_with("idempotency.Token") {
            return msg.get(1).and_then(|v| match v {
                DynamicValue::String(s) if !s.is_empty() => {
                    encode_idempotency_bytes_hex(s.as_bytes())
                }
                DynamicValue::Bytes(b) => encode_idempotency_bytes_hex(b),
                _ => None,
            });
        }
        for val in msg.fields.values() {
            if let DynamicValue::Message(sub) = val {
                if let Some(found) = visit(sub, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    visit(dyn_msg, 0)
}

pub(crate) fn extract_rpc_idempotency_key(kwargs: &HashMap<String, PyRep>) -> Option<String> {
    kwargs
        .get("idempotency_key")
        .or_else(|| kwargs.get("idempotencyKey"))
        .and_then(|v| match v {
            PyRep::String(s) if !s.is_empty() => Some(s.clone()),
            PyRep::Bytes(b) | PyRep::Buffer(b) => encode_idempotency_bytes_hex(b),
            _ => None,
        })
}

pub(crate) fn parse_quasar_positive_u64_id(val: &DynamicValue) -> Option<u64> {
    match val {
        DynamicValue::String(s) => s.trim_start_matches("VEVC-").parse::<u64>().ok(),
        DynamicValue::Uint32(u) => Some(u64::from(*u)),
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        _ => None,
    }
    .filter(|&id| id > 0)
}

pub(crate) fn parse_quasar_positive_u32_checked(val: &DynamicValue) -> Option<u32> {
    match val {
        DynamicValue::Uint32(u) => Some(*u),
        DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
        DynamicValue::Int32(i) => u32::try_from(*i).ok(),
        DynamicValue::Int64(i) => u32::try_from(*i).ok(),
        _ => None,
    }
    .filter(|&v| v > 0)
}

pub(crate) fn ensure_quasar_wire_u32_quantity(
    raw_bytes: &[u8],
    nested_in_order: bool,
) -> Result<(), String> {
    let Ok(wm) = eve_proto::quasar::WireMessage::decode(raw_bytes) else {
        return Ok(());
    };
    if nested_in_order {
        for f in &wm.fields {
            if f.field_number == 1 {
                if let eve_proto::quasar::WireValue::LengthDelimited(ref sub_bytes) = f.value {
                    if let Ok(order_wm) = eve_proto::quasar::WireMessage::decode(sub_bytes) {
                        for sf in &order_wm.fields {
                            if sf.field_number == 2 {
                                if let eve_proto::quasar::WireValue::Varint(v) = sf.value {
                                    u32::try_from(v).map_err(|_| {
                                        "Purchase quantity overflows u32".to_string()
                                    })?;
                                }
                            }
                        }
                    }
                }
            } else if f.field_number == 3 {
                if let eve_proto::quasar::WireValue::Varint(v) = f.value {
                    u32::try_from(v)
                        .map_err(|_| "Purchase quantity overflows u32".to_string())?;
                }
            }
        }
    } else {
        for f in &wm.fields {
            if f.field_number == 2 {
                if let eve_proto::quasar::WireValue::Varint(v) = f.value {
                    u32::try_from(v)
                        .map_err(|_| "Purchase quantity overflows u32".to_string())?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn parse_quasar_clamped_token_quantity(
    dyn_msg: &DynamicQuasarMessage,
    raw_qty: Option<u64>,
) -> Result<u32, String> {
    let order_qty = dyn_msg.get(1).and_then(|v| match v {
        DynamicValue::Message(order) => order.get(2),
        _ => None,
    });
    if let Some(qv) = order_qty.or_else(|| dyn_msg.get(3)) {
        let parsed = match qv {
            DynamicValue::Uint32(u) => Ok(*u),
            DynamicValue::Uint64(u) => {
                u32::try_from(*u).map_err(|_| "Purchase quantity overflows u32".to_string())
            }
            DynamicValue::Int32(i) => {
                u32::try_from(*i).map_err(|_| "Purchase quantity must be non-negative".to_string())
            }
            DynamicValue::Int64(i) => {
                u32::try_from(*i).map_err(|_| "Purchase quantity overflows u32".to_string())
            }
            _ => Err("Invalid purchase quantity".to_string()),
        }?;
        return Ok(parsed.clamp(1, 10_000));
    }
    if let Some(raw_q) = raw_qty {
        let parsed = u32::try_from(raw_q)
            .map_err(|_| "Purchase quantity overflows u32".to_string())?;
        return Ok(parsed.clamp(1, 10_000));
    }
    Ok(1)
}

pub(crate) fn store_offer_has_deliverable_fulfillment(
    offer: &StoreOffer,
    catalog: &StoreCatalog,
    offer_tags: &[String],
) -> bool {
    if compute_omega_license_duration_secs(offer, catalog, offer_tags, 1).is_some() {
        return true;
    }
    offer.product_ids.iter().any(|pid| {
        catalog
            .products
            .iter()
            .any(|p| p.id == *pid && p.quantity > 0 && p.type_id > 0)
    })
}

pub(crate) fn validate_store_char_ownership(
    snap: &WorldStateSnapshot,
    buyer_user_id: u64,
    char_id: i64,
) -> Result<(), StorePurchaseError> {
    match snap.characters.get(&char_id) {
        Some(ch) if ch.user_id != buyer_user_id => Err(StorePurchaseError::TargetCharacterForbidden(
            format!("Target character {char_id} does not belong to user"),
        )),
        None => Err(StorePurchaseError::TargetCharacterNotFound(format!(
            "Target character {char_id} not found"
        ))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn resolve_eligible_omega_weekly_free_sp_grant(
    snap: &WorldStateSnapshot,
    user_id: u64,
    char_id: i64,
    offer_id: i64,
    now_unix: u64,
) -> Result<Option<i64>, StorePurchaseError> {
    let Some(sp_grant) = resolve_omega_weekly_free_sp_grant(snap, offer_id) else {
        return Ok(None);
    };
    validate_store_char_ownership(snap, user_id, char_id)?;
    let catalog = read_store_catalog(snap);
    if let Some(offer) = catalog.offers.iter().find(|o| i64::from(o.id) == offer_id) {
        if offer.targeted
            && !is_user_eligible_for_targeted_offer(snap, offer, user_id, now_unix)
        {
            return Err(StorePurchaseError::TargetedNotEligible(format!(
                "User {user_id} is not eligible for targeted offer {offer_id}"
            )));
        }
    }
    Ok(Some(sp_grant))
}

enum PreparedStorePurchase {
    IdempotentReplay {
        record: CommittedStorePurchaseRecord,
        new_plex_balance: i64,
    },
    Ready(PreparedStorePurchaseData),
}

struct PreparedStorePurchaseData {
    offer_id: u64,
    tx_description: String,
    vault_uid: u64,
    total_cost_plex: i64,
    total_plex_credit: i64,
    net_plex_delta: i64,
    amount_in_cents: u32,
    currency: String,
    is_free_mode_settlement: bool,
    consume_token_idx: Option<usize>,
    record_receipt: bool,
    record_plex_tx: bool,
    create_omega_schedule: bool,
    omega_duration_secs: Option<u64>,
    omega_target_uid: u64,
    delivery_target: Option<(i64, i64, Vec<(u32, i32)>)>,
    delivered_items_count: i64,
    payment_token_id: Option<u32>,
    idem_key: Option<(String, String)>,
}

struct ResolvedStorePayment {
    total_cost_plex: i64,
    amount_in_cents: u32,
    currency: String,
    is_free_mode_settlement: bool,
    consume_token_idx: Option<usize>,
    record_receipt: bool,
    record_plex_tx: bool,
    create_omega_schedule: bool,
}

fn catalog_has_fast_checkout_plex_offer(
    catalog: &StoreCatalog,
    tags_map: &HashMap<u32, Vec<String>>,
) -> bool {
    catalog.offers.iter().any(|offer| {
        let tags = tags_map.get(&offer.id).map(Vec::as_slice).unwrap_or(&[]);
        is_fast_checkout_catalog_offer(offer, catalog, tags)
            && offer.product_ids.iter().any(|pid| {
                catalog.products.iter().any(|p| {
                    p.id == *pid
                        && p.type_id == crate::services::market_contracts::PLEX_TYPE_ID
                        && p.quantity > 0
                })
            })
    })
}

fn resolve_store_purchase_offer(
    snap: &WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
) -> Result<(StoreOffer, StoreCatalog, Vec<String>, i64), StorePurchaseError> {
    let catalog = read_store_catalog(snap);
    let tags_map = read_store_offer_tags(snap);
    if let Some(offer) = catalog
        .offers
        .iter()
        .find(|o| u64::from(o.id) == input.offer_id)
        .cloned()
    {
        let tags = tags_map.get(&offer.id).cloned().unwrap_or_default();
        if input.fast_checkout_mode && !is_fast_checkout_catalog_offer(&offer, &catalog, &tags) {
            return Err(StorePurchaseError::OfferNotFound(format!(
                "Offer {} is not eligible for FastCheckout",
                input.offer_id
            )));
        }
        if offer.targeted
            && !is_user_eligible_for_targeted_offer(snap, &offer, input.buyer_user_id, input.now_unix)
        {
            return Err(StorePurchaseError::TargetedNotEligible(format!(
                "User {} is not eligible for targeted offer {}",
                input.buyer_user_id, input.offer_id
            )));
        }
        let mut plex_credit_per_unit = 0_i64;
        if input.fast_checkout_mode {
            for pid in &offer.product_ids {
                if let Some(prod) = catalog.products.iter().find(|p| p.id == *pid) {
                    if prod.type_id == crate::services::market_contracts::PLEX_TYPE_ID
                        && prod.quantity > 0
                    {
                        plex_credit_per_unit = plex_credit_per_unit
                            .checked_add(i64::from(prod.quantity))
                            .ok_or_else(|| {
                                StorePurchaseError::Overflow(
                                    "Total PLEX quantity overflows i64".into(),
                                )
                            })?;
                    }
                }
            }
        }
        return Ok((offer, catalog, tags, plex_credit_per_unit));
    }

    if let StorePurchasePaymentSpec::FastCheckoutAuto {
        free_fast_checkout_mode: true,
        allow_default_free_plex_packs: true,
    } = input.payment
    {
        if !catalog_has_fast_checkout_plex_offer(&catalog, &tags_map) {
            if let Some(pack) =
                crate::services::fast_checkout_and_activity::default_free_plex_pack_offers()
                    .into_iter()
                    .find(|p| p.offer_id >= 0 && p.offer_id as u64 == input.offer_id)
            {
                let unit_cents = (pack.price_usd * 100.0).round() as u64;
                let synthetic_offer = StoreOffer {
                    id: pack.offer_id as u32,
                    name: pack.name,
                    description: pack.description,
                    category_id: pack.category_id,
                    price_plex: pack.price_plex,
                    product_ids: Vec::new(),
                    targeted: false,
                    price_cents: Some(unit_cents),
                    currency: Some("USD".to_string()),
                };
                return Ok((synthetic_offer, catalog, pack.tags, pack.plex_amount));
            }
        }
    }

    Err(StorePurchaseError::OfferNotFound(format!(
        "Store offer {} not found",
        input.offer_id
    )))
}

fn find_quick_pay_token_for_user(
    snap: &WorldStateSnapshot,
    user_id: u64,
    explicit_token_id: Option<u32>,
) -> Result<usize, StorePurchaseError> {
    if let Some(token_id) = explicit_token_id {
        let tok_idx = snap
            .quasar_commerce
            .payment_tokens
            .iter()
            .position(|t| t.id == token_id)
            .ok_or_else(|| {
                StorePurchaseError::PaymentTokenNotFound(format!(
                    "Payment token {token_id} not found"
                ))
            })?;
        let tok = &snap.quasar_commerce.payment_tokens[tok_idx];
        if tok.user_id != user_id {
            return Err(StorePurchaseError::PaymentTokenForbidden(
                "Forbidden: token belongs to another user".into(),
            ));
        }
        if tok.consumed {
            return Err(StorePurchaseError::PaymentTokenInvalid(
                "Payment token already consumed".into(),
            ));
        }
        if !tok.is_quick_pay {
            return Err(StorePurchaseError::PaymentTokenInvalid(
                "Payment token is not quick-pay enabled".into(),
            ));
        }
        return Ok(tok_idx);
    }

    snap.quasar_commerce
        .payment_tokens
        .iter()
        .position(|t| t.user_id == user_id && t.is_quick_pay && !t.consumed)
        .ok_or_else(|| {
            StorePurchaseError::PaymentTokenRequired(
                "No active quick-pay payment token for user".into(),
            )
        })
}

fn resolve_store_purchase_payment(
    snap: &WorldStateSnapshot,
    offer: &StoreOffer,
    input: &StorePurchaseInput<'_>,
) -> Result<ResolvedStorePayment, StorePurchaseError> {
    if offer.price_plex < 0 {
        return Err(StorePurchaseError::InvalidOffer(format!(
            "Offer {} has invalid negative PLEX price",
            offer.id
        )));
    }

    match input.payment {
        StorePurchasePaymentSpec::PlexVault {
            allow_free_when_zero,
        } => {
            if offer.price_plex == 0 && !allow_free_when_zero {
                return Err(StorePurchaseError::InvalidOffer(format!(
                    "Offer {} has invalid non-positive PLEX price",
                    offer.id
                )));
            }
            let total_plex = offer
                .price_plex
                .checked_mul(i64::from(input.quantity))
                .filter(|&v| v >= 0)
                .ok_or_else(|| StorePurchaseError::Overflow("PLEX price overflows i64".into()))?;
            Ok(ResolvedStorePayment {
                total_cost_plex: total_plex,
                amount_in_cents: 0,
                currency: "PLX".to_string(),
                is_free_mode_settlement: offer.price_plex == 0,
                consume_token_idx: None,
                record_receipt: false,
                record_plex_tx: true,
                create_omega_schedule: false,
            })
        }
        StorePurchasePaymentSpec::PaymentTokenById {
            token_id,
            create_omega_schedule,
        } => {
            let unit_cents_u64 = offer.price_cents.ok_or_else(|| {
                StorePurchaseError::PaymentUnavailable(format!(
                    "DATA GAP: Real-money price_cents not configured for store offer {}",
                    offer.id
                ))
            })?;
            let unit_cents = u32::try_from(unit_cents_u64).map_err(|_| {
                StorePurchaseError::Overflow("Payment amount overflows u32".into())
            })?;
            let amount = unit_cents.checked_mul(input.quantity).ok_or_else(|| {
                StorePurchaseError::Overflow("Payment amount overflows u32".into())
            })?;
            let tok_idx =
                find_quick_pay_token_for_user(snap, input.buyer_user_id, Some(token_id))?;
            let currency = offer.currency.clone().unwrap_or_else(|| "USD".to_string());
            Ok(ResolvedStorePayment {
                total_cost_plex: 0,
                amount_in_cents: amount,
                currency,
                is_free_mode_settlement: false,
                consume_token_idx: Some(tok_idx),
                record_receipt: true,
                record_plex_tx: false,
                create_omega_schedule,
            })
        }
        StorePurchasePaymentSpec::FastCheckoutAuto {
            free_fast_checkout_mode,
            ..
        } => {
            if offer.price_plex > 0 {
                let total_plex = offer
                    .price_plex
                    .checked_mul(i64::from(input.quantity))
                    .filter(|&v| v >= 0)
                    .ok_or_else(|| {
                        StorePurchaseError::Overflow("Total PLEX price overflows i64".into())
                    })?;
                return Ok(ResolvedStorePayment {
                    total_cost_plex: total_plex,
                    amount_in_cents: 0,
                    currency: "PLX".to_string(),
                    is_free_mode_settlement: false,
                    consume_token_idx: None,
                    record_receipt: false,
                    record_plex_tx: true,
                    create_omega_schedule: false,
                });
            }
            let unit_cents_u64 = offer.price_cents.filter(|&c| c > 0).ok_or_else(|| {
                StorePurchaseError::InvalidOffer(format!(
                    "Offer {} has no valid price",
                    offer.id
                ))
            })?;
            let unit_cents = u32::try_from(unit_cents_u64).map_err(|_| {
                StorePurchaseError::Overflow("Payment amount overflows u32".into())
            })?;
            if free_fast_checkout_mode {
                return Ok(ResolvedStorePayment {
                    total_cost_plex: 0,
                    amount_in_cents: 0,
                    currency: "FREE".to_string(),
                    is_free_mode_settlement: true,
                    consume_token_idx: None,
                    record_receipt: false,
                    record_plex_tx: false,
                    create_omega_schedule: false,
                });
            }
            let amount = unit_cents.checked_mul(input.quantity).ok_or_else(|| {
                StorePurchaseError::Overflow("Payment amount overflows u32".into())
            })?;
            let tok_idx = find_quick_pay_token_for_user(snap, input.buyer_user_id, None)?;
            let currency = offer.currency.clone().unwrap_or_else(|| "USD".to_string());
            Ok(ResolvedStorePayment {
                total_cost_plex: 0,
                amount_in_cents: amount,
                currency,
                is_free_mode_settlement: false,
                consume_token_idx: Some(tok_idx),
                record_receipt: true,
                record_plex_tx: false,
                create_omega_schedule: false,
            })
        }
    }
}

fn collect_store_purchase_delivery(
    snap: &WorldStateSnapshot,
    offer: &StoreOffer,
    catalog: &StoreCatalog,
    input: &StorePurchaseInput<'_>,
    recipient_cid: Option<i64>,
) -> Result<(Option<(i64, i64, Vec<(u32, i32)>)>, i64), StorePurchaseError> {
    let mut items_to_deliver = Vec::new();
    for pid in &offer.product_ids {
        if let Some(prod) = catalog.products.iter().find(|p| p.id == *pid) {
            if prod.type_id > 0
                && prod.quantity > 0
                && !(input.fast_checkout_mode
                    && prod.type_id == crate::services::market_contracts::PLEX_TYPE_ID)
            {
                let deliver_qty = prod
                    .quantity
                    .checked_mul(input.quantity)
                    .and_then(|q| i32::try_from(q).ok())
                    .ok_or_else(|| {
                        StorePurchaseError::Overflow("Delivery quantity overflow".into())
                    })?;
                items_to_deliver.push((prod.type_id, deliver_qty));
            }
        }
    }
    let delivered_items_count: i64 = items_to_deliver.iter().map(|&(_, q)| i64::from(q)).sum();
    if items_to_deliver.is_empty() {
        return Ok((None, 0));
    }

    let Some(cid) = recipient_cid else {
        return Err(StorePurchaseError::NoDeliveryCharacter(
            "No valid recipient character for store delivery".to_string(),
        ));
    };
    let Some(ch) = snap.characters.get(&cid) else {
        return Err(StorePurchaseError::TargetCharacterNotFound(format!(
            "Recipient character {cid} not found"
        )));
    };
    let session_station =
        if input.gift_char_id.is_none() && input.active_char_id == Some(cid) {
            input.session_station_id.filter(|&st| st > 0)
        } else {
            None
        };
    let Some(target_station) = session_station
        .or_else(|| ch.station_id.filter(|&st| st > 0))
        .or_else(|| ch.structure_id.filter(|&st| st > 0))
        .or_else(|| (ch.home_station_id > 0).then_some(ch.home_station_id))
    else {
        return Err(StorePurchaseError::MissingDeliveryStation(format!(
            "Target character {cid} has no valid delivery station"
        )));
    };

    if !matches!(input.channel, StorePurchaseChannel::StoreManager) {
        let mut totals_by_type: BTreeMap<u32, i32> = BTreeMap::new();
        for &(type_id, deliver_qty) in &items_to_deliver {
            let entry = totals_by_type.entry(type_id).or_insert(0);
            *entry = entry.checked_add(deliver_qty).ok_or_else(|| {
                StorePurchaseError::Overflow("Delivery quantity overflow".into())
            })?;
        }
        for (type_id, total_deliver_qty) in totals_by_type {
            if let Some(existing) = snap.items.values().find(|it| {
                it.owner_id == cid
                    && it.location_id == target_station
                    && it.flag == eve_db::InventoryFlag::Hangar
                    && it.type_id == type_id
                    && !it.singleton
            }) {
                if existing.quantity.checked_add(total_deliver_qty).is_none() {
                    return Err(StorePurchaseError::Overflow(
                        "Existing hangar stack quantity overflows i32".into(),
                    ));
                }
            }
        }
    }

    Ok((
        Some((cid, target_station, items_to_deliver)),
        delivered_items_count,
    ))
}

fn validate_active_char_ownership(
    snap: &WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
) -> Result<(), StorePurchaseError> {
    if input.gift_char_id.is_some() {
        return Ok(());
    }
    if let Some(acid) = input.active_char_id {
        validate_store_char_ownership(snap, input.buyer_user_id, acid)?;
    }
    Ok(())
}

fn input_payment_token_id(input: &StorePurchaseInput<'_>) -> Option<u32> {
    match input.payment {
        StorePurchasePaymentSpec::PaymentTokenById { token_id, .. } => Some(token_id),
        StorePurchasePaymentSpec::PlexVault { .. }
        | StorePurchasePaymentSpec::FastCheckoutAuto { .. } => None,
    }
}

fn check_store_purchase_idempotency_replay<'a>(
    snap: &'a WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
    idem_key: &Option<(String, String)>,
) -> Result<Option<(&'a CommittedStorePurchaseRecord, i64)>, StorePurchaseError> {
    let Some((map_key, _)) = idem_key else {
        return Ok(None);
    };
    let Some(record) = snap.quasar_commerce.store_purchase_idempotency.get(map_key) else {
        return Ok(None);
    };
    validate_active_char_ownership(snap, input)?;
    let req_token_id = input_payment_token_id(input);
    if record.offer_id != input.offer_id
        || record.quantity != input.quantity
        || record.gift_char_id != input.gift_char_id
        || record.active_char_id != input.active_char_id
        || record.payment_token_id != req_token_id
    {
        return Err(StorePurchaseError::IdempotencyConflict(format!(
            "Idempotency key '{}' already committed with different parameters",
            record.idempotency_key
        )));
    }
    Ok(Some((
        record,
        snap.unified_plex_balance_for_user(input.buyer_user_id),
    )))
}

fn prepare_store_purchase_in_snapshot(
    snap: &WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
    allow_idempotent_replay: bool,
) -> Result<PreparedStorePurchase, StorePurchaseError> {
    if input.quantity == 0 {
        return Err(StorePurchaseError::InvalidQuantity(
            "Purchase quantity must be positive".to_string(),
        ));
    }

    let vault_uid = input.buyer_user_id;
    let idem_key = raw_and_map_store_idempotency_key(input);
    if allow_idempotent_replay {
        if let Some((record, new_plex_balance)) =
            check_store_purchase_idempotency_replay(snap, input, &idem_key)?
        {
            return Ok(PreparedStorePurchase::IdempotentReplay {
                record: record.clone(),
                new_plex_balance,
            });
        }
    }

    if let Some(gift_cid) = input.gift_char_id {
        if !snap.characters.contains_key(&gift_cid) {
            return Err(StorePurchaseError::GiftRecipientNotFound(format!(
                "Gift recipient character {gift_cid} not found"
            )));
        }
    }

    let (offer, catalog, tags, plex_credit_per_unit) =
        resolve_store_purchase_offer(snap, input)?;

    validate_active_char_ownership(snap, input)?;

    let payment = resolve_store_purchase_payment(snap, &offer, input)?;

    let total_plex_credit = plex_credit_per_unit
        .checked_mul(i64::from(input.quantity))
        .filter(|&v| v >= 0)
        .ok_or_else(|| {
            StorePurchaseError::Overflow("Total PLEX quantity overflows i64".into())
        })?;

    let recipient_cid = input.gift_char_id.or(input.active_char_id).or_else(|| {
        snap.characters
            .values()
            .filter(|c| c.user_id == input.buyer_user_id)
            .map(|c| c.character_id)
            .min()
    });
    let (delivery_target, delivered_items_count) =
        collect_store_purchase_delivery(snap, &offer, &catalog, input, recipient_cid)?;

    let omega_duration_secs =
        compute_omega_license_duration_secs(&offer, &catalog, &tags, input.quantity);
    let offer_is_priced = payment.total_cost_plex > 0
        || payment.amount_in_cents > 0
        || offer.price_plex > 0
        || offer.price_cents.is_some_and(|c| c > 0);
    let has_deliverable =
        omega_duration_secs.is_some() || total_plex_credit > 0 || delivery_target.is_some();
    if offer_is_priced && !has_deliverable {
        return Err(StorePurchaseError::UnfulfillableVirtualOffer(format!(
            "Offer {} has no deliverable physical, PLEX, or Omega fulfillment",
            offer.id
        )));
    }

    let cur_bal = snap.unified_plex_balance_for_user(vault_uid);
    if payment.total_cost_plex > 0 && cur_bal < payment.total_cost_plex {
        return Err(StorePurchaseError::InsufficientPlex {
            required: payment.total_cost_plex,
            available: cur_bal,
        });
    }
    let after_debit = cur_bal.checked_sub(payment.total_cost_plex).ok_or_else(|| {
        StorePurchaseError::Overflow("PLEX balance underflows i64".into())
    })?;
    if after_debit.checked_add(total_plex_credit).is_none() {
        return Err(StorePurchaseError::Overflow(
            "Account PLEX vault overflows i64".into(),
        ));
    }
    let net_plex_delta = total_plex_credit
        .checked_sub(payment.total_cost_plex)
        .ok_or_else(|| StorePurchaseError::Overflow("Net PLEX delta overflows i64".into()))?;

    let omega_target_uid = if input.gift_char_id.is_some() {
        recipient_cid
            .and_then(|cid| snap.characters.get(&cid).map(|c| c.user_id))
            .unwrap_or(input.buyer_user_id)
    } else {
        input.buyer_user_id
    };

    Ok(PreparedStorePurchase::Ready(PreparedStorePurchaseData {
        offer_id: input.offer_id,
        tx_description: input.tx_description_mode.format(&offer.name),
        vault_uid,
        total_cost_plex: payment.total_cost_plex,
        total_plex_credit,
        net_plex_delta,
        amount_in_cents: payment.amount_in_cents,
        currency: payment.currency,
        is_free_mode_settlement: payment.is_free_mode_settlement,
        consume_token_idx: payment.consume_token_idx,
        record_receipt: payment.record_receipt,
        record_plex_tx: payment.record_plex_tx
            || total_plex_credit > 0
            || payment.total_cost_plex > 0,
        create_omega_schedule: payment.create_omega_schedule,
        omega_duration_secs,
        omega_target_uid,
        delivery_target,
        delivered_items_count,
        payment_token_id: input_payment_token_id(input),
        idem_key,
    }))
}

fn commit_store_purchase_in_snapshot(
    snap: &mut WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
    data: PreparedStorePurchaseData,
) -> Result<StorePurchaseOutcome, StorePurchaseError> {
    if let Some(tok_idx) = data.consume_token_idx {
        snap.quasar_commerce.payment_tokens[tok_idx].consumed = true;
    }

    let mut new_bal = snap.unified_plex_balance_for_user(data.vault_uid);
    if data.total_cost_plex > 0 {
        new_bal = snap
            .adjust_unified_plex_for_user(data.vault_uid, -data.total_cost_plex)
            .ok_or(StorePurchaseError::InsufficientPlex {
                required: data.total_cost_plex,
                available: new_bal,
            })?;
    }
    if data.total_plex_credit > 0 {
        new_bal = snap
            .adjust_unified_plex_for_user(data.vault_uid, data.total_plex_credit)
            .ok_or_else(|| {
                StorePurchaseError::Overflow("Account PLEX vault overflows i64".into())
            })?;
    }

    snap.next_entity_id += 1;
    let tx_id = snap.next_entity_id as u64;
    if data.record_plex_tx {
        record_plex_transaction(
            snap,
            tx_id,
            input.buyer_user_id,
            data.net_plex_delta,
            new_bal,
            &data.tx_description,
            input.now_unix,
        );
    }

    let merge_existing = !matches!(input.channel, StorePurchaseChannel::StoreManager);
    let delivered_mutations =
        if let Some((cid, target_station, items_to_deliver)) = data.delivery_target {
            deliver_store_hangar_items_in_snapshot(
                snap,
                cid,
                target_station,
                &items_to_deliver,
                merge_existing,
            )
        } else {
            Vec::new()
        };
    let delivered_item_id = delivered_mutations
        .last()
        .and_then(|(_, _, post)| post.as_ref().map(|r| r.item_id))
        .unwrap_or(0);

    let mut omega_notice = None;
    let mut schedule_id = None;
    if let Some(duration_secs) = data.omega_duration_secs {
        let (new_expiry, notice) =
            crate::services::quasar_auth_user::try_activate_user_omega_license_in_snapshot(
                snap,
                data.omega_target_uid,
                duration_secs,
                input.now_unix,
            );
        if data.create_omega_schedule {
            let interval_days = u32::try_from(duration_secs / 86_400).unwrap_or(u32::MAX);
            schedule_id = Some(upsert_user_payment_schedule_in_snapshot(
                snap,
                data.omega_target_uid,
                1,
                new_expiry,
                &format!("VEVC-{}", data.offer_id),
                interval_days,
                input.now_unix,
            ));
        }
        omega_notice = notice;
    }

    if data.record_receipt {
        snap.quasar_commerce.payment_receipts.push(PaymentReceiptRecord {
            receipt_id: tx_id,
            user_id: input.buyer_user_id,
            offer_id: data.offer_id.to_string(),
            amount_in_cents: data.amount_in_cents,
            currency: data.currency.clone(),
            timestamp_unix: input.now_unix,
        });
    }

    if let Some((map_key, raw_key)) = data.idem_key {
        let record = CommittedStorePurchaseRecord {
            buyer_user_id: input.buyer_user_id,
            channel: input.channel.tag().to_string(),
            idempotency_key: raw_key,
            offer_id: data.offer_id,
            quantity: input.quantity,
            gift_char_id: input.gift_char_id,
            active_char_id: input.active_char_id,
            payment_token_id: data.payment_token_id,
            tx_id,
            plex_spent: data.total_cost_plex,
            plex_credited: data.total_plex_credit,
            is_free_mode_settlement: data.is_free_mode_settlement,
            delivered_item_id,
            delivered_items_count: data.delivered_items_count,
            committed_unix: input.now_unix,
            commit_seq: tx_id,
        };
        insert_bounded_store_purchase_idempotency(
            &mut snap.quasar_commerce.store_purchase_idempotency,
            map_key,
            record,
            MAX_STORE_PURCHASE_IDEMPOTENCY_PER_USER,
            MAX_STORE_PURCHASE_IDEMPOTENCY_ENTRIES,
        );
    }

    Ok(StorePurchaseOutcome {
        tx_id,
        buyer_user_id: input.buyer_user_id,
        plex_spent: data.total_cost_plex,
        plex_credited: data.total_plex_credit,
        net_plex_delta: data.net_plex_delta,
        new_plex_balance: new_bal,
        is_free_mode_settlement: data.is_free_mode_settlement,
        tx_description: data.tx_description,
        omega_target_user_id: data.omega_target_uid,
        omega_notice,
        schedule_id,
        delivered_mutations,
        delivered_item_id,
        delivered_items_count: data.delivered_items_count,
        idempotent_replay: false,
    })
}

/// Validates and atomically executes any store purchase (`PurchaseRequest`, `TokenRequest`,
/// `FastCheckoutService.BuyOffer`, or `storeManager.buy_offer`) inside a mutable snapshot.
pub(crate) fn purchase_store_offer_in_snapshot(
    snap: &mut WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
) -> Result<StorePurchaseOutcome, StorePurchaseError> {
    match prepare_store_purchase_in_snapshot(snap, input, true)? {
        PreparedStorePurchase::IdempotentReplay {
            record,
            new_plex_balance,
        } => Ok(StorePurchaseOutcome {
            tx_id: record.tx_id,
            buyer_user_id: record.buyer_user_id,
            plex_spent: record.plex_spent,
            plex_credited: record.plex_credited,
            net_plex_delta: record.plex_credited - record.plex_spent,
            new_plex_balance,
            is_free_mode_settlement: record.is_free_mode_settlement,
            tx_description: String::new(),
            omega_target_user_id: record.buyer_user_id,
            omega_notice: None,
            schedule_id: None,
            delivered_mutations: Vec::new(),
            delivered_item_id: record.delivered_item_id,
            delivered_items_count: record.delivered_items_count,
            idempotent_replay: true,
        }),
        PreparedStorePurchase::Ready(data) => commit_store_purchase_in_snapshot(snap, input, data),
    }
}

fn raw_and_map_store_idempotency_key(input: &StorePurchaseInput<'_>) -> Option<(String, String)> {
    input.idempotency_key.filter(|k| !k.is_empty()).map(|k| {
        (
            format!("{}:{}:{k}", input.buyer_user_id, input.channel.tag()),
            k.to_string(),
        )
    })
}

pub(crate) fn insert_bounded_store_purchase_idempotency(
    map: &mut BTreeMap<String, CommittedStorePurchaseRecord>,
    map_key: String,
    record: CommittedStorePurchaseRecord,
    max_per_user: usize,
    max_global: usize,
) {
    if !map.contains_key(&map_key) {
        if max_per_user > 0 {
            let mut user_count = map
                .values()
                .filter(|r| r.buyer_user_id == record.buyer_user_id)
                .count();
            while user_count >= max_per_user {
                let Some(oldest_user_key) = map
                    .iter()
                    .filter(|(_, r)| r.buyer_user_id == record.buyer_user_id)
                    .min_by_key(|(_, r)| (r.committed_unix, r.commit_seq))
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                map.remove(&oldest_user_key);
                user_count -= 1;
            }
        }
        if max_global > 0 {
            while map.len() >= max_global {
                let Some(oldest_global_key) = map
                    .iter()
                    .min_by_key(|(_, r)| (r.committed_unix, r.commit_seq))
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                map.remove(&oldest_global_key);
            }
        }
    }
    map.insert(map_key, record);
}

/// Validates committed idempotency replay/conflict first, falling back to live offer
/// availability and character ownership before external password verification.
pub(crate) fn validate_store_purchase_offer_in_snapshot(
    snap: &WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
) -> Result<(), StorePurchaseError> {
    let idem_key = raw_and_map_store_idempotency_key(input);
    if check_store_purchase_idempotency_replay(snap, input, &idem_key)?.is_some() {
        return Ok(());
    }
    resolve_store_purchase_offer(snap, input)?;
    validate_active_char_ownership(snap, input)?;
    Ok(())
}

/// Evaluates `storeManager.can_purchase_offer` using the same live snapshot validation
/// as `purchase_store_offer_in_snapshot`, returning `Ok(false)` when PLEX or delivery station
/// is lacking while propagating invalid/ineligible offer or quantity errors.
pub(crate) fn can_purchase_store_offer_in_snapshot(
    snap: &WorldStateSnapshot,
    input: &StorePurchaseInput<'_>,
) -> Result<bool, StorePurchaseError> {
    match prepare_store_purchase_in_snapshot(snap, input, false) {
        Ok(_) => Ok(true),
        Err(StorePurchaseError::InsufficientPlex { .. })
        | Err(StorePurchaseError::MissingDeliveryStation(_)) => Ok(false),
        Err(err) => Err(err),
    }
}
