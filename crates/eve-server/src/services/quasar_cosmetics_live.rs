//! `quasar_cosmetics_live` — state-backed V24.01 Quasar handlers for ship logos, ship SKIN state,
//! corporation palettes and Upwell structure paintwork licenses (`Q-9`).
//!
//! Wire layouts follow the client's generated descriptors:
//! - `eve_public.cosmetic.ship.logo` (`DisplayRequest` / `ClearRequest`, consumed by
//!   `cosmetics/client/messengers/cosmetics/ship/shipEmblemsRequestMessenger.pyj`);
//! - `eve_public.cosmetic.ship.api` (`GetRequest`, `GetAllInBubbleRequest`, `SetNotice`,
//!   `SetAllInBubbleNotice`, consumed by `shipSkinStateRequestMessenger.pyj` / `shipSkinStateNoticeMessenger.pyj`);
//! - `eve_public.cosmetic.corporation.palette.api` (`GetRequest`, `GetOwnRequest`, `SetRequest`,
//!   `CanEditRequest`, consumed by `corporation/client/colors/corpColorsRequestMessenger.pyj`);
//! - `eve_public.cosmetic.structure.paintwork.api` and `...paintwork.license.api[.admin]`
//!   (consumed by `cosmetics/client/messengers/cosmetics/structures/*` and
//!   `cosmetics/client/messengers/entitlements/corporation/structure/requestMessenger.pyj`).
//!
//! Ship SKIN state is read from the authoritative `applied_ship_skins` table written by
//! `shipCosmeticsMgr.ApplySkinToShip`; logos, palettes and paintwork licenses persist in
//! `WorldStateSnapshot::quasar_state_blobs["quasar_cosmetics_live"]`.

#![forbid(unsafe_code)]

use super::quasar_social_chat::ctx_queue_notice;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND,
};
use eve_db::{WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

const BLOB_KEY: &str = "quasar_cosmetics_live";
/// `corpRoleBrandManager` (V24 client `eve/common/lib/appConst.pyj`: 34359738368 = 1 << 35).
pub const CORP_ROLE_BRAND_MANAGER: u64 = 1 << 35;
/// SDE category of ships (`invCategories.categoryID` 6).
const CATEGORY_SHIP: u32 = 6;

type HandlerResult = Result<GoogleAny, (u32, String)>;

/// Corporation palette (`eve_public.cosmetic.corporation.palette.Attributes`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PaletteRecord {
    pub main: [u32; 3],
    pub secondary: Option<[u32; 3]>,
    pub tertiary: Option<[u32; 3]>,
    pub last_modifier: u32,
    pub last_modified_unix: u64,
}

/// An issued structure paintwork license; while it exists it is the structure's paintwork.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PaintworkLicense {
    pub uuid: [u8; 16],
    pub corporation_id: u32,
    pub activator_id: u32,
    pub issued_unix: u64,
    pub duration_secs: u64,
    pub structure_id: u64,
    pub solar_system_id: u32,
    /// SlotConfiguration fields 1..=7 (first..fourth, primary, secondary, detailing); `None` = empty.
    pub slots: [Option<u32>; 7],
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CosmeticsLiveData {
    /// ship item id -> logo slot index -> kind (1 = alliance, 2 = corporation).
    pub ship_logos: HashMap<u64, BTreeMap<i32, u8>>,
    pub palettes: HashMap<u32, PaletteRecord>,
    /// license uuid (hex) -> license.
    pub paint_licenses: HashMap<String, PaintworkLicense>,
    pub next_license_seq: u64,
}

pub struct CosmeticsLiveState {
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
    pub data: RwLock<CosmeticsLiveData>,
}

impl CosmeticsLiveState {
    pub fn load(db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) -> Arc<Self> {
        let data = db.with_snapshot(|s| {
            s.quasar_state_blobs.get(BLOB_KEY).and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
        });
        Arc::new(Self { db, sde, data: RwLock::new(data) })
    }

    fn save(&self) {
        let json = serde_json::to_string(&*self.data.read().unwrap_or_else(|e| e.into_inner()));
        if let Ok(json) = json {
            self.db.with_mut_snapshot(|s| {
                s.quasar_state_blobs.insert(BLOB_KEY.into(), json);
            });
        }
    }
}

fn hex(u: &[u8]) -> String {
    u.iter().map(|b| format!("{b:02x}")).collect()
}

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }
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

fn as_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        _ => None,
    }
}

fn sub(msg: &DynamicQuasarMessage, field: u32) -> Option<&DynamicQuasarMessage> {
    match msg.get(field)? {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

/// `Identifier { sequential = 1 }` nested at `field`.
fn seq_of(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    sub(msg, field).and_then(|m| m.get(1)).and_then(as_u64)
}

fn uuid_of(msg: &DynamicQuasarMessage, field: u32) -> Option<[u8; 16]> {
    match sub(msg, field)?.get(1)? {
        DynamicValue::Bytes(b) if b.len() == 16 => b.as_slice().try_into().ok(),
        _ => None,
    }
}

fn require_char(ctx: &AuthoritativeContext) -> Result<u32, (u32, String)> {
    ctx.effective_character_id().ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".to_string()))
}

/// Character's corporation from the authoritative character record.
fn char_corp(snap: &WorldStateSnapshot, cid: u32) -> Option<u32> {
    snap.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32)
}

/// Encodes `eve_public.cosmetic.ship.State` for a ship item, or `None` if it isn't a ship.
fn ship_state(snap: &WorldStateSnapshot, sde: &SdeRegistry, ship_id: u64) -> Option<Vec<u8>> {
    let item = snap.items.get(&(ship_id as i64))?;
    if sde.types.get(&item.type_id).map(|t| t.category_id) != Some(CATEGORY_SHIP) {
        return None;
    }
    let mut skin = Vec::new();
    match snap.applied_ship_skins.get(&(ship_id as i64)) {
        Some(&applied) => {
            // ApplySkinToShip stores either the SKIN id or the activated license type; the
            // client's first-party identifier is the SKIN id.
            let skin_id = sde.skin_licenses.get(&applied).map(|l| l.skin_id).unwrap_or(applied);
            let mut first = Vec::new();
            put_seq(1, skin_id as u64, &mut first);
            encode_bytes_field(1, &first, &mut skin);
        }
        None => encode_varint_field(3, 1, &mut skin), // no_skin
    }
    let mut out = Vec::new();
    put_seq(1, item.owner_id.max(0) as u64, &mut out);
    put_seq(2, ship_id, &mut out);
    encode_bytes_field(3, &skin, &mut out);
    Some(out)
}

/// States of every pilot's active ship in space in `solar_system_id`.
fn ship_states_in_space(snap: &WorldStateSnapshot, sde: &SdeRegistry, solar_system_id: u32) -> Vec<Vec<u8>> {
    let mut ships: Vec<u64> = snap
        .characters
        .values()
        .filter(|c| c.solar_system_id == solar_system_id && c.station_id.is_none() && c.structure_id.is_none())
        .filter(|c| c.active_ship_id > 0)
        .map(|c| c.active_ship_id as u64)
        .collect();
    ships.sort_unstable();
    ships.into_iter().filter_map(|s| ship_state(snap, sde, s)).collect()
}

/// Announces a ship's SKIN state after `shipCosmeticsMgr.ApplySkinToShip` changed it: to the
/// owner's solar system when the ship is in space (everyone on grid sees the new SKIN), otherwise
/// to the owner only.
pub fn publish_ship_skin_set(gateway: &PublicGatewayRouter, db: &WorldRepository, sde: &SdeRegistry, ship_id: i64, now_unix: u64) {
    let found = db.with_snapshot(|s| {
        let state = ship_state(s, sde, ship_id.max(0) as u64)?;
        let owner = s.items.get(&ship_id).map(|i| i.owner_id)?;
        let ch = s.characters.get(&owner);
        let in_space = ch.is_some_and(|c| c.active_ship_id == ship_id && c.station_id.is_none() && c.structure_id.is_none());
        Some((state, owner as u32, ch.map(|c| c.solar_system_id).filter(|_| in_space)))
    });
    if let Some((state, owner, system)) = found {
        let mut payload = Vec::new();
        encode_bytes_field(1, &state, &mut payload);
        let scope = system.map_or(NoticeScope::Character(owner), NoticeScope::SolarSystem);
        let _ = gateway.publish_notice(scope, any("eve_public.cosmetic.ship.api.SetNotice", payload), now_unix);
    }
}

/// When a pilot enters space (undock), sends them the SKIN states of every ship already in the
/// system (`SetAllInBubbleNotice`) and the system's structure paintworks (`SetAllInSolarSystemNotice`).
pub fn publish_arrival_cosmetics(gateway: &PublicGatewayRouter, db: &WorldRepository, sde: &SdeRegistry, char_id: i64, now_unix: u64) {
    // The persisted blob is the source of truth (every handler saves after mutating).
    let Some((system, states, data)) = db.with_snapshot(|s| {
        let sys = s.characters.get(&char_id)?.solar_system_id;
        let data: CosmeticsLiveData =
            s.quasar_state_blobs.get(BLOB_KEY).and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
        Some((sys, ship_states_in_space(s, sde, sys), data))
    }) else {
        return;
    };
    let mut bubble = Vec::new();
    for st in &states {
        encode_bytes_field(1, st, &mut bubble);
    }
    let scope = NoticeScope::Character(char_id as u32);
    let _ = gateway.publish_notice(scope.clone(), any("eve_public.cosmetic.ship.api.SetAllInBubbleNotice", bubble), now_unix);
    let mut paint = Vec::new();
    for lic in licenses_in_system(&data, system) {
        let mut entry = Vec::new();
        put_seq(1, lic.structure_id, &mut entry);
        encode_bytes_field(2, &encode_slots(&lic.slots), &mut entry);
        encode_bytes_field(1, &entry, &mut paint);
    }
    put_seq(2, system as u64, &mut paint);
    let _ = gateway.publish_notice(scope, any("eve_public.cosmetic.structure.paintwork.api.SetAllInSolarSystemNotice", paint), now_unix);
}

fn licenses_in_system(data: &CosmeticsLiveData, system: u32) -> Vec<PaintworkLicense> {
    let mut v: Vec<PaintworkLicense> = data.paint_licenses.values().filter(|l| l.solar_system_id == system).cloned().collect();
    v.sort_by_key(|l| l.structure_id);
    v
}

fn parse_slots(cfg: Option<&DynamicQuasarMessage>) -> Result<[Option<u32>; 7], (u32, String)> {
    let cfg = cfg.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing paintwork slot configuration".to_string()))?;
    let mut slots = [None; 7];
    for (i, slot) in slots.iter_mut().enumerate() {
        *slot = sub(cfg, i as u32 + 1).and_then(|s| s.get(1)).and_then(as_u64).map(|p| p as u32);
    }
    if slots.iter().all(Option::is_none) {
        return Err((STATUS_BAD_REQUEST, "Paintwork must set at least one slot".to_string()));
    }
    Ok(slots)
}

fn encode_slots(slots: &[Option<u32>; 7]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        let mut slot = Vec::new();
        match s {
            Some(p) => encode_varint_field(1, *p as u64, &mut slot),
            None => encode_varint_field(2, 1, &mut slot), // empty
        }
        encode_bytes_field(i as u32 + 1, &slot, &mut out);
    }
    out
}

fn encode_license_attrs(l: &PaintworkLicense) -> Vec<u8> {
    let mut a = Vec::new();
    put_seq(1, l.corporation_id as u64, &mut a);
    put_seq(2, l.activator_id as u64, &mut a);
    put_seconds(3, l.issued_unix, &mut a);
    put_seconds(4, l.duration_secs, &mut a);
    put_seq(5, l.structure_id, &mut a);
    a
}

fn uuid_msg(uuid: &[u8; 16]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    b
}

/// Paintwork `SetNotice` for one structure, broadcast to its solar system.
fn queue_structure_paintwork(structure_id: u64, system: u32, slots: &[Option<u32>; 7], now: u64) {
    let mut payload = Vec::new();
    put_seq(1, structure_id, &mut payload);
    encode_bytes_field(2, &encode_slots(slots), &mut payload);
    ctx_queue_notice(NoticeScope::SolarSystem(system), any("eve_public.cosmetic.structure.paintwork.api.SetNotice", payload), now);
}

fn parse_rgb(m: Option<&DynamicQuasarMessage>) -> Result<Option<[u32; 3]>, (u32, String)> {
    let Some(m) = m else { return Ok(None) };
    let mut rgb = [0u32; 3];
    for (i, c) in rgb.iter_mut().enumerate() {
        *c = m.get(i as u32 + 1).and_then(as_u64).unwrap_or(0) as u32;
        if *c > 255 {
            return Err((STATUS_BAD_REQUEST, "Color components must be 0..=255".to_string()));
        }
    }
    Ok(Some(rgb))
}

fn encode_palette(p: &PaletteRecord) -> Vec<u8> {
    let rgb = |c: &[u32; 3]| {
        let mut b = Vec::new();
        for (i, v) in c.iter().enumerate() {
            encode_varint_field(i as u32 + 1, *v as u64, &mut b);
        }
        b
    };
    let mut a = Vec::new();
    encode_bytes_field(1, &rgb(&p.main), &mut a);
    match &p.secondary {
        Some(c) => encode_bytes_field(2, &rgb(c), &mut a),
        None => encode_varint_field(3, 1, &mut a),
    }
    match &p.tertiary {
        Some(c) => encode_bytes_field(4, &rgb(c), &mut a),
        None => encode_varint_field(5, 1, &mut a),
    }
    a
}

/// Corporation palette editors: the CEO, directors and brand managers of that corporation.
fn can_edit_palette(ctx: &AuthoritativeContext, snap: &WorldStateSnapshot, cid: u32, corp: u32) -> bool {
    let roles = ctx.identity.as_ref().and_then(|id| id.roles.as_ref()).map_or(0, |r| r.bitmask);
    let is_ceo = snap.corporations.get(&(corp as i64)).is_some_and(|c| c.ceo_id == cid as i64);
    is_ceo || roles & (eve_db::corp_roles::ROLE_DIRECTOR | CORP_ROLE_BRAND_MANAGER) != 0
}

/// Registers the state-backed handlers (call after the generic cosmetics registrations).
pub fn register(
    router: &PublicGatewayRouter,
    state: Arc<CosmeticsLiveState>,
    skinr_state: Arc<super::quasar_cosmetics_skinr::SkinrCosmeticsState>,
) {
    let reg = |name: &'static str, f: fn(&CosmeticsLiveState, &AuthoritativeContext, &DynamicQuasarMessage) -> HandlerResult| {
        let st = Arc::clone(&state);
        router.register_handler(name, Arc::new(move |ctx, _any, msg| f(&st, ctx, msg)));
    };
    reg("eve_public.cosmetic.ship.logo.DisplayRequest", logo_display);
    reg("eve_public.cosmetic.ship.logo.ClearRequest", logo_clear);
    reg("eve_public.cosmetic.ship.api.GetRequest", ship_get);
    reg("eve_public.cosmetic.ship.api.GetAllInBubbleRequest", ship_get_all_in_bubble);
    reg("eve_public.cosmetic.corporation.palette.api.GetRequest", palette_get);
    reg("eve_public.cosmetic.corporation.palette.api.GetOwnRequest", palette_get_own);
    reg("eve_public.cosmetic.corporation.palette.api.SetRequest", palette_set);
    reg("eve_public.cosmetic.corporation.palette.api.CanEditRequest", palette_can_edit);
    reg("eve_public.cosmetic.structure.paintwork.api.GetRequest", paintwork_get);
    reg("eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest", paintwork_get_all_in_system);
    reg("eve_public.cosmetic.structure.paintwork.license.api.GetRequest", license_get);
    reg("eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest", license_get_all_owned);
    reg("eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest", license_revoke_by_corp);
    reg("eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", license_issue);
    reg("eve_public.cosmetic.structure.paintwork.license.api.GetCatalogueRequest", license_get_catalogue);
    reg("eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest", license_admin_issue);
    reg("eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest", license_admin_revoke);
    super::quasar_cosmetics_market::register(router, Arc::clone(&state.db), Arc::clone(&state.sde), skinr_state);
}

/// Validates a logo request: caller owns the ship and the slot index is non-negative.
fn logo_target(st: &CosmeticsLiveState, cid: u32, id: Option<&DynamicQuasarMessage>) -> Result<(u64, i32), (u32, String)> {
    let id = id.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing ship logo identifier".to_string()))?;
    let ship = seq_of(id, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing ship identifier".to_string()))?;
    let index = match id.get(2) {
        Some(DynamicValue::Int32(i)) => *i,
        None => 0,
        _ => return Err((STATUS_BAD_REQUEST, "Invalid logo slot index".to_string())),
    };
    if index < 0 {
        return Err((STATUS_BAD_REQUEST, "Logo slot index must be non-negative".to_string()));
    }
    st.db.with_snapshot(|s| {
        let item = s.items.get(&(ship as i64)).filter(|i| st.sde.types.get(&i.type_id).map(|t| t.category_id) == Some(CATEGORY_SHIP));
        match item {
            None => Err((STATUS_NOT_FOUND, format!("Ship {ship} not found"))),
            Some(i) if i.owner_id != cid as i64 => Err((STATUS_FORBIDDEN, format!("Ship {ship} is not yours"))),
            Some(_) => Ok((ship, index)),
        }
    })
}

fn logo_display(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let (ship, index) = logo_target(st, cid, sub(msg, 1))?;
    let attr = sub(msg, 2).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing logo attributes".to_string()))?;
    let kind = if attr.get(1).is_some() {
        // Alliance emblems are only available to pilots whose corporation is in an alliance.
        let in_alliance = st.db.with_snapshot(|s| s.characters.get(&(cid as i64)).is_some_and(|c| c.alliance_id.is_some()));
        if !in_alliance {
            return Err((STATUS_FORBIDDEN, "Not a member of an alliance".to_string()));
        }
        1
    } else if attr.get(2).is_some() {
        2
    } else {
        return Err((STATUS_BAD_REQUEST, "Logo type must be alliance or corporation".to_string()));
    };
    st.data.write().unwrap_or_else(|e| e.into_inner()).ship_logos.entry(ship).or_default().insert(index, kind);
    st.save();
    Ok(any("eve_public.cosmetic.ship.logo.DisplayResponse", Vec::new()))
}

fn logo_clear(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let (ship, index) = logo_target(st, cid, sub(msg, 1))?;
    {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        if let Some(slots) = data.ship_logos.get_mut(&ship) {
            slots.remove(&index);
            if slots.is_empty() {
                data.ship_logos.remove(&ship);
            }
        }
    }
    st.save();
    Ok(any("eve_public.cosmetic.ship.logo.ClearResponse", Vec::new()))
}

fn ship_get(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    require_char(ctx)?;
    let ship = seq_of(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing ship identifier".to_string()))?;
    let state = st.db.with_snapshot(|s| ship_state(s, &st.sde, ship)).ok_or_else(|| (STATUS_NOT_FOUND, format!("Ship {ship} not found")))?;
    let mut out = Vec::new();
    encode_bytes_field(1, &state, &mut out);
    Ok(any("eve_public.cosmetic.ship.api.GetResponse", out))
}

fn ship_get_all_in_bubble(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, _msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let states = st.db.with_snapshot(|s| {
        // Docked pilots have no bubble; in-space pilots see every ship in their system.
        let ch = s.characters.get(&(cid as i64)).filter(|c| c.station_id.is_none() && c.structure_id.is_none());
        ch.map(|c| ship_states_in_space(s, &st.sde, c.solar_system_id)).unwrap_or_default()
    });
    let mut out = Vec::new();
    for state in &states {
        encode_bytes_field(1, state, &mut out);
    }
    Ok(any("eve_public.cosmetic.ship.api.GetAllInBubbleResponse", out))
}

fn palette_get(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    require_char(ctx)?;
    let corp = sub(msg, 1).and_then(|m| seq_of(m, 1)).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation identifier".to_string()))? as u32;
    let p = st.data.read().unwrap_or_else(|e| e.into_inner()).palettes.get(&corp).cloned();
    let p = p.ok_or_else(|| (STATUS_NOT_FOUND, format!("Corporation {corp} has no palette")))?;
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_palette(&p), &mut out);
    Ok(any("eve_public.cosmetic.corporation.palette.api.GetResponse", out))
}

fn palette_get_own(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, _msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let corp = st.db.with_snapshot(|s| char_corp(s, cid)).ok_or_else(|| (STATUS_NOT_FOUND, "Character not found".to_string()))?;
    let p = st.data.read().unwrap_or_else(|e| e.into_inner()).palettes.get(&corp).cloned();
    let p = p.ok_or_else(|| (STATUS_NOT_FOUND, format!("Corporation {corp} has no palette")))?;
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_palette(&p), &mut out);
    put_seq(2, p.last_modifier as u64, &mut out);
    put_seconds(3, p.last_modified_unix, &mut out);
    Ok(any("eve_public.cosmetic.corporation.palette.api.GetOwnResponse", out))
}

fn palette_set(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let (corp, allowed) = st.db.with_snapshot(|s| {
        let corp = char_corp(s, cid);
        (corp, corp.is_some_and(|c| can_edit_palette(ctx, s, cid, c)))
    });
    let corp = corp.ok_or_else(|| (STATUS_NOT_FOUND, "Character not found".to_string()))?;
    if !allowed {
        return Err((STATUS_FORBIDDEN, "Only the CEO, directors or brand managers can set corporation colors".to_string()));
    }
    let attrs = sub(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing palette attributes".to_string()))?;
    let main = parse_rgb(sub(attrs, 1))?.ok_or_else(|| (STATUS_BAD_REQUEST, "Main color is required".to_string()))?;
    let record = PaletteRecord {
        main,
        secondary: parse_rgb(sub(attrs, 2))?,
        tertiary: parse_rgb(sub(attrs, 4))?,
        last_modifier: cid,
        last_modified_unix: ctx.now_unix(),
    };
    st.data.write().unwrap_or_else(|e| e.into_inner()).palettes.insert(corp, record);
    st.save();
    Ok(any("eve_public.cosmetic.corporation.palette.api.SetResponse", Vec::new()))
}

fn palette_can_edit(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, _msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let can = st.db.with_snapshot(|s| char_corp(s, cid).is_some_and(|c| can_edit_palette(ctx, s, cid, c)));
    let mut out = Vec::new();
    encode_varint_field(1, can as u64, &mut out);
    Ok(any("eve_public.cosmetic.corporation.palette.api.CanEditResponse", out))
}

fn paintwork_get(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    require_char(ctx)?;
    let structure = seq_of(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing structure identifier".to_string()))?;
    let lic = st.data.read().unwrap_or_else(|e| e.into_inner()).paint_licenses.values().find(|l| l.structure_id == structure).cloned();
    let lic = lic.ok_or_else(|| (STATUS_NOT_FOUND, format!("Structure {structure} has no paintwork")))?;
    let mut out = Vec::new();
    encode_bytes_field(1, &uuid_msg(&lic.uuid), &mut out);
    encode_bytes_field(2, &encode_slots(&lic.slots), &mut out);
    Ok(any("eve_public.cosmetic.structure.paintwork.api.GetResponse", out))
}

fn paintwork_get_all_in_system(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, _msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let system = st.db.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.solar_system_id));
    let system = system.ok_or_else(|| (STATUS_NOT_FOUND, "Character not found".to_string()))?;
    let mut out = Vec::new();
    let data = st.data.read().unwrap_or_else(|e| e.into_inner()).clone();
    for lic in licenses_in_system(&data, system) {
        let mut entry = Vec::new();
        put_seq(1, lic.structure_id, &mut entry);
        encode_bytes_field(2, &encode_slots(&lic.slots), &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    Ok(any("eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemResponse", out))
}

fn license_get(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let uuid = uuid_of(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing license identifier".to_string()))?;
    let corp = st.db.with_snapshot(|s| char_corp(s, cid));
    let lic = st.data.read().unwrap_or_else(|e| e.into_inner()).paint_licenses.get(&hex(&uuid)).cloned();
    // Licenses are visible only to the owning corporation.
    let lic = lic.filter(|l| Some(l.corporation_id) == corp).ok_or_else(|| (STATUS_NOT_FOUND, "License not found".to_string()))?;
    let mut out = Vec::new();
    encode_bytes_field(2, &encode_license_attrs(&lic), &mut out);
    Ok(any("eve_public.cosmetic.structure.paintwork.license.api.GetResponse", out))
}

fn license_get_all_owned(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, _msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let corp = st.db.with_snapshot(|s| char_corp(s, cid)).ok_or_else(|| (STATUS_NOT_FOUND, "Character not found".to_string()))?;
    let mut lics: Vec<PaintworkLicense> = st
        .data
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .paint_licenses
        .values()
        .filter(|l| l.corporation_id == corp)
        .cloned()
        .collect();
    lics.sort_by_key(|l| l.structure_id);
    let mut out = Vec::new();
    for l in &lics {
        let mut entry = Vec::new();
        encode_bytes_field(1, &uuid_msg(&l.uuid), &mut entry);
        encode_bytes_field(2, &encode_license_attrs(l), &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    Ok(any("eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationResponse", out))
}

/// Removes a license; the structure reverts to its default (all-empty) paintwork.
fn revoke(st: &CosmeticsLiveState, uuid: &[u8; 16], owner_corp: Option<u32>, now: u64) -> Result<(), (u32, String)> {
    let removed = {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        let key = hex(uuid);
        match data.paint_licenses.get(&key) {
            None => return Err((STATUS_NOT_FOUND, "License not found".to_string())),
            Some(l) if owner_corp.is_some_and(|c| c != l.corporation_id) => {
                return Err((STATUS_FORBIDDEN, "License belongs to another corporation".to_string()))
            }
            Some(_) => data.paint_licenses.remove(&key),
        }
    };
    st.save();
    if let Some(l) = removed {
        queue_structure_paintwork(l.structure_id, l.solar_system_id, &[None; 7], now);
    }
    Ok(())
}

fn license_revoke_by_corp(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let uuid = uuid_of(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing license identifier".to_string()))?;
    let roles = ctx.identity.as_ref().and_then(|id| id.roles.as_ref()).map_or(0, |r| r.bitmask);
    let (corp, is_ceo) = st.db.with_snapshot(|s| {
        let corp = char_corp(s, cid);
        (corp, corp.is_some_and(|c| s.corporations.get(&(c as i64)).is_some_and(|r| r.ceo_id == cid as i64)))
    });
    if !is_ceo && roles & eve_db::corp_roles::ROLE_DIRECTOR == 0 {
        return Err((STATUS_FORBIDDEN, "Director role required to revoke paintwork licenses".to_string()));
    }
    revoke(st, &uuid, Some(corp.unwrap_or(0)), ctx.now_unix())?;
    Ok(any("eve_public.cosmetic.structure.paintwork.license.api.RevokeResponse", Vec::new()))
}

pub const PARAGON_CORP_ID: i64 = 1_000_419;

pub const CATALOGUE_ENTRIES: &[(u32, u64, u64)] = &[
    (35832, 2_592_000, 1_000),
    (35832, 7_776_000, 2_500),
    (35833, 2_592_000, 2_000),
    (35834, 2_592_000, 5_000),
];

pub fn get_catalogue_price(structure_type: u32, duration_secs: u64) -> Option<u64> {
    CATALOGUE_ENTRIES
        .iter()
        .find(|(t, d, _)| *t == structure_type && *d == duration_secs)
        .map(|(_, _, cost)| *cost)
}

fn license_issue(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    let cid = require_char(ctx)?;
    let slots = parse_slots(sub(msg, 1))?;
    let duration_secs = sub(msg, 2).and_then(|d| d.get(1)).and_then(as_u64).unwrap_or(0);
    if duration_secs == 0 {
        return Err((STATUS_BAD_REQUEST, "License duration must be positive".to_string()));
    }
    let structures: Vec<u64> = match msg.get(3) {
        Some(DynamicValue::Repeated(v)) => v
            .iter()
            .filter_map(|x| match x {
                DynamicValue::Message(m) => m.get(1).and_then(as_u64),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if structures.is_empty() {
        return Err((STATUS_BAD_REQUEST, "At least one structure is required".to_string()));
    }
    let is_admin = require_admin_role(ctx).is_ok();
    let (targets, total_cost) = st.db.with_snapshot(|s| {
        let caller_corp = char_corp(s, cid).unwrap_or(0);
        let mut total_cost: u64 = 0;
        let targets = structures
            .iter()
            .map(|id| {
                let r = s.structures
                    .get(&(*id as i64))
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Structure {id} not found")))?;
                let corp = r.owner_corp_id as u32;
                if !is_admin && corp != caller_corp {
                    return Err((STATUS_FORBIDDEN, format!("Structure {id} is not owned by your corporation")));
                }
                let price = get_catalogue_price(r.type_id, duration_secs).ok_or_else(|| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("No paintwork license in catalogue for structure type {} with duration {}s", r.type_id, duration_secs),
                    )
                })?;
                total_cost = total_cost.saturating_add(price);
                Ok((*id, corp, r.solar_system_id))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, (u32, String)>((targets, total_cost))
    })?;
    let roles = ctx.identity.as_ref().and_then(|id| id.roles.as_ref()).map_or(0, |r| r.bitmask);
    let is_ceo = st.db.with_snapshot(|s| {
        let corp = char_corp(s, cid);
        corp.is_some_and(|c| s.corporations.get(&(c as i64)).is_some_and(|r| r.ceo_id == cid as i64))
    });
    if !is_admin && !is_ceo && roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER) == 0 {
        return Err((STATUS_FORBIDDEN, "Director or Station Manager role required to issue paintwork licenses".to_string()));
    }
    if !is_admin && total_cost > 0 {
        st.db.try_with_mut_snapshot(|s| -> Result<(), String> {
            let caller_corp = char_corp(s, cid).unwrap_or(0) as i64;
            let corp_key = (caller_corp, PARAGON_CORP_ID);
            let corp_lp = s.corp_lp_balances.get(&corp_key).copied().unwrap_or(0);
            let char_lp = s.characters.get(&(cid as i64)).and_then(|c| c.loyalty_points.get(&PARAGON_CORP_ID).copied()).unwrap_or(0);

            if corp_lp >= total_cost as i64 {
                *s.corp_lp_balances.entry(corp_key).or_insert(0) -= total_cost as i64;
                Ok(())
            } else if char_lp >= total_cost as i64 {
                let ch = s.characters.get_mut(&(cid as i64)).expect("char exists");
                *ch.loyalty_points.entry(PARAGON_CORP_ID).or_insert(0) -= total_cost as i64;
                Ok(())
            } else {
                Err(format!("Insufficient EverMarks for paintwork license (requires {total_cost} Paragon LP)"))
            }
        }).map_err(|e| (STATUS_FORBIDDEN, e))?;
    }
    let now = ctx.now_unix();
    let mut issued = Vec::new();
    {
        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
        for (structure_id, corp, system) in targets {
            // A structure carries one paintwork: a new license replaces the previous one.
            data.paint_licenses.retain(|_, l| l.structure_id != structure_id);
            data.next_license_seq += 1;
            let mut uuid = [0u8; 16];
            uuid[0..8].copy_from_slice(b"PAINTLIC");
            uuid[8..16].copy_from_slice(&data.next_license_seq.to_be_bytes());
            let lic = PaintworkLicense { uuid, corporation_id: corp, activator_id: cid, issued_unix: now, duration_secs, structure_id, solar_system_id: system, slots };
            data.paint_licenses.insert(hex(&uuid), lic.clone());
            issued.push(lic);
        }
    }
    st.save();
    let mut out = Vec::new();
    for lic in &issued {
        queue_structure_paintwork(lic.structure_id, lic.solar_system_id, &lic.slots, now);
        let mut entry = Vec::new();
        encode_bytes_field(1, &uuid_msg(&lic.uuid), &mut entry);
        encode_bytes_field(2, &encode_license_attrs(lic), &mut entry);
        put_seq(3, lic.structure_id, &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    Ok(any("eve_public.cosmetic.structure.paintwork.license.api.IssueResponse", out))
}

fn license_admin_issue(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    require_admin_role(ctx)?;
    let mut res = license_issue(st, ctx, msg)?;
    res.type_url = format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.structure.paintwork.license.api.admin.IssueResponse");
    Ok(res)
}

fn license_get_catalogue(_st: &CosmeticsLiveState, _ctx: &AuthoritativeContext, _msg: &DynamicQuasarMessage) -> HandlerResult {
    let mut out = Vec::new();
    for &(st_type, duration_secs, lp_cost) in CATALOGUE_ENTRIES {
        let mut item = Vec::new();
        put_seq(1, st_type as u64, &mut item);
        let mut dur = Vec::new();
        encode_varint_field(1, duration_secs, &mut dur);
        encode_bytes_field(2, &dur, &mut item);
        let mut price = Vec::new();
        encode_varint_field(1, lp_cost, &mut price);
        encode_bytes_field(3, &price, &mut item);
        encode_bytes_field(1, &item, &mut out);
    }
    Ok(any("eve_public.cosmetic.structure.paintwork.license.api.GetCatalogueResponse", out))
}

fn license_admin_revoke(st: &CosmeticsLiveState, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> HandlerResult {
    require_admin_role(ctx)?;
    let uuid = uuid_of(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing license identifier".to_string()))?;
    revoke(st, &uuid, None, ctx.now_unix())?;
    Ok(any("eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeResponse", Vec::new()))
}
