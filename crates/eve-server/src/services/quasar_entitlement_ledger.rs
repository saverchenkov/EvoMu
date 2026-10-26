//! `quasar_entitlement_ledger` — persisted asset holdings and entitlements behind the V24.01
//! `eve.assetholding.asset.api.SpawnRequest`, `eve.assetholding.entitlement.api.DirectDeliveryRequest`
//! and `eve_public.assetholding.entitlement.api.{GetRequest,RedeemRequest}` handlers (`Q-6`).
//!
//! Lifecycle (all through Quasar requests):
//! 1. a GM spawns an inventory asset (item stacks of SDE types);
//! 2. a GM direct-delivers an entitlement to that asset to a character, which publishes
//!    `eve_public.assetholding.entitlement.api.GrantedNotice` to the holder;
//! 3. the holder redeems it; the asset's items are materialized in the holder's station hangar
//!    and `RedeemedNotice` (consumed by `assetholding/client/messengers/assetholdingMessenger.pyj`)
//!    is published.
//!
//! State persists in `WorldStateSnapshot::quasar_state_blobs["quasar_entitlements"]`.

#![forbid(unsafe_code)]

use super::quasar_social_chat::ctx_queue_notice;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

const BLOB_KEY: &str = "quasar_entitlements";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AssetRecord {
    pub uuid: [u8; 16],
    pub name: String,
    /// Item stacks `(type_id, quantity)` delivered per redeemed entitlement unit.
    pub stacks: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EntitlementRecord {
    pub holder: u32,
    pub asset: [u8; 16],
    pub group_name: String,
    pub group_uuid: Option<[u8; 16]>,
    pub total: u64,
    pub redeemed: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EntitlementLedger {
    pub assets: HashMap<String, AssetRecord>,
    pub entitlements: HashMap<String, EntitlementRecord>,
    pub next_asset_seq: u64,
}

fn hex(u: &[u8]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }
}

fn load(db: &WorldRepository) -> EntitlementLedger {
    db.with_snapshot(|s| s.quasar_state_blobs.get(BLOB_KEY).and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default())
}

fn sub(msg: &DynamicQuasarMessage, field: u32) -> Option<&DynamicQuasarMessage> {
    match msg.get(field)? {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

fn as_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        _ => None,
    }
}

fn bytes16(msg: Option<&DynamicQuasarMessage>, field: u32) -> Option<[u8; 16]> {
    match msg?.get(field)? {
        DynamicValue::Bytes(b) if b.len() == 16 => b.as_slice().try_into().ok(),
        _ => None,
    }
}

fn repeated(msg: &DynamicQuasarMessage, field: u32) -> Vec<&DynamicQuasarMessage> {
    match msg.get(field) {
        Some(DynamicValue::Repeated(v)) => v
            .iter()
            .filter_map(|x| match x {
                DynamicValue::Message(m) => Some(m.as_ref()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Parsed `entitlement.Identifier { holder = 1 { character = 1 }, asset = 2 { uuid }, group_by = 3 { name, uuid } }`.
struct EntitlementKey {
    holder: u32,
    asset: [u8; 16],
    group_name: String,
    group_uuid: Option<[u8; 16]>,
}

impl EntitlementKey {
    fn parse(id: Option<&DynamicQuasarMessage>) -> Result<Self, (u32, String)> {
        let id = id.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing entitlement identifier".to_string()))?;
        let holder = sub(id, 1)
            .and_then(|h| sub(h, 1))
            .and_then(|c| c.get(1))
            .and_then(as_u64)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Entitlement holder must be a character".to_string()))? as u32;
        let asset = bytes16(sub(id, 2), 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing asset identifier".to_string()))?;
        let group = sub(id, 3);
        let group_name = match group.and_then(|g| g.get(1)) {
            Some(DynamicValue::String(s)) => s.clone(),
            _ => String::new(),
        };
        Ok(Self { holder, asset, group_name, group_uuid: bytes16(group, 2) })
    }

    fn key(&self) -> String {
        let group = self.group_uuid.map(|u| hex(&u)).unwrap_or_else(|| self.group_name.clone());
        format!("{}:{}:{}", self.holder, hex(&self.asset), group)
    }
}

/// Encodes the public `eve_public.assetholding.entitlement.Identifier`.
fn public_identifier(e: &EntitlementRecord) -> Vec<u8> {
    let (mut ch, mut holder, mut asset, mut group, mut out) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, e.holder as u64, &mut ch);
    encode_bytes_field(1, &ch, &mut holder);
    encode_bytes_field(1, &e.asset, &mut asset);
    encode_string_field(1, &e.group_name, &mut group);
    if let Some(u) = e.group_uuid {
        encode_bytes_field(2, &u, &mut group);
    }
    encode_bytes_field(1, &holder, &mut out);
    encode_bytes_field(2, &asset, &mut out);
    encode_bytes_field(3, &group, &mut out);
    out
}

fn entitlement_notice(type_name: &str, e: &EntitlementRecord, quantity: u64) -> GoogleAny {
    let mut out = Vec::new();
    encode_bytes_field(1, &public_identifier(e), &mut out);
    encode_varint_field(2, quantity, &mut out);
    any(type_name, out)
}

/// Registers the ledger-backed handlers (after the generic asset holding registrations).
pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) {
    let (d, s) = (Arc::clone(&db), Arc::clone(&sde));
    router.register_handler("eve.assetholding.asset.api.SpawnRequest", Arc::new(move |ctx, _any, msg| spawn(&d, &s, ctx, msg)));
    let d = Arc::clone(&db);
    router.register_handler(
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
        Arc::new(move |ctx, _any, msg| direct_delivery(&d, ctx, msg)),
    );
    let d = Arc::clone(&db);
    router.register_handler("eve_public.assetholding.entitlement.api.GetRequest", Arc::new(move |ctx, _any, msg| get(&d, ctx, msg)));
    let d = Arc::clone(&db);
    router.register_handler("eve_public.assetholding.entitlement.api.RedeemRequest", Arc::new(move |ctx, _any, msg| redeem(&d, ctx, msg)));
}

/// GM: `SpawnRequest { internal_name = 2, itemstack = 6 { stacks = 1 [{ type = 1, quantity = 2 }] } }`.
fn spawn(db: &WorldRepository, sde: &SdeRegistry, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let stacks_msg = sub(msg, 6).ok_or_else(|| (STATUS_BAD_REQUEST, "Only item-stack assets are supported".to_string()))?;
    let mut stacks = Vec::new();
    for st in repeated(stacks_msg, 1) {
        let type_id = sub(st, 1).and_then(|t| t.get(1)).and_then(as_u64).unwrap_or(0) as u32;
        let qty = st.get(2).and_then(as_u64).unwrap_or(0) as u32;
        if !sde.types.contains_key(&type_id) || qty == 0 {
            return Err((STATUS_BAD_REQUEST, format!("Invalid item stack (type {type_id}, quantity {qty})")));
        }
        stacks.push((type_id, qty));
    }
    if stacks.is_empty() {
        return Err((STATUS_BAD_REQUEST, "Asset needs at least one item stack".to_string()));
    }
    let name = match msg.get(2) {
        Some(DynamicValue::String(s)) => s.clone(),
        _ => String::new(),
    };
    let mut ledger = load(db);
    ledger.next_asset_seq += 1;
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"ASSETHLD");
    uuid[8..16].copy_from_slice(&ledger.next_asset_seq.to_be_bytes());
    ledger.assets.insert(hex(&uuid), AssetRecord { uuid, name: name.clone(), stacks });
    save(db, &ledger);
    let (mut id, mut attrs, mut out) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &uuid, &mut id);
    encode_string_field(1, &name, &mut attrs);
    encode_varint_field(7, 1, &mut attrs); // no_auto_release
    encode_bytes_field(1, &id, &mut out);
    encode_bytes_field(2, &attrs, &mut out);
    Ok(any("eve.assetholding.asset.api.SpawnResponse", out))
}

fn save(db: &WorldRepository, ledger: &EntitlementLedger) {
    if let Ok(json) = serde_json::to_string(ledger) {
        db.with_mut_snapshot(|s| {
            s.quasar_state_blobs.insert(BLOB_KEY.into(), json);
        });
    }
}

/// GM: `DirectDeliveryRequest { entitlements = 2 [{ entitlement = 2, quantity = 10 }] }` grants
/// (or tops up) entitlements and notifies each holder with `GrantedNotice`.
fn direct_delivery(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let entries = repeated(msg, 2);
    if entries.is_empty() {
        return Err((STATUS_BAD_REQUEST, "No entitlements to deliver".to_string()));
    }
    let mut ledger = load(db);
    let mut granted = Vec::new();
    for e in entries {
        let key = EntitlementKey::parse(sub(e, 2))?;
        let quantity = e.get(10).and_then(as_u64).unwrap_or(0);
        if quantity == 0 {
            return Err((STATUS_BAD_REQUEST, "Delivered quantity must be positive".to_string()));
        }
        if !ledger.assets.contains_key(&hex(&key.asset)) {
            return Err((STATUS_NOT_FOUND, "Asset not found".to_string()));
        }
        if !db.with_snapshot(|s| s.characters.contains_key(&(key.holder as i64))) {
            return Err((STATUS_NOT_FOUND, format!("Character {} not found", key.holder)));
        }
        let rec = ledger.entitlements.entry(key.key()).or_insert_with(|| EntitlementRecord {
            holder: key.holder,
            asset: key.asset,
            group_name: key.group_name.clone(),
            group_uuid: key.group_uuid,
            total: 0,
            redeemed: 0,
        });
        rec.total = rec.total.saturating_add(quantity);
        granted.push((rec.clone(), quantity));
    }
    save(db, &ledger);
    for (rec, quantity) in granted {
        ctx_queue_notice(
            NoticeScope::Character(rec.holder),
            entitlement_notice("eve_public.assetholding.entitlement.api.GrantedNotice", &rec, quantity),
            ctx.now_unix(),
        );
    }
    Ok(any("eve.assetholding.entitlement.api.DirectDeliveryResponse", Vec::new()))
}

/// Holder-only lookup of an entitlement by its public identifier.
fn owned_entitlement(ledger: &EntitlementLedger, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<(String, EntitlementRecord), (u32, String)> {
    let cid = ctx.effective_character_id().ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".to_string()))?;
    let key = EntitlementKey::parse(sub(msg, 1))?;
    if key.holder != cid {
        return Err((STATUS_FORBIDDEN, "Entitlement belongs to another character".to_string()));
    }
    let k = key.key();
    let rec = ledger.entitlements.get(&k).cloned().ok_or_else(|| (STATUS_NOT_FOUND, "Entitlement not found".to_string()))?;
    Ok((k, rec))
}

/// `GetResponse { entitlement = 2 (Attributes { quantity = 1 { total = 1, redeemed = 2 } }) }`.
fn get(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    let (_, rec) = owned_entitlement(&load(db), ctx, msg)?;
    let (mut qty, mut attrs, mut out) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, rec.total, &mut qty);
    encode_varint_field(2, rec.redeemed, &mut qty);
    encode_bytes_field(1, &qty, &mut attrs);
    encode_bytes_field(2, &attrs, &mut out);
    Ok(any("eve_public.assetholding.entitlement.api.GetResponse", out))
}

/// Redeems every outstanding unit: materializes the asset's item stacks (× units) in the holder's
/// current station hangar (home station when in space) and publishes `RedeemedNotice`.
fn redeem(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    let mut ledger = load(db);
    let (key, rec) = owned_entitlement(&ledger, ctx, msg)?;
    let units = rec.total.saturating_sub(rec.redeemed);
    if units == 0 {
        return Err((STATUS_BAD_REQUEST, "Entitlement has nothing left to redeem".to_string()));
    }
    let asset = ledger.assets.get(&hex(&rec.asset)).cloned().ok_or_else(|| (STATUS_NOT_FOUND, "Asset not found".to_string()))?;
    let holder = rec.holder as i64;
    db.with_mut_snapshot(|s| {
        let ch = s.characters.get(&holder).ok_or_else(|| (STATUS_NOT_FOUND, "Character not found".to_string()))?;
        let location = ch.station_id.unwrap_or(ch.home_station_id);
        for (type_id, qty) in &asset.stacks {
            let quantity = i32::try_from((*qty as u64).saturating_mul(units)).map_err(|_| (STATUS_BAD_REQUEST, "Quantity overflow".to_string()))?;
            s.next_entity_id += 1;
            let item_id = s.next_entity_id;
            s.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: *type_id,
                    owner_id: holder,
                    location_id: location,
                    flag: InventoryFlag::Hangar,
                    quantity,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        Ok::<(), (u32, String)>(())
    })?;
    let mut updated = rec;
    updated.redeemed = updated.total;
    ledger.entitlements.insert(key, updated.clone());
    save(db, &ledger);
    ctx_queue_notice(
        NoticeScope::Character(updated.holder),
        entitlement_notice("eve_public.assetholding.entitlement.api.RedeemedNotice", &updated, units),
        ctx.now_unix(),
    );
    Ok(any("eve_public.assetholding.entitlement.api.RedeemResponse", Vec::new()))
}
