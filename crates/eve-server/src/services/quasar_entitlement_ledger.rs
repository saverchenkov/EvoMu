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
    #[serde(default)]
    pub character_title_id: Option<u64>,
    #[serde(default)]
    pub ship_skin_license_hex: Option<String>,
    /// Optional finite capacity `(original, remaining)`.
    #[serde(default)]
    pub capacity: Option<(u64, u64)>,
    #[serde(default)]
    pub benefactor_char_id: Option<u32>,
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

pub(super) fn hex(u: &[u8]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }
}

pub(super) fn load(db: &WorldRepository) -> EntitlementLedger {
    db.with_snapshot(|s| s.quasar_state_blobs.get(BLOB_KEY).and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default())
}

pub fn redeemed_titles_for_char(db: &WorldRepository, char_id: u32) -> Vec<u64> {
    let ledger = load(db);
    let mut out = Vec::new();
    for ent in ledger.entitlements.values() {
        if ent.holder == char_id && ent.redeemed > 0 {
            if let Some(tid) = ledger.assets.get(&hex(&ent.asset)).and_then(|a| a.character_title_id) {
                if !out.contains(&tid) {
                    out.push(tid);
                }
            }
        }
    }
    out
}

pub fn redeemed_skin_licenses_for_char(db: &WorldRepository, char_id: u32) -> Vec<(String, u64)> {
    let ledger = load(db);
    let mut map: HashMap<String, u64> = HashMap::new();
    for ent in ledger.entitlements.values() {
        if ent.holder == char_id && ent.redeemed > 0 {
            if let Some(ref sh) = ledger.assets.get(&hex(&ent.asset)).and_then(|a| a.ship_skin_license_hex.clone()) {
                *map.entry(sh.clone()).or_insert(0) += ent.redeemed;
            }
        }
    }
    let mut out: Vec<(String, u64)> = map.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub(super) fn encode_asset_attributes(asset: &AssetRecord, is_public: bool) -> Vec<u8> {
    let mut cap_msg = Vec::new();
    if let Some((orig, rem)) = asset.capacity {
        let mut fin = Vec::new();
        encode_varint_field(1, orig, &mut fin);
        encode_varint_field(2, rem, &mut fin);
        encode_bytes_field(1, &fin, &mut cap_msg);
    } else {
        encode_varint_field(2, 1, &mut cap_msg);
    }
    let mut ben_msg = Vec::new();
    if let Some(cid) = asset.benefactor_char_id {
        let mut ch = Vec::new();
        encode_varint_field(1, cid as u64, &mut ch);
        encode_bytes_field(1, &ch, &mut ben_msg);
    } else if !is_public {
        encode_varint_field(3, 1, &mut ben_msg);
    }
    let mut attrs = Vec::new();
    if is_public {
        if !ben_msg.is_empty() {
            encode_bytes_field(1, &ben_msg, &mut attrs);
        }
        encode_bytes_field(3, &cap_msg, &mut attrs);
        return attrs;
    }
    encode_string_field(1, &asset.name, &mut attrs);
    encode_bytes_field(3, &ben_msg, &mut attrs);
    let mut unit_msg = Vec::new();
    if let Some(&(type_id, qty)) = asset.stacks.first() {
        let (mut t, mut st) = (Vec::new(), Vec::new());
        encode_varint_field(1, type_id as u64, &mut t);
        encode_bytes_field(1, &t, &mut st);
        encode_varint_field(2, qty as u64, &mut st);
        encode_bytes_field(8, &st, &mut unit_msg);
    } else if let Some(ref skin_hex) = asset.ship_skin_license_hex {
        let (mut sk, mut lic) = (Vec::new(), Vec::new());
        encode_string_field(1, skin_hex, &mut sk);
        encode_bytes_field(1, &sk, &mut lic);
        encode_bytes_field(10, &lic, &mut unit_msg);
    } else if let Some(tid) = asset.character_title_id {
        let (mut t_id, mut t_unit) = (Vec::new(), Vec::new());
        let uuid = super::quasar_cosmetics_drafts::id_to_uuid_bytes(tid);
        encode_bytes_field(1, &uuid, &mut t_id);
        encode_bytes_field(1, &t_id, &mut t_unit);
        encode_bytes_field(12, &t_unit, &mut unit_msg);
    }
    if !unit_msg.is_empty() {
        encode_bytes_field(4, &unit_msg, &mut attrs);
    }
    encode_bytes_field(5, &cap_msg, &mut attrs);
    encode_varint_field(7, 1, &mut attrs);
    attrs
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
pub(super) fn public_identifier(e: &EntitlementRecord) -> Vec<u8> {
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
    let (d, s) = (Arc::clone(&db), Arc::clone(&sde));
    router.register_handler("eve.assetholding.asset.api.HoldRequest", Arc::new(move |ctx, _any, msg| hold(&d, &s, ctx, msg)));
    let (d, s) = (Arc::clone(&db), Arc::clone(&sde));
    router.register_handler("eve.assetholding.asset.api.CreateRequest", Arc::new(move |ctx, _any, msg| create_asset(&d, &s, ctx, msg)));
    let d = Arc::clone(&db);
    router.register_handler(
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
        Arc::new(move |ctx, _any, msg| direct_delivery(&d, ctx, msg)),
    );
    let d = Arc::clone(&db);
    router.register_handler("eve_public.assetholding.entitlement.api.GetRequest", Arc::new(move |ctx, _any, msg| get(&d, ctx, msg, true)));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.GetRequest", Arc::new(move |ctx, _any, msg| get(&d, ctx, msg, false)));
    let d = Arc::clone(&db);
    router.register_handler("eve_public.assetholding.entitlement.api.RedeemRequest", Arc::new(move |ctx, _any, msg| redeem(&d, ctx, msg, true, false)));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.RedeemRequest", Arc::new(move |ctx, _any, msg| redeem(&d, ctx, msg, false, false)));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.admin.RedeemRequest", Arc::new(move |ctx, _any, msg| redeem(&d, ctx, msg, false, true)));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.DeleteRequest", Arc::new(move |ctx, _any, msg| delete_or_expire(&d, ctx, msg, "eve.assetholding.entitlement.api.DeleteResponse", false)));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.admin.ExpireRequest", Arc::new(move |ctx, _any, msg| delete_or_expire(&d, ctx, msg, "eve.assetholding.entitlement.api.admin.ExpireResponse", true)));
    let d = Arc::clone(&db);
    router.register_handler("eve_public.assetholding.entitlement.api.admin.ExpireRequest", Arc::new(move |ctx, _any, msg| delete_or_expire(&d, ctx, msg, "eve_public.assetholding.entitlement.api.admin.ExpireResponse", true)));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.admin.AutoRedeemRequest", Arc::new(move |ctx, _any, msg| auto_redeem(&d, ctx, msg, "eve.assetholding.entitlement.api.admin.AutoRedeemResponse")));
    let d = Arc::clone(&db);
    router.register_handler("eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest", Arc::new(move |ctx, _any, msg| auto_redeem(&d, ctx, msg, "eve_public.assetholding.entitlement.api.admin.AutoRedeemResponse")));
    let d = Arc::clone(&db);
    router.register_handler("eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest", Arc::new(move |ctx, _any, msg| redeliver_stale(&d, ctx, msg)));
}

const MAX_LEDGER_ENTRIES: usize = 10_000;

/// GM: `SpawnRequest { internal_name = 2, itemstack = 6 { stacks = 1 } | character_title = 13 { title = 1, finite = 3 } }`.
fn spawn(db: &WorldRepository, sde: &SdeRegistry, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let mut stacks = Vec::new();
    let mut character_title_id = None;
    let mut capacity = None;
    if let Some(stacks_msg) = sub(msg, 6) {
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
    } else if let Some(title_sp) = sub(msg, 13) {
        let tid = sub(title_sp, 1)
            .and_then(|t| match t.get(1) {
                Some(DynamicValue::Bytes(b)) => Some(super::quasar_cosmetics_drafts::extract_id_from_uuid_bytes(b)),
                Some(v) => as_u64(v),
                None => None,
            })
            .filter(|&id| id > 0)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid character title identifier".to_string()))?;
        character_title_id = Some(tid);
        if let Some(fin) = title_sp.get(3).and_then(as_u64).filter(|&c| c > 0) {
            capacity = Some((fin, fin));
        }
    } else {
        return Err((STATUS_BAD_REQUEST, "Unsupported SpawnRequest unit".to_string()));
    }
    let name = match msg.get(2) {
        Some(DynamicValue::String(s)) => s.clone(),
        _ => String::new(),
    };
    let mut ledger = load(db);
    if ledger.assets.len() >= MAX_LEDGER_ENTRIES {
        return Err((STATUS_BAD_REQUEST, "Asset ledger capacity reached".to_string()));
    }
    ledger.next_asset_seq += 1;
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"ASSETHLD");
    uuid[8..16].copy_from_slice(&ledger.next_asset_seq.to_be_bytes());
    let record = AssetRecord {
        uuid,
        name,
        stacks,
        character_title_id,
        ship_skin_license_hex: None,
        capacity,
        benefactor_char_id: ctx.effective_character_id(),
    };
    let attrs = encode_asset_attributes(&record, false);
    ledger.assets.insert(hex(&uuid), record);
    save(db, &ledger);
    let (mut id, mut out) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &uuid, &mut id);
    encode_bytes_field(1, &id, &mut out);
    encode_bytes_field(2, &attrs, &mut out);
    Ok(any("eve.assetholding.asset.api.SpawnResponse", out))
}

/// `HoldRequest { creator = 2, itemstack = 7 | ship_skin_license = 11 { license = 1 { skin = 1 { hex = 1 } }, capacity = 2 } }`.
fn hold(db: &WorldRepository, sde: &SdeRegistry, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let creator_cid = sub(msg, 2).and_then(|c| c.get(1)).and_then(as_u64).map(|u| u as u32).or_else(|| ctx.effective_character_id());
    let mut stacks = Vec::new();
    let mut ship_skin_license_hex = None;
    let capacity: Option<(u64, u64)>;
    let name: String;
    if let Some(skin_hp) = sub(msg, 11) {
        let skin_hex = sub(skin_hp, 1)
            .and_then(|u| sub(u, 1))
            .and_then(|s| match s.get(1) {
                Some(DynamicValue::String(h)) if !h.trim().is_empty() => Some(h.trim().to_string()),
                _ => None,
            })
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing ship_skin_license hex".to_string()))?;
        let cap = skin_hp.get(2).and_then(as_u64).unwrap_or(1).max(1);
        name = format!("skin_license:{skin_hex}");
        ship_skin_license_hex = Some(skin_hex);
        capacity = Some((cap, cap));
    } else if let Some(stack_hp) = sub(msg, 7) {
        let typed = sub(stack_hp, 1).unwrap_or(stack_hp);
        for st in repeated(typed, 1) {
            let type_id = sub(st, 1).and_then(|t| t.get(1)).and_then(as_u64).unwrap_or(0) as u32;
            let qty = st.get(2).and_then(as_u64).unwrap_or(0) as u32;
            if !sde.types.contains_key(&type_id) || qty == 0 {
                return Err((STATUS_BAD_REQUEST, format!("Invalid held item stack ({type_id}, {qty})")));
            }
            stacks.push((type_id, qty));
        }
        if stacks.is_empty() {
            return Err((STATUS_BAD_REQUEST, "Held itemstack requires at least one stack".to_string()));
        }
        let cap = stack_hp.get(7).and_then(as_u64).unwrap_or(1).max(1);
        name = format!("held_stack:{}", stacks[0].0);
        capacity = Some((cap, cap));
    } else {
        return Err((STATUS_BAD_REQUEST, "Unsupported HoldRequest unit".to_string()));
    }
    let mut ledger = load(db);
    if ledger.assets.len() >= MAX_LEDGER_ENTRIES {
        return Err((STATUS_BAD_REQUEST, "Asset ledger capacity reached".to_string()));
    }
    ledger.next_asset_seq += 1;
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"ASSETHLD");
    uuid[8..16].copy_from_slice(&ledger.next_asset_seq.to_be_bytes());
    let record = AssetRecord {
        uuid,
        name,
        stacks,
        character_title_id: None,
        ship_skin_license_hex,
        capacity,
        benefactor_char_id: creator_cid,
    };
    let attrs = encode_asset_attributes(&record, false);
    ledger.assets.insert(hex(&uuid), record);
    save(db, &ledger);
    let (mut id, mut out) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &uuid, &mut id);
    encode_bytes_field(1, &id, &mut out);
    encode_bytes_field(2, &attrs, &mut out);
    Ok(any("eve.assetholding.asset.api.HoldResponse", out))
}

/// `CreateRequest { creator = 2, benefactor = 3, unit = 4, infinite = 5, finite = 6 }`.
fn create_asset(db: &WorldRepository, sde: &SdeRegistry, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let unit = sub(msg, 4).ok_or_else(|| (STATUS_BAD_REQUEST, "CreateRequest requires unit at field 4".to_string()))?;
    let mut stacks = Vec::new();
    let mut character_title_id = None;
    let mut ship_skin_license_hex = None;
    if let Some(st_u) = sub(unit, 8) {
        let type_id = sub(st_u, 1).and_then(|t| t.get(1)).and_then(as_u64).unwrap_or(0) as u32;
        let qty = st_u.get(2).and_then(as_u64).unwrap_or(1).max(1) as u32;
        if !sde.types.contains_key(&type_id) {
            return Err((STATUS_BAD_REQUEST, format!("Invalid item type {type_id}")));
        }
        stacks.push((type_id, qty));
    } else if let Some(sk_u) = sub(unit, 10) {
        let hex_s = sub(sk_u, 1).and_then(|s| match s.get(1) {
            Some(DynamicValue::String(h)) if !h.trim().is_empty() => Some(h.trim().to_string()),
            _ => None,
        }).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skin license hex".to_string()))?;
        ship_skin_license_hex = Some(hex_s);
    } else if let Some(t_u) = sub(unit, 12) {
        let tid = sub(t_u, 1).and_then(|t| match t.get(1) {
            Some(DynamicValue::Bytes(b)) => Some(super::quasar_cosmetics_drafts::extract_id_from_uuid_bytes(b)),
            Some(v) => as_u64(v),
            None => None,
        }).filter(|&v| v > 0).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing character title id".to_string()))?;
        character_title_id = Some(tid);
    } else {
        return Err((STATUS_BAD_REQUEST, "Unsupported CreateRequest unit".to_string()));
    }
    let capacity = msg.get(6).and_then(as_u64).filter(|&c| c > 0).map(|c| (c, c));
    let mut ledger = load(db);
    if ledger.assets.len() >= MAX_LEDGER_ENTRIES {
        return Err((STATUS_BAD_REQUEST, "Asset ledger capacity reached".to_string()));
    }
    ledger.next_asset_seq += 1;
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"ASSETHLD");
    uuid[8..16].copy_from_slice(&ledger.next_asset_seq.to_be_bytes());
    ledger.assets.insert(hex(&uuid), AssetRecord {
        uuid,
        name: format!("created:{}", ledger.next_asset_seq),
        stacks,
        character_title_id,
        ship_skin_license_hex,
        capacity,
        benefactor_char_id: ctx.effective_character_id(),
    });
    save(db, &ledger);
    let (mut id, mut out) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &uuid, &mut id);
    encode_bytes_field(1, &id, &mut out);
    Ok(any("eve.assetholding.asset.api.CreateResponse", out))
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
        if !db.with_snapshot(|s| s.characters.contains_key(&(key.holder as i64))) {
            return Err((STATUS_NOT_FOUND, format!("Character {} not found", key.holder)));
        }
        let k = key.key();
        if !ledger.entitlements.contains_key(&k) && ledger.entitlements.len() >= MAX_LEDGER_ENTRIES {
            return Err((STATUS_BAD_REQUEST, "Entitlement ledger capacity reached".to_string()));
        }
        let asset = ledger
            .assets
            .get_mut(&hex(&key.asset))
            .ok_or_else(|| (STATUS_NOT_FOUND, "Asset not found".to_string()))?;
        if let Some((_orig, rem)) = &mut asset.capacity {
            if *rem < quantity {
                return Err((STATUS_BAD_REQUEST, "Asset finite capacity exhausted".to_string()));
            }
            *rem -= quantity;
        }
        let rec = ledger.entitlements.entry(k).or_insert_with(|| EntitlementRecord {
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

/// Holder-or-admin lookup of an entitlement by its identifier.
fn owned_entitlement(ledger: &EntitlementLedger, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage, allow_admin: bool) -> Result<(String, EntitlementRecord), (u32, String)> {
    let is_admin = allow_admin && require_admin_role(ctx).is_ok();
    let key = EntitlementKey::parse(sub(msg, 1))?;
    if !is_admin {
        let cid = ctx.effective_character_id().ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".to_string()))?;
        if key.holder != cid {
            return Err((STATUS_FORBIDDEN, "Entitlement belongs to another character".to_string()));
        }
    }
    let k = key.key();
    let rec = ledger.entitlements.get(&k).cloned().ok_or_else(|| (STATUS_NOT_FOUND, "Entitlement not found".to_string()))?;
    Ok((k, rec))
}

/// `GetResponse { entitlement = 1|2 (Attributes { quantity = 1 { total = 1, redeemed = 2 } }) }`.
fn get(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage, is_public: bool) -> Result<GoogleAny, (u32, String)> {
    let (_, rec) = owned_entitlement(&load(db), ctx, msg, !is_public)?;
    let (mut qty, mut attrs, mut out) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, rec.total, &mut qty);
    encode_varint_field(2, rec.redeemed, &mut qty);
    encode_bytes_field(1, &qty, &mut attrs);
    if !is_public {
        encode_varint_field(5, 1, &mut attrs);
        encode_bytes_field(1, &attrs, &mut out);
        Ok(any("eve.assetholding.entitlement.api.GetResponse", out))
    } else {
        encode_bytes_field(2, &attrs, &mut out);
        Ok(any("eve_public.assetholding.entitlement.api.GetResponse", out))
    }
}

/// Redeems every outstanding unit: materializes the asset's item stacks (× units) in the holder's
/// current station hangar (home station when in space) and publishes `RedeemedNotice`.
fn redeem(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage, is_public: bool, require_admin: bool) -> Result<GoogleAny, (u32, String)> {
    if require_admin {
        require_admin_role(ctx)?;
    }
    let mut ledger = load(db);
    let (key, rec) = owned_entitlement(&ledger, ctx, msg, require_admin || !is_public)?;
    redeem_record(db, &mut ledger, ctx.now_unix(), &key, rec)?;
    save(db, &ledger);
    let rsp_type = if require_admin {
        "eve.assetholding.entitlement.api.admin.RedeemResponse"
    } else if is_public {
        "eve_public.assetholding.entitlement.api.RedeemResponse"
    } else {
        "eve.assetholding.entitlement.api.RedeemResponse"
    };
    Ok(any(rsp_type, Vec::new()))
}

fn redeem_record(db: &WorldRepository, ledger: &mut EntitlementLedger, now_unix: u64, key: &str, rec: EntitlementRecord) -> Result<(), (u32, String)> {
    let units = rec.total.saturating_sub(rec.redeemed);
    if units == 0 {
        return Err((STATUS_BAD_REQUEST, "Entitlement has nothing left to redeem".to_string()));
    }
    let asset_key = hex(&rec.asset);
    let mut asset = ledger.assets.get(&asset_key).cloned().ok_or_else(|| (STATUS_NOT_FOUND, "Asset not found".to_string()))?;
    let mut cap_claimed: Option<(u64, u64)> = None;
    if let Some((orig, rem)) = asset.capacity {
        if rem < units {
            return Err((STATUS_BAD_REQUEST, "Asset finite capacity exhausted".to_string()));
        }
        let new_rem = rem - units;
        asset.capacity = Some((orig, new_rem));
        cap_claimed = Some((rem, new_rem));
    }
    let holder = rec.holder as i64;
    db.try_with_mut_snapshot(|s| {
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
    ledger.assets.insert(asset_key, asset.clone());
    ledger.entitlements.insert(key.to_string(), updated.clone());
    if let Some((prev_rem, cur_rem)) = cap_claimed {
        let (mut a_id, mut cap_n) = (Vec::new(), Vec::new());
        encode_bytes_field(1, &asset.uuid, &mut a_id);
        encode_bytes_field(1, &a_id, &mut cap_n);
        encode_varint_field(2, prev_rem, &mut cap_n);
        encode_varint_field(3, cur_rem, &mut cap_n);
        ctx_queue_notice(
            NoticeScope::Character(updated.holder),
            any("eve_public.assetholding.asset.api.FiniteCapacityClaimedNotice", cap_n),
            now_unix,
        );
    }
    if let Some(tid) = asset.character_title_id {
        let (mut t_id, mut t_n) = (Vec::new(), Vec::new());
        encode_bytes_field(1, &super::quasar_cosmetics_drafts::id_to_uuid_bytes(tid), &mut t_id);
        encode_bytes_field(1, &t_id, &mut t_n);
        ctx_queue_notice(
            NoticeScope::Character(updated.holder),
            any("eve_public.cosmetic.character.title.api.GrantedNotice", t_n),
            now_unix,
        );
    }
    if let Some(ref skin_hex) = asset.ship_skin_license_hex {
        let (mut ch_id, mut sk_id, mut lic_id, mut lic_at, mut lic_n) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, updated.holder as u64, &mut ch_id);
        encode_string_field(1, skin_hex, &mut sk_id);
        encode_bytes_field(1, &ch_id, &mut lic_id);
        encode_bytes_field(2, &sk_id, &mut lic_id);
        encode_varint_field(1, 0, &mut lic_at);
        encode_varint_field(2, units, &mut lic_at);
        encode_bytes_field(1, &lic_id, &mut lic_n);
        encode_bytes_field(2, &lic_at, &mut lic_n);
        ctx_queue_notice(
            NoticeScope::Character(updated.holder),
            any("eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice", lic_n),
            now_unix,
        );
    }
    ctx_queue_notice(
        NoticeScope::Character(updated.holder),
        entitlement_notice("eve_public.assetholding.entitlement.api.RedeemedNotice", &updated, units),
        now_unix,
    );
    Ok(())
}

fn delete_or_expire(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage, rsp_type: &str, require_admin: bool) -> Result<GoogleAny, (u32, String)> {
    if require_admin {
        require_admin_role(ctx)?;
    }
    let mut ledger = load(db);
    let (key, rec) = owned_entitlement(&ledger, ctx, msg, true)?;
    if !require_admin && require_admin_role(ctx).is_err() && rec.redeemed > 0 {
        return Err((
            STATUS_FORBIDDEN,
            "Cannot delete an already-redeemed entitlement without admin role".to_string(),
        ));
    }
    ledger.entitlements.remove(&key);
    save(db, &ledger);
    let (mut a_id, mut del_n) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &rec.asset, &mut a_id);
    encode_bytes_field(1, &a_id, &mut del_n);
    ctx_queue_notice(
        NoticeScope::Character(rec.holder),
        any("eve_public.assetholding.asset.api.DeletedNotice", del_n),
        ctx.now_unix(),
    );
    Ok(any(rsp_type, Vec::new()))
}

fn auto_redeem(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage, rsp_type: &str) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let target_key = EntitlementKey::parse(sub(msg, 1)).ok().map(|k| k.key());
    let target_holder = sub(msg, 1)
        .and_then(|h| sub(h, 1).and_then(|c| c.get(1)).or_else(|| h.get(1)))
        .and_then(as_u64)
        .or_else(|| msg.get(1).and_then(as_u64))
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0);
    let target_asset = sub(msg, 2).and_then(|a| match a.get(1) {
        Some(DynamicValue::Bytes(b)) if b.len() == 16 => Some(hex(b)),
        _ => None,
    });
    let mut ledger = load(db);
    let pending: Vec<(String, EntitlementRecord)> = ledger
        .entitlements
        .iter()
        .filter(|(k, r)| {
            r.total > r.redeemed
                && target_key.as_ref().is_none_or(|tk| *k == tk)
                && (target_key.is_some() || target_holder.is_none_or(|th| r.holder == th))
                && (target_key.is_some() || target_asset.as_ref().is_none_or(|ta| &hex(&r.asset) == ta))
        })
        .map(|(k, r)| (k.clone(), r.clone()))
        .collect();
    for (k, rec) in pending {
        let _ = redeem_record(db, &mut ledger, ctx.now_unix(), &k, rec);
    }
    save(db, &ledger);
    Ok(any(rsp_type, Vec::new()))
}

fn redeliver_stale(db: &WorldRepository, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    require_admin_role(ctx)?;
    let target_holder = sub(msg, 1)
        .and_then(|h| sub(h, 1).and_then(|c| c.get(1)).or_else(|| h.get(1)))
        .and_then(as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0);
    let ledger = load(db);
    for rec in ledger.entitlements.values() {
        let rem = rec.total.saturating_sub(rec.redeemed);
        if rem > 0 && target_holder.is_none_or(|th| rec.holder == th) {
            ctx_queue_notice(
                NoticeScope::Character(rec.holder),
                entitlement_notice("eve_public.assetholding.entitlement.api.GrantedNotice", rec, rem),
                ctx.now_unix(),
            );
        }
    }
    Ok(any("eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsResponse", Vec::new()))
}
