//! `quasar_cosmetics_market` — state-backed V24.01 Quasar handlers for the Paragon Hub
//! Cosmetic Market (ship SKIN listings, SKINR design component listings, fee/tax quotes,
//! and asynchronous transaction notices).
//!
//! Wire layouts follow the V24.01 client descriptors:
//! - `eve_public.cosmetic.market.api` (`GetBrokerFeesRequest`, `GetTaxRateRequest`,
//!   `GetBuyerFeeRequest`, `GetBuyerFeesDefinitionRequest`);
//! - `eve_public.cosmetic.market.component.listing.api` (`GetAllRequest`,
//!   `InitiatePurchaseListingRequest`);
//! - `eve_public.cosmetic.market.skin.listing.api` (`GetAllRequest`, `GetAllOwnedRequest`,
//!   `GetRequest`, `InitiateCreateListingRequest`, `CancelListingRequest`,
//!   `InitiatePurchaseListingRequest`);
//! - `eve_public.cosmetic.market.skin.listing.api.admin` (`SetExpirationRequest`);
//! - `eve_public.cosmetic.market.transaction.api` (`TransactionSucceededNotice`,
//!   `TransactionFailedNotice`).

#![forbid(unsafe_code)]

use super::quasar_cosmetics_market_types::{
    GROUP_SHIP_SKIN_DESIGN_ELEMENT, LISTING_DURATIONS_SECS,
};
pub use super::quasar_cosmetics_market_types::{
    ComponentListingRecord, CosmeticsMarketData, ListingTargetRecord, SkinListingRecord,
    BUYER_FEES_VERSION, MAX_LISTING_PRICE_UNITS, MAX_LISTING_QUANTITY, PARAGON_CORP_ID,
};
use super::quasar_cosmetics_skinr::{SkinrCosmeticsState, ThirdPartySkinLicense};
use super::quasar_social_chat::ctx_queue_notice;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, RwLock};

const BLOB_KEY: &str = "quasar_cosmetics_market";

type HandlerResult = Result<GoogleAny, (u32, String)>;

pub struct CosmeticsMarketState {
    db: Arc<WorldRepository>,
    pub data: RwLock<CosmeticsMarketData>,
    pub skinr: Arc<SkinrCosmeticsState>,
}

impl CosmeticsMarketState {
    pub fn load(
        db: Arc<WorldRepository>,
        _sde: Arc<SdeRegistry>,
        skinr: Arc<SkinrCosmeticsState>,
    ) -> Arc<Self> {
        let data: CosmeticsMarketData = db.with_snapshot(|s| {
            s.quasar_state_blobs
                .get(BLOB_KEY)
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default()
        });
        Arc::new(Self {
            db,
            data: RwLock::new(data),
            skinr,
        })
    }

    fn save(&self) {
        let json = serde_json::to_string(&*self.data.read().unwrap_or_else(|e| e.into_inner()));
        if let Ok(json) = json {
            self.db.with_mut_snapshot(|s| {
                s.quasar_state_blobs.insert(BLOB_KEY.into(), json);
            });
        }
    }

    fn sync_from_blob(&self) {
        let needs_sync = self
            .data
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .component_listings
            .is_empty();
        if !needs_sync {
            return;
        }
        let persisted: Option<CosmeticsMarketData> = self.db.with_snapshot(|s| {
            s.quasar_state_blobs
                .get(BLOB_KEY)
                .and_then(|j| serde_json::from_str(j).ok())
        });
        if let Some(p) = persisted {
            if !p.component_listings.is_empty() {
                let mut data = self.data.write().unwrap_or_else(|e| e.into_inner());
                for (k, v) in p.component_listings {
                    data.component_listings.entry(k).or_insert(v);
                }
                if data.next_seq < p.next_seq {
                    data.next_seq = p.next_seq;
                }
            }
        }
    }

    pub fn skin_tier_for_hex(&self, skin_hex: &str) -> Option<u32> {
        let data = self.skinr.data.read().unwrap_or_else(|e| e.into_inner());
        if let Some(skin) = data.skins.get(skin_hex) {
            if skin.tier_level > 0 {
                return Some(skin.tier_level);
            }
        }
        drop(data);
        self.db.with_snapshot(|s| {
            s.quasar_state_blobs
                .get(super::quasar_cosmetics_skinr::SKINR_BLOB_KEY)
                .and_then(|j| {
                    serde_json::from_str::<super::quasar_cosmetics_skinr::SkinrCosmeticsData>(j)
                        .ok()
                })
                .and_then(|d| d.skins.get(skin_hex).map(|sk| sk.tier_level))
                .filter(|&t| t > 0)
        })
    }
}

pub fn seed_component_listings_if_empty(data: &mut CosmeticsMarketData, sde: &SdeRegistry) -> bool {
    if !data.component_listings.is_empty() {
        return false;
    }
    let mut design_types: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.group_id == GROUP_SHIP_SKIN_DESIGN_ELEMENT && t.published)
        .map(|t| t.type_id)
        .collect();
    design_types.sort_unstable();
    if design_types.len() < 4 {
        let mut fallback: Vec<u32> = sde.skin_licenses.keys().copied().collect();
        fallback.sort_unstable();
        design_types.extend(fallback.into_iter().take(4));
    }
    while design_types.len() < 4 {
        design_types.push(82_340 + design_types.len() as u32);
    }
    let templates = [
        (design_types[0], Some(250_000u64), None, 1u64, 0u32),
        (design_types[1], Some(1_000_000u64), None, 5u64, 20u32),
        (design_types[2], None, Some(1_000u64), 1u64, 0u32),
        (design_types[3], None, Some(4_000u64), 5u64, 20u32),
    ];
    for (idx, (type_id, price_isk, price_plex_cents, bundle_size, discount_percent)) in
        templates.into_iter().enumerate()
    {
        let seq = (idx as u64) + 1;
        let mut uuid = [0u8; 16];
        uuid[0..8].copy_from_slice(b"COMPLIST");
        uuid[8..16].copy_from_slice(&seq.to_be_bytes());
        let rec = ComponentListingRecord {
            uuid,
            sale_end_unix: 4_102_444_800,
            price_isk,
            price_plex_cents,
            component_item_type_id: type_id,
            bundle_size,
            discount_percent,
        };
        data.component_listings.insert(hex(&uuid), rec);
    }
    if data.next_seq < 10 {
        data.next_seq = 10;
    }
    true
}

fn hex(u: &[u8]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

fn put_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_seconds(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, secs, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_uuid(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_isk(field: u32, units: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, units, &mut b);
    encode_varint_field(2, 0, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_plex_cents(field: u32, cents: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, cents, &mut b);
    encode_bytes_field(field, &b, out);
}

fn encode_sint_field(field: u32, v: i64, out: &mut Vec<u8>) {
    encode_varint_field(field, ((v << 1) ^ (v >> 63)) as u64, out);
}

fn put_buyer_fee(field: u32, lp_amount: u64, out: &mut Vec<u8>) {
    let mut lp = Vec::new();
    put_seq(1, PARAGON_CORP_ID, &mut lp);
    encode_varint_field(2, lp_amount, &mut lp);
    let mut fee = Vec::new();
    encode_bytes_field(1, &lp, &mut fee);
    encode_bytes_field(field, &fee, out);
}

fn as_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int64(i) | DynamicValue::Sint64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) | DynamicValue::Sint32(i) => u64::try_from(*i).ok(),
        _ => None,
    }
}

fn checked_isk_cents(isk_units: u64) -> Option<IskCents> {
    if isk_units > MAX_LISTING_PRICE_UNITS {
        return None;
    }
    let cents_u64 = isk_units.checked_mul(100)?;
    let cents_i64 = i64::try_from(cents_u64).ok()?;
    Some(IskCents(cents_i64))
}


fn as_bool(v: &DynamicValue) -> Option<bool> {
    match v {
        DynamicValue::Bool(b) => Some(*b),
        DynamicValue::Uint64(u) => Some(*u != 0),
        DynamicValue::Uint32(u) => Some(*u != 0),
        DynamicValue::Int64(i) => Some(*i != 0),
        DynamicValue::Int32(i) => Some(*i != 0),
        _ => None,
    }
}

fn as_str(v: &DynamicValue) -> Option<&str> {
    match v {
        DynamicValue::String(s) => Some(s.as_str()),
        DynamicValue::Bytes(b) => std::str::from_utf8(b).ok(),
        _ => None,
    }
}

fn sub(msg: &DynamicQuasarMessage, field: u32) -> Option<&DynamicQuasarMessage> {
    match msg.get(field)? {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

fn seq_of(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    sub(msg, field).and_then(|m| m.get(1)).and_then(as_u64)
}

fn uuid_of(msg: &DynamicQuasarMessage, field: u32) -> Option<[u8; 16]> {
    match sub(msg, field)?.get(1)? {
        DynamicValue::Bytes(b) if b.len() == 16 => b.as_slice().try_into().ok(),
        DynamicValue::String(s) if s.len() == 16 => s.as_bytes().try_into().ok(),
        _ => None,
    }
}

fn require_char(ctx: &AuthoritativeContext) -> Result<u32, (u32, String)> {
    ctx.effective_character_id()
        .ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".to_string()))
}

fn parse_listing_target(msg: Option<&DynamicQuasarMessage>) -> ListingTargetRecord {
    let Some(m) = msg else {
        return ListingTargetRecord::Public;
    };
    if let Some(corp) = seq_of(m, 2) {
        return ListingTargetRecord::Corporation(corp);
    }
    if let Some(ally) = seq_of(m, 3) {
        return ListingTargetRecord::Alliance(ally);
    }
    if let Some(ch) = seq_of(m, 4) {
        return ListingTargetRecord::Character(ch);
    }
    ListingTargetRecord::Public
}

fn encode_listing_target(target: &ListingTargetRecord, out: &mut Vec<u8>) {
    match target {
        ListingTargetRecord::Public => encode_varint_field(1, 1, out),
        ListingTargetRecord::Corporation(id) => put_seq(2, *id, out),
        ListingTargetRecord::Alliance(id) => put_seq(3, *id, out),
        ListingTargetRecord::Character(id) => put_seq(4, *id, out),
    }
}

fn encode_component_entry(rec: &ComponentListingRecord) -> Vec<u8> {
    let mut attrs = Vec::new();
    put_seconds(2, rec.sale_end_unix, &mut attrs);
    if let Some(isk) = rec.price_isk {
        put_isk(3, isk, &mut attrs);
    } else if let Some(plex_cents) = rec.price_plex_cents {
        put_plex_cents(4, plex_cents, &mut attrs);
    }
    put_seq(5, rec.component_item_type_id as u64, &mut attrs);
    encode_varint_field(6, rec.bundle_size, &mut attrs);
    encode_varint_field(7, rec.discount_percent as u64, &mut attrs);

    let mut entry = Vec::new();
    put_uuid(1, &rec.uuid, &mut entry);
    encode_bytes_field(2, &attrs, &mut entry);
    entry
}

fn encode_skin_entry(rec: &SkinListingRecord) -> Vec<u8> {
    let mut attrs = Vec::new();
    put_seq(2, rec.seller_id as u64, &mut attrs);
    put_seconds(3, rec.created_unix, &mut attrs);
    put_seconds(4, rec.expires_unix, &mut attrs);
    if let Some(isk) = rec.price_isk {
        put_isk(5, isk, &mut attrs);
    } else if let Some(plex_cents) = rec.price_plex_cents {
        put_plex_cents(6, plex_cents, &mut attrs);
    }
    let mut skin_id = Vec::new();
    encode_string_field(1, &rec.skin_hex, &mut skin_id);
    let mut lic_id = Vec::new();
    put_seq(1, rec.seller_id as u64, &mut lic_id);
    encode_bytes_field(2, &skin_id, &mut lic_id);
    encode_bytes_field(7, &lic_id, &mut attrs);
    encode_varint_field(8, rec.quantity as u64, &mut attrs);
    let mut target_buf = Vec::new();
    encode_listing_target(&rec.target, &mut target_buf);
    encode_bytes_field(9, &target_buf, &mut attrs);
    encode_varint_field(10, u64::from(rec.branded), &mut attrs);

    let mut entry = Vec::new();
    put_uuid(1, &rec.uuid, &mut entry);
    encode_bytes_field(2, &attrs, &mut entry);
    entry
}

fn queue_transaction_notice(cid: u32, tx_uuid: &[u8; 16], succeeded: bool, now_unix: u64) {
    let mut payload = Vec::new();
    put_uuid(1, tx_uuid, &mut payload);
    let type_name = if succeeded {
        "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"
    } else {
        "eve_public.cosmetic.market.transaction.api.TransactionFailedNotice"
    };
    ctx_queue_notice(
        NoticeScope::Character(cid),
        any(type_name, payload),
        now_unix,
    );
}

fn next_uuid(data: &mut CosmeticsMarketData, prefix: &[u8; 8]) -> [u8; 16] {
    data.next_seq += 1;
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(prefix);
    uuid[8..16].copy_from_slice(&data.next_seq.to_be_bytes());
    uuid
}

fn broker_fee_for(duration_secs: u64, target: &ListingTargetRecord) -> u64 {
    let base = LISTING_DURATIONS_SECS
        .iter()
        .find(|(d, _)| *d == duration_secs)
        .map(|(_, fee)| *fee)
        .unwrap_or_else(|| ((duration_secs.max(86_400) / 86_400) * 50_000).max(100_000));
    if target.is_branded() {
        base / 2
    } else {
        base
    }
}

fn market_get_broker_fees(
    _st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let _cid = require_char(ctx)?;
    let target = parse_listing_target(sub(msg, 1));
    let mut out = Vec::new();
    for (secs, _) in LISTING_DURATIONS_SECS {
        let fee_isk = broker_fee_for(secs, &target);
        let mut bf = Vec::new();
        put_seconds(1, secs, &mut bf);
        put_isk(2, fee_isk, &mut bf);
        encode_bytes_field(1, &bf, &mut out);

        let mut fpd = Vec::new();
        put_seconds(1, secs, &mut fpd);
        encode_bytes_field(2, &bf, &mut fpd);
        encode_bytes_field(2, &fpd, &mut out);
    }
    Ok(any(
        "eve_public.cosmetic.market.api.GetBrokerFeesResponse",
        out,
    ))
}

fn market_get_tax_rate(
    _st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let _cid = require_char(ctx)?;
    let target = parse_listing_target(sub(msg, 1));
    let rate_units: i64 = if target.is_branded() { 10 } else { 20 };
    let mut tr = Vec::new();
    encode_sint_field(1, rate_units, &mut tr);
    encode_sint_field(2, 0, &mut tr);
    let mut out = Vec::new();
    encode_bytes_field(1, &tr, &mut out);
    Ok(any(
        "eve_public.cosmetic.market.api.GetTaxRateResponse",
        out,
    ))
}

fn market_get_buyer_fee(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let _cid = require_char(ctx)?;
    let target = parse_listing_target(sub(msg, 1));
    let skin_hex = sub(msg, 2)
        .and_then(|m| m.get(1))
        .and_then(as_str)
        .unwrap_or("");
    if skin_hex.is_empty() {
        return Err((STATUS_BAD_REQUEST, "Empty skin identifier".to_string()));
    }
    let Some(tier_u32) = st.skin_tier_for_hex(skin_hex) else {
        return Err((
            STATUS_NOT_FOUND,
            format!("SKIN design '{skin_hex}' not found"),
        ));
    };
    let tier = u64::from(tier_u32);
    let lp_amount = if target.is_branded() {
        tier * 2_500
    } else {
        tier * 1_000
    };
    let mut out = Vec::new();
    put_buyer_fee(1, lp_amount, &mut out);
    Ok(any(
        "eve_public.cosmetic.market.api.GetBuyerFeeResponse",
        out,
    ))
}

fn market_get_buyer_fees_definition(
    _st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    _msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let _cid = require_char(ctx)?;
    let mut out = Vec::new();
    for level in 1u64..=5 {
        let mut fpt = Vec::new();
        put_seq(1, level, &mut fpt);
        put_buyer_fee(2, level * 1_000, &mut fpt);
        put_buyer_fee(3, level * 2_500, &mut fpt);
        encode_bytes_field(1, &fpt, &mut out);
    }
    encode_string_field(2, BUYER_FEES_VERSION, &mut out);
    Ok(any(
        "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionResponse",
        out,
    ))
}

fn component_get_all(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    _msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let _cid = require_char(ctx)?;
    st.sync_from_blob();
    let now = ctx.now_unix();
    let listings: Vec<ComponentListingRecord> = st
        .data
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .component_listings
        .values()
        .filter(|l| l.sale_end_unix > now)
        .cloned()
        .collect();
    let mut out = Vec::new();
    for rec in &listings {
        encode_bytes_field(1, &encode_component_entry(rec), &mut out);
    }
    Ok(any(
        "eve_public.cosmetic.market.component.listing.api.GetAllResponse",
        out,
    ))
}

fn component_initiate_purchase(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    st.sync_from_blob();
    let uuid = uuid_of(msg, 1)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing component listing UUID".to_string()))?;
    let quantity_u64 = msg.get(2).and_then(as_u64).unwrap_or(0);
    if quantity_u64 == 0 || quantity_u64 > u64::from(MAX_LISTING_QUANTITY) {
        return Err((
            STATUS_BAD_REQUEST,
            format!("Purchase quantity must be in 1..={MAX_LISTING_QUANTITY}"),
        ));
    }
    let immediately_activate = msg.get(3).and_then(as_bool).unwrap_or(false);
    let now = ctx.now_unix();
    let (listing, tx_uuid) = {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(l) = data.component_listings.get(&hex(&uuid)).cloned() else {
            return Err((STATUS_NOT_FOUND, "Component listing not found".to_string()));
        };
        if l.sale_end_unix <= now {
            return Err((STATUS_BAD_REQUEST, "Component listing has expired".to_string()));
        }
        let tx = next_uuid(&mut data, b"COSMTXID");
        (l, tx)
    };

    let granted_qty_u64 = listing
        .bundle_size
        .checked_mul(quantity_u64)
        .filter(|&q| q > 0)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Component bundle quantity overflow".to_string()))?;
    let granted_qty = i32::try_from(granted_qty_u64)
        .map_err(|_| (STATUS_BAD_REQUEST, "Component bundle quantity exceeds i32::MAX".to_string()))?;
    let checked_isk_total = match listing.price_isk {
        Some(isk_unit) => {
            if isk_unit > MAX_LISTING_PRICE_UNITS {
                return Err((STATUS_BAD_REQUEST, "Component ISK price exceeds maximum safe bounds".to_string()));
            }
            Some(isk_unit.checked_mul(quantity_u64).and_then(checked_isk_cents)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Component ISK total overflows".to_string()))?)
        }
        None => None,
    };
    let checked_plex_total = match listing.price_plex_cents {
        Some(plex_cents) => {
            let cents = plex_cents
                .checked_mul(quantity_u64)
                .filter(|&c| c <= MAX_LISTING_PRICE_UNITS)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Component PLEX total overflows".to_string()))?;
            let units = cents
                .checked_add(99)
                .map(|v| v / 100)
                .and_then(|v| i64::try_from(v).ok())
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Component PLEX units overflow".to_string()))?;
            Some(units)
        }
        None => None,
    };
    st.save();

    let succeeded = st.db.with_mut_snapshot(|s| {
        let Some(ch) = s.characters.get_mut(&(cid as i64)) else {
            return false;
        };
        if let Some(total_cost) = checked_isk_total {
            let Some(rem) = ch.balance.checked_sub(total_cost) else {
                return false;
            };
            ch.balance = rem;
        } else if let Some(total_plex) = checked_plex_total {
            if ch.plex_balance < total_plex {
                return false;
            }
            let Some(rem_plex) = ch.plex_balance.checked_sub(total_plex) else {
                return false;
            };
            ch.plex_balance = rem_plex;
        }
        let location_id = ch
            .station_id
            .or(ch.structure_id)
            .unwrap_or(ch.home_station_id.max(60_003_760));
        let item_id = s
            .items
            .keys()
            .copied()
            .max()
            .unwrap_or(990_000_000)
            .max(990_000_000)
            + 1;
        s.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: listing.component_item_type_id,
                owner_id: cid as i64,
                location_id,
                flag: InventoryFlag::Hangar,
                quantity: granted_qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        true
    });

    queue_transaction_notice(cid, &tx_uuid, succeeded, now);
    let mut out = Vec::new();
    put_uuid(1, &tx_uuid, &mut out);
    encode_varint_field(2, u64::from(immediately_activate), &mut out);
    Ok(any(
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingResponse",
        out,
    ))
}

fn skin_visible_to(
    rec: &SkinListingRecord,
    cid: u32,
    corp_id: u32,
    alliance_id: Option<u32>,
    filter_target: Option<&ListingTargetRecord>,
) -> bool {
    if let Some(ft) = filter_target {
        if &rec.target != ft {
            return false;
        }
    }
    if rec.seller_id == cid {
        return true;
    }
    match rec.target {
        ListingTargetRecord::Public => true,
        ListingTargetRecord::Character(t) => t == u64::from(cid),
        ListingTargetRecord::Corporation(t) => t == u64::from(corp_id),
        ListingTargetRecord::Alliance(t) => alliance_id.is_some_and(|a| u64::from(a) == t),
    }
}

fn skin_get_all(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let now = ctx.now_unix();
    let (corp_id, alliance_id) = st.db.with_snapshot(|s| {
        s.characters
            .get(&(cid as i64))
            .map(|c| (c.corporation_id as u32, c.alliance_id.map(|a| a as u32)))
            .unwrap_or((1_000_009, None))
    });
    let filter_target = sub(msg, 4).map(|m| parse_listing_target(Some(m)));
    let listings: Vec<SkinListingRecord> = st
        .data
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .skin_listings
        .values()
        .filter(|l| {
            l.expires_unix > now
                && l.quantity > 0
                && skin_visible_to(l, cid, corp_id, alliance_id, filter_target.as_ref())
        })
        .cloned()
        .collect();
    let mut out = Vec::new();
    for rec in &listings {
        encode_bytes_field(1, &encode_skin_entry(rec), &mut out);
    }
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.GetAllResponse",
        out,
    ))
}

fn skin_get_all_owned(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    _msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let listings: Vec<SkinListingRecord> = st
        .data
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .skin_listings
        .values()
        .filter(|l| l.seller_id == cid && l.quantity > 0)
        .cloned()
        .collect();
    let mut out = Vec::new();
    for rec in &listings {
        encode_bytes_field(1, &encode_skin_entry(rec), &mut out);
    }
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedResponse",
        out,
    ))
}

fn skin_get(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let uuid = uuid_of(msg, 1)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skin listing UUID".to_string()))?;
    let rec = st
        .data
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .skin_listings
        .get(&hex(&uuid))
        .cloned()
        .ok_or_else(|| (STATUS_NOT_FOUND, "Skin listing not found".to_string()))?;
    let (corp_id, alliance_id) = st.db.with_snapshot(|s| {
        s.characters
            .get(&(cid as i64))
            .map(|c| {
                let corp = c.corporation_id as u32;
                let al = s
                    .corporations
                    .get(&c.corporation_id)
                    .and_then(|cr| cr.alliance_id)
                    .map(|a| a as u32);
                (corp, al)
            })
            .unwrap_or((1_000_009, None))
    });
    if !skin_visible_to(&rec, cid, corp_id, alliance_id, None)
        && crate::quasar_gateway::require_admin_role(ctx).is_err()
    {
        return Err((
            STATUS_FORBIDDEN,
            "Caller is not eligible for this targeted SKIN listing".to_string(),
        ));
    }
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_skin_entry(&rec), &mut out);
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.GetResponse",
        out,
    ))
}

fn extract_skin_hex(lic_msg: Option<&DynamicQuasarMessage>, cid: u32) -> Result<String, (u32, String)> {
    let lic = lic_msg.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skin license identifier".to_string()))?;
    if let Some(owner_seq) = seq_of(lic, 1) {
        if owner_seq != 0 && owner_seq != u64::from(cid) {
            return Err((
                STATUS_FORBIDDEN,
                "Cannot list a SKIN license owned by another character".to_string(),
            ));
        }
    }
    let hex_str = sub(lic, 2)
        .and_then(|s| s.get(1))
        .and_then(as_str)
        .or_else(|| lic.get(1).and_then(as_str))
        .unwrap_or("");
    if hex_str.is_empty() {
        return Err((STATUS_BAD_REQUEST, "Missing skin hex identifier".to_string()));
    }
    Ok(hex_str.to_string())
}

fn skin_initiate_create(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let price_isk = sub(msg, 1).and_then(|m| m.get(1)).and_then(as_u64).filter(|v| *v > 0);
    let price_plex_cents = sub(msg, 2).and_then(|m| m.get(1)).and_then(as_u64).filter(|v| *v > 0);
    if price_isk.is_none() && price_plex_cents.is_none() {
        return Err((
            STATUS_BAD_REQUEST,
            "Listing must specify a positive ISK or PLEX price".to_string(),
        ));
    }
    let skin_hex = extract_skin_hex(sub(msg, 3), cid)?;
    let duration_secs = sub(msg, 4).and_then(|m| m.get(1)).and_then(as_u64).unwrap_or(0);
    if duration_secs == 0 {
        return Err((
            STATUS_BAD_REQUEST,
            "Listing duration must be positive".to_string(),
        ));
    }
    let quantity_u64 = msg.get(5).and_then(as_u64).unwrap_or(0);
    if quantity_u64 == 0 || quantity_u64 > u64::from(MAX_LISTING_QUANTITY) {
        return Err((
            STATUS_BAD_REQUEST,
            format!("Listing quantity must be in 1..={MAX_LISTING_QUANTITY}"),
        ));
    }
    let quantity = quantity_u64 as u32;
    if let Some(isk_unit) = price_isk {
        if isk_unit > MAX_LISTING_PRICE_UNITS
            || isk_unit.checked_mul(quantity_u64).and_then(checked_isk_cents).is_none()
        {
            return Err((
                STATUS_BAD_REQUEST,
                "Listing ISK price exceeds maximum safe bounds".to_string(),
            ));
        }
    }
    if let Some(plex_cents) = price_plex_cents {
        if plex_cents > MAX_LISTING_PRICE_UNITS
            || plex_cents
                .checked_mul(quantity_u64)
                .filter(|&v| v <= MAX_LISTING_PRICE_UNITS)
                .is_none()
        {
            return Err((
                STATUS_BAD_REQUEST,
                "Listing PLEX price exceeds maximum safe bounds".to_string(),
            ));
        }
    }
    let target = parse_listing_target(sub(msg, 6));
    let broker_fee = checked_isk_cents(broker_fee_for(duration_secs, &target))
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Broker fee overflows".to_string()))?;
    let now = ctx.now_unix();

    let tx_uuid = {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        let mut skinr_data = st.skinr.data.write().unwrap_or_else(|e| e.into_inner());
        let lic_key = SkinrCosmeticsState::skin_lic_key(cid, &skin_hex);
        let avail = skinr_data
            .skin_licenses
            .get(&lic_key)
            .map(|l| u32::try_from(l.unactivated).unwrap_or(u32::MAX))
            .unwrap_or(0);
        if avail < quantity {
            return Err((
                STATUS_PRECONDITION_FAILED,
                "Insufficient unactivated SKIN licenses for listing".to_string(),
            ));
        }
        let tx = next_uuid(&mut data, b"COSMTXID");
        let fee_paid = st.db.with_mut_snapshot(|s| {
            let Some(ch) = s.characters.get_mut(&(cid as i64)) else {
                return false;
            };
            let Some(rem) = ch.balance.checked_sub(broker_fee) else {
                return false;
            };
            ch.balance = rem;
            true
        });
        if !fee_paid {
            drop(skinr_data);
            drop(data);
            st.save();
            queue_transaction_notice(cid, &tx, false, now);
            let mut out = Vec::new();
            put_uuid(1, &tx, &mut out);
            return Ok(any(
                "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingResponse",
                out,
            ));
        }
        if let Some(lic) = skinr_data.skin_licenses.get_mut(&lic_key) {
            lic.unactivated = lic.unactivated.saturating_sub(u64::from(quantity));
            lic.escrowed = lic.escrowed.saturating_add(u64::from(quantity));
        }
        let listing_uuid = next_uuid(&mut data, b"SKINLIST");
        let rec = SkinListingRecord {
            uuid: listing_uuid,
            seller_id: cid,
            created_unix: now,
            expires_unix: now.saturating_add(duration_secs),
            price_isk,
            price_plex_cents,
            skin_hex: skin_hex.clone(),
            quantity,
            branded: target.is_branded(),
            target,
        };
        data.skin_listings.insert(hex(&listing_uuid), rec);
        tx
    };
    st.skinr.save();
    st.save();
    queue_transaction_notice(cid, &tx_uuid, true, now);
    let mut out = Vec::new();
    put_uuid(1, &tx_uuid, &mut out);
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingResponse",
        out,
    ))
}

fn skin_cancel_listing(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let uuid = uuid_of(msg, 1)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skin listing UUID".to_string()))?;
    let key = hex(&uuid);
    let removed = {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(existing) = data.skin_listings.get(&key) else {
            return Err((STATUS_NOT_FOUND, "Skin listing not found".to_string()));
        };
        if existing.seller_id != cid {
            return Err((
                STATUS_FORBIDDEN,
                "Only the seller may cancel a SKIN listing".to_string(),
            ));
        }
        data.skin_listings.remove(&key)
    };
    if let Some(removed) = removed {
        {
            let mut skinr_data = st.skinr.data.write().unwrap_or_else(|e| e.into_inner());
            let lic_key = SkinrCosmeticsState::skin_lic_key(cid, &removed.skin_hex);
            if let Some(lic) = skinr_data.skin_licenses.get_mut(&lic_key) {
                lic.unactivated = lic.unactivated.saturating_add(u64::from(removed.quantity));
                lic.escrowed = lic.escrowed.saturating_sub(u64::from(removed.quantity));
            }
        }
        st.skinr.save();
        st.save();
    }
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.CancelListingResponse",
        Vec::new(),
    ))
}

fn skin_initiate_purchase(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let uuid = uuid_of(msg, 1)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skin listing UUID".to_string()))?;
    let quantity_u64 = msg.get(2).and_then(as_u64).unwrap_or(0);
    if quantity_u64 == 0 || quantity_u64 > u64::from(MAX_LISTING_QUANTITY) {
        return Err((
            STATUS_BAD_REQUEST,
            format!("Purchase quantity must be in 1..={MAX_LISTING_QUANTITY}"),
        ));
    }
    let quantity = quantity_u64 as u32;
    if let Some(ver) = msg.get(3).and_then(as_str) {
        if !ver.is_empty() && ver != BUYER_FEES_VERSION {
            return Err((
                STATUS_BAD_REQUEST,
                format!("Outdated buyer_fees_version {ver}; expected {BUYER_FEES_VERSION}"),
            ));
        }
    }
    let now = ctx.now_unix();
    let (corp_id, alliance_id) = st.db.with_snapshot(|s| {
        s.characters
            .get(&(cid as i64))
            .map(|c| (c.corporation_id as u32, c.alliance_id.map(|a| a as u32)))
            .unwrap_or((1_000_009, None))
    });
    let key = hex(&uuid);
    let (listing, tx_uuid) = {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(l) = data.skin_listings.get(&key).cloned() else {
            return Err((STATUS_NOT_FOUND, "Skin listing not found".to_string()));
        };
        if l.expires_unix <= now {
            return Err((STATUS_BAD_REQUEST, "Skin listing has expired".to_string()));
        }
        if quantity > l.quantity {
            return Err((
                STATUS_BAD_REQUEST,
                "Requested quantity exceeds listing quantity".to_string(),
            ));
        }
        if !skin_visible_to(&l, cid, corp_id, alliance_id, None) {
            return Err((
                STATUS_FORBIDDEN,
                "Caller is not eligible for this targeted SKIN listing".to_string(),
            ));
        }
        if let Some(l_mut) = data.skin_listings.get_mut(&key) {
            l_mut.quantity = l_mut.quantity.saturating_sub(quantity);
            if l_mut.quantity == 0 {
                data.skin_listings.remove(&key);
            }
        }
        let tx = next_uuid(&mut data, b"COSMTXID");
        (l, tx)
    };
    let rollback_listing_qty = |st_ref: &CosmeticsMarketState, orig: &SkinListingRecord, qty: u32| {
        let mut data = st_ref.data.write().unwrap_or_else(|e| e.into_inner());
        if let Some(l_mut) = data.skin_listings.get_mut(&key) {
            l_mut.quantity = l_mut.quantity.saturating_add(qty);
        } else {
            let mut restored = orig.clone();
            restored.quantity = qty;
            data.skin_listings.insert(key.clone(), restored);
        }
    };

    let tax_pct: u64 = if listing.branded { 10 } else { 20 };
    let Some(tier_u32) = st.skin_tier_for_hex(&listing.skin_hex) else {
        rollback_listing_qty(st, &listing, quantity);
        return Err((
            STATUS_NOT_FOUND,
            format!("SKIN design '{}' not found", listing.skin_hex),
        ));
    };
    let tier = u64::from(tier_u32);
    let fee_per_unit = if listing.branded { tier * 2_500 } else { tier * 1_000 };
    let Some(buyer_lp_fee) = fee_per_unit
        .checked_mul(quantity_u64)
        .and_then(|v| i64::try_from(v).ok())
    else {
        rollback_listing_qty(st, &listing, quantity);
        return Err((STATUS_BAD_REQUEST, "Buyer EverMark fee overflows".to_string()));
    };

    let buyer_lp = st.db.with_snapshot(|s| {
        s.characters.get(&(cid as i64)).and_then(|c| c.loyalty_points.get(&(PARAGON_CORP_ID as i64)).copied()).unwrap_or(0)
    });
    if buyer_lp < buyer_lp_fee {
        rollback_listing_qty(st, &listing, quantity);
        queue_transaction_notice(cid, &tx_uuid, false, now);
        return Err((STATUS_PRECONDITION_FAILED, "Insufficient EverMarks for buyer fee".to_string()));
    }

    let checked_isk_settlement = match listing.price_isk {
        Some(isk_unit) => {
            let calc = (|| -> Result<(IskCents, IskCents), (u32, String)> {
                if isk_unit > MAX_LISTING_PRICE_UNITS {
                    return Err((STATUS_BAD_REQUEST, "Listing ISK price exceeds maximum safe bounds".to_string()));
                }
                let total_isk = isk_unit
                    .checked_mul(quantity_u64)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Total ISK price overflows".to_string()))?;
                let total_cost = checked_isk_cents(total_isk)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Total ISK price exceeds limit".to_string()))?;
                let tax_isk = total_isk
                    .checked_mul(tax_pct)
                    .map(|v| v / 100)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "ISK tax calculation overflows".to_string()))?;
                let net_isk = total_isk
                    .checked_sub(tax_isk)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "ISK net calculation underflows".to_string()))?;
                let net_credit = checked_isk_cents(net_isk)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Net ISK credit exceeds limit".to_string()))?;
                Ok((total_cost, net_credit))
            })();
            match calc {
                Ok(v) => Some(v),
                Err(e) => {
                    rollback_listing_qty(st, &listing, quantity);
                    return Err(e);
                }
            }
        }
        None => None,
    };
    let checked_plex_settlement = match listing.price_plex_cents {
        Some(plex_cents) => {
            let calc = (|| -> Result<(i64, i64), (u32, String)> {
                let total_cents = plex_cents
                    .checked_mul(quantity_u64)
                    .filter(|&c| c <= MAX_LISTING_PRICE_UNITS)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Total PLEX price overflows".to_string()))?;
                let total_plex = total_cents
                    .checked_add(99)
                    .map(|v| v / 100)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Total PLEX units overflow".to_string()))?;
                let total_plex_i64 = i64::try_from(total_plex)
                    .map_err(|_| (STATUS_BAD_REQUEST, "Total PLEX units exceed i64".to_string()))?;
                let tax_plex = total_plex
                    .checked_mul(tax_pct)
                    .map(|v| v / 100)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "PLEX tax calculation overflows".to_string()))?;
                let net_plex_i64 = i64::try_from(total_plex.saturating_sub(tax_plex))
                    .map_err(|_| (STATUS_BAD_REQUEST, "Net PLEX credit exceeds i64".to_string()))?;
                Ok((total_plex_i64, net_plex_i64))
            })();
            match calc {
                Ok(v) => Some(v),
                Err(e) => {
                    rollback_listing_qty(st, &listing, quantity);
                    return Err(e);
                }
            }
        }
        None => None,
    };

    let settled = st.db.with_mut_snapshot(|s| {
        let Some(buyer) = s.characters.get(&(cid as i64)) else {
            return false;
        };
        let cur_lp = buyer
            .loyalty_points
            .get(&(PARAGON_CORP_ID as i64))
            .copied()
            .unwrap_or(0);
        if cur_lp < buyer_lp_fee {
            return false;
        }
        if let Some((total_cost, net_credit)) = checked_isk_settlement {
            if buyer.balance.checked_sub(total_cost).is_none() {
                return false;
            }
            if let Some(seller) = s.characters.get(&(listing.seller_id as i64)) {
                if seller.balance.checked_add(net_credit).is_none() {
                    return false;
                }
            }
            if let Some(b) = s.characters.get_mut(&(cid as i64)) {
                b.balance = b.balance.checked_sub(total_cost).unwrap_or(b.balance);
                if let Some(lp) = b.loyalty_points.get_mut(&(PARAGON_CORP_ID as i64)) {
                    *lp -= buyer_lp_fee;
                }
            }
            if let Some(seller) = s.characters.get_mut(&(listing.seller_id as i64)) {
                seller.balance = seller.balance.checked_add(net_credit).unwrap_or(seller.balance);
            }
        } else if let Some((total_plex, net_plex)) = checked_plex_settlement {
            if buyer.plex_balance < total_plex {
                return false;
            }
            if let Some(seller) = s.characters.get(&(listing.seller_id as i64)) {
                if seller.plex_balance.checked_add(net_plex).is_none() {
                    return false;
                }
            }
            if let Some(b) = s.characters.get_mut(&(cid as i64)) {
                b.plex_balance -= total_plex;
                if let Some(lp) = b.loyalty_points.get_mut(&(PARAGON_CORP_ID as i64)) {
                    *lp -= buyer_lp_fee;
                }
            }
            if let Some(seller) = s.characters.get_mut(&(listing.seller_id as i64)) {
                seller.plex_balance += net_plex;
            }
        } else {
            return false;
        }
        true
    });

    if settled {
        {
            let mut skinr_data = st.skinr.data.write().unwrap_or_else(|e| e.into_inner());
            let s_key = SkinrCosmeticsState::skin_lic_key(listing.seller_id, &listing.skin_hex);
            if let Some(lic) = skinr_data.skin_licenses.get_mut(&s_key) {
                lic.escrowed = lic.escrowed.saturating_sub(u64::from(quantity));
            }
            let b_key = SkinrCosmeticsState::skin_lic_key(cid, &listing.skin_hex);
            let b_entry = skinr_data.skin_licenses.entry(b_key).or_insert_with(|| ThirdPartySkinLicense {
                character_id: cid,
                skin_hex: listing.skin_hex.clone(),
                activated: false,
                unactivated: 0,
                escrowed: 0,
                first_acquired_unix: now,
            });
            b_entry.unactivated = b_entry.unactivated.saturating_add(u64::from(quantity));
        }
        st.skinr.save();
        st.save();
    } else {
        rollback_listing_qty(st, &listing, quantity);
    }
    st.save();
    queue_transaction_notice(cid, &tx_uuid, settled, now);
    let mut out = Vec::new();
    put_uuid(1, &tx_uuid, &mut out);
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingResponse",
        out,
    ))
}

fn skin_admin_set_expiration(
    st: &CosmeticsMarketState,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let _cid = require_char(ctx)?;
    let uuid = uuid_of(msg, 1)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skin listing UUID".to_string()))?;
    let expires_unix = sub(msg, 2)
        .and_then(|m| m.get(1))
        .and_then(as_u64)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing expiration timestamp".to_string()))?;
    {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(rec) = data.skin_listings.get_mut(&hex(&uuid)) else {
            return Err((STATUS_NOT_FOUND, "Skin listing not found".to_string()));
        };
        rec.expires_unix = expires_unix;
    }
    st.save();
    Ok(any(
        "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationResponse",
        Vec::new(),
    ))
}

/// Registers the state-backed Paragon Hub Cosmetic Market handlers.
pub fn register(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
    skinr: Arc<SkinrCosmeticsState>,
) {
    let state = CosmeticsMarketState::load(db, sde, skinr);
    let reg = |name: &'static str,
               f: fn(&CosmeticsMarketState, &AuthoritativeContext, &DynamicQuasarMessage) -> HandlerResult| {
        let st = Arc::clone(&state);
        router.register_handler(name, Arc::new(move |ctx, _any, msg| f(&st, ctx, msg)));
    };
    reg(
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        market_get_broker_fees,
    );
    reg(
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        market_get_tax_rate,
    );
    reg(
        "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
        market_get_buyer_fee,
    );
    reg(
        "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest",
        market_get_buyer_fees_definition,
    );
    reg(
        "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
        component_get_all,
    );
    reg(
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
        component_initiate_purchase,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        skin_get_all,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        skin_get_all_owned,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.GetRequest",
        skin_get,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        skin_initiate_create,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
        skin_cancel_listing,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        skin_initiate_purchase,
    );
    reg(
        "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
        skin_admin_set_expiration,
    );
}
