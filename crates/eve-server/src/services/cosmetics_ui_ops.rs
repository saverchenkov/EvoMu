//! `cosmetics_ui_ops` — SKIN license bookkeeping (licence listing with expiry, timed-licence
//! expiry with `OnSkinLicenseRemoved`) and client UI persistence services (overview preset
//! links, shared setting links, station guests, achievements, contextual-offer tracking,
//! NES intro state). Only V24 client RPC names are dispatched; everything else returns `None`
//! so the router answers `UnimplementedRPC`.
#![forbid(unsafe_code)]
use crate::router::ServerContext;
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const COSMETICS_UI_STATE_KEY: i64 = -7_700_002;
/// Contextual-offer tracking log is capped per character so it cannot grow unbounded.
const MAX_TRACKED_OFFER_EVENTS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinLicenseEntry {
    pub skin_id: u32,
    pub license_type_id: u32,
    pub activated_at_unix: u64,
    /// `None` = permanent licence.
    pub expires_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLinkRecord {
    pub owner_char_id: i64,
    pub name: String,
    /// Opaque client payload, stored verbatim and returned unchanged.
    pub data: PyRep,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CosmeticsUiExtendedState {
    pub skin_license_entries: HashMap<i64, Vec<SkinLicenseEntry>>,
    pub next_link_id: i64,
    pub overview_presets: HashMap<i64, StoredLinkRecord>,
    pub shared_settings: HashMap<i64, StoredLinkRecord>,
    pub achievements_by_char: HashMap<i64, Vec<i64>>,
    pub achievement_counters_by_char: HashMap<i64, HashMap<String, i64>>,
    pub contextual_offer_events: HashMap<i64, Vec<(String, i64, u64)>>,
    pub nes_intro_skipped: HashMap<i64, u64>,
    pub opened_crates: HashMap<i64, u64>,
    pub claimed_crate_loot: HashMap<i64, Vec<i64>>,
    pub claimed_milestones: HashMap<i64, Vec<i64>>,
    pub login_campaign_claimed_days: HashMap<i64, Vec<u64>>,
    pub seasonal_claimed_days: HashMap<i64, Vec<u64>>,
}

pub fn load_cosmetics_ui_state(snap: &WorldStateSnapshot) -> CosmeticsUiExtendedState {
    snap.item_custom_names
        .get(&COSMETICS_UI_STATE_KEY)
        .and_then(|raw| serde_json::from_str::<CosmeticsUiExtendedState>(raw).ok())
        .unwrap_or_default()
}

pub fn save_cosmetics_ui_state(snap: &mut WorldStateSnapshot, state: &CosmeticsUiExtendedState) {
    if let Ok(json) = serde_json::to_string(state) {
        snap.item_custom_names.insert(COSMETICS_UI_STATE_KEY, json);
    }
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// Records a licence activation; `duration_days <= 0` (SDE `-1`) means permanent.
pub fn record_skin_license_activation(snap: &mut WorldStateSnapshot, char_id: i64, license_type_id: u32, skin_id: u32, duration_days: i32, now_unix: u64) -> Option<u64> {
    let mut ext = load_cosmetics_ui_state(snap);
    let entries = ext.skin_license_entries.entry(char_id).or_default();
    let existing_exp = entries.iter().find(|e| e.skin_id == skin_id).map(|e| e.expires_at_unix);
    let expires_at = if duration_days > 0 {
        let add = u64::from(duration_days.unsigned_abs()) * 86_400;
        match existing_exp {
            // A permanent licence is never downgraded by a timed one.
            Some(None) => None,
            // Stacking a timed licence on an active timed licence extends it.
            Some(Some(exp)) if exp > now_unix => Some(exp.saturating_add(add)),
            _ => Some(now_unix.saturating_add(add)),
        }
    } else {
        None
    };
    entries.retain(|e| e.skin_id != skin_id);
    entries.push(SkinLicenseEntry { skin_id, license_type_id, activated_at_unix: now_unix, expires_at_unix: expires_at });
    save_cosmetics_ui_state(snap, &ext);
    expires_at
}

/// Removes expired timed licences for `char_id`, strips them from `licensed_skins`, and
/// reverts any of the character's ships wearing them to the default skin. Returns the
/// expired SKIN IDs so the caller can push `OnSkinLicenseRemoved` after releasing the lock.
pub fn purge_expired_skins_for_char(snap: &mut WorldStateSnapshot, char_id: i64, now_unix: u64) -> Vec<u32> {
    let mut ext = load_cosmetics_ui_state(snap);
    let mut expired: Vec<SkinLicenseEntry> = Vec::new();
    if let Some(entries) = ext.skin_license_entries.get_mut(&char_id) {
        entries.retain(|e| match e.expires_at_unix {
            Some(exp) if now_unix >= exp => {
                expired.push(e.clone());
                false
            }
            _ => true,
        });
    }
    if expired.is_empty() {
        return Vec::new();
    }
    let removed_ids: Vec<u32> = expired.iter().flat_map(|e| [e.skin_id, e.license_type_id]).collect();
    if let Some(lics) = snap.licensed_skins.get_mut(&char_id) {
        lics.retain(|id| !removed_ids.contains(id));
    }
    let char_ships: Vec<i64> = snap.items.values().filter(|i| i.owner_id == char_id).map(|i| i.item_id).collect();
    for sid in char_ships {
        if snap.applied_ship_skins.get(&sid).map(|applied| removed_ids.contains(applied)).unwrap_or(false) {
            snap.applied_ship_skins.remove(&sid);
        }
    }
    save_cosmetics_ui_state(snap, &ext);
    expired.into_iter().map(|e| e.skin_id).collect()
}

/// Pushes `OnSkinLicenseRemoved` for every expired SKIN (call outside any snapshot lock).
pub fn notify_skins_removed(session: &mut SessionState, expired: &[u32]) {
    for &skin_id in expired {
        session.push_notification("OnSkinLicenseRemoved", vec![PyRep::Integer(i64::from(skin_id))]);
    }
}

pub async fn handle(ctx: &ServerContext, session: &mut SessionState, service: &str, method: &str, args: &[PyRep], _kwargs: &HashMap<String, PyRep>) -> Option<Result<PyRep, BridgeRpcError>> {
    if (service, method) == ("shipCosmeticsMgr", "ActivateSkinLicense") {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(raw_skin_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(err("InvalidSkinLicenseType", "ActivateSkinLicense requires skin license typeID")));
        };
        let Ok(requested_skin_id) = u32::try_from(raw_skin_id) else {
            return Some(Err(err("InvalidSkinLicenseType", format!("Invalid SKIN license typeID {raw_skin_id}"))));
        };
        let Some(skin_type) = ctx.sde.types.get(&requested_skin_id) else {
            return Some(Err(err("InvalidSkinLicenseType", format!("Type {requested_skin_id} not found in SDE"))));
        };
        if skin_type.category_id != 91 {
            return Some(Err(err(
                "InvalidSkinLicenseType",
                format!("Type {requested_skin_id} (category {}) is not a Permanent SKIN license (category 91)", skin_type.category_id),
            )));
        }
        let Some(lic_info) = ctx.sde.skin_licenses.get(&requested_skin_id) else {
            return Some(Err(err("InvalidSkinLicenseType", format!("Type {requested_skin_id} is not mapped to any SKIN in SDE skin_licenses"))));
        };
        let Some(skin_info) = ctx.sde.skins.get(&lic_info.skin_id) else {
            return Some(Err(err(
                "InvalidSkinLicenseType",
                format!("SKIN ID {} mapped by license {requested_skin_id} does not exist in SDE skins", lic_info.skin_id),
            )));
        };
        if let Some(ship_tid) = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0) {
            if !skin_info.types.contains(&(ship_tid as u32)) {
                return Some(Err(err(
                    "InvalidShipTypeForSkin",
                    format!("SKIN license {requested_skin_id} ('{}') is not compatible with ship type {ship_tid}", skin_info.internal_name),
                )));
            }
        }
        let current_dock = session.station_id.or(session.structure_id);
        let current_ship = session.ship_id;
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let license_item_id = snap
                .items
                .values()
                .find(|i| i.type_id == requested_skin_id && i.quantity >= 1 && snap.is_item_consumable_by(i, char_id, current_dock, current_ship))
                .map(|i| i.item_id);
            let Some(item_id) = license_item_id else {
                return Err(err(
                    "SkinLicenseItemNotFound",
                    format!("Character {char_id} does not own SKIN license item (typeID {requested_skin_id}) at current station/structure or active ship"),
                ));
            };
            if let Some(item) = snap.items.get_mut(&item_id) {
                if item.quantity > 1 {
                    item.quantity -= 1;
                    item.version += 1;
                } else {
                    snap.items.remove(&item_id);
                }
            }
            let list = snap.licensed_skins.entry(char_id).or_default();
            if !list.contains(&requested_skin_id) {
                list.push(requested_skin_id);
            }
            if !list.contains(&lic_info.skin_id) {
                list.push(lic_info.skin_id);
            }
            let exp = record_skin_license_activation(snap, char_id, requested_skin_id, lic_info.skin_id, lic_info.duration, ctx.now_unix());
            Ok((requested_skin_id, exp))
        });
        return match res {
            Ok((skin_id, exp_opt)) => {
                session.push_notification("OnSkinLicenseActivated", vec![PyRep::Integer(skin_id as i64), PyRep::Integer(lic_info.duration as i64)]);
                Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("skinID", PyRep::Integer(skin_id as i64)),
                    ("skinHashID", PyRep::Integer(skin_id as i64)),
                    ("durationDays", PyRep::Integer(lic_info.duration as i64)),
                    ("expiresAtUnix", exp_opt.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
                    ("licensed", PyRep::Bool(true)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }
    if (service, method) == ("shipCosmeticsMgr", "ApplySkinToShip") {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(raw_ship_type) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(err("InvalidShipTypeForSkin", "ApplySkinToShip requires shipTypeID")));
        };
        let Ok(ship_type_id) = u32::try_from(raw_ship_type) else {
            return Some(Err(err("InvalidShipTypeForSkin", format!("Invalid ship typeID {raw_ship_type}"))));
        };
        if !ctx.sde.types.get(&ship_type_id).map(|t| t.category_id == 6).unwrap_or(false) {
            return Some(Err(err("InvalidShipTypeForSkin", format!("Type {ship_type_id} is not a valid ship type in SDE"))));
        }
        let (target_skin_id, resolved_skin_id, design_name) = match args.get(1) {
            Some(PyRep::Integer(id)) => {
                let Ok(sid) = u32::try_from(*id) else {
                    return Some(Err(err("SkinNotLicensed", format!("Invalid SKIN ID {id}"))));
                };
                if let Some(lic) = ctx.sde.skin_licenses.get(&sid) {
                    let name = ctx.sde.types.get(&sid).map(|t| t.name.clone()).unwrap_or_else(|| format!("SKIN {sid}"));
                    (sid, lic.skin_id, name)
                } else if let Some(skin) = ctx.sde.skins.get(&sid) {
                    (sid, sid, skin.internal_name.clone())
                } else {
                    return Some(Err(err("SkinNotLicensed", format!("SKIN ID {sid} is not a valid SDE SKIN"))));
                }
            }
            Some(PyRep::String(name)) => {
                let found = ctx.sde.types.values().find(|t| t.category_id == 91 && t.name.eq_ignore_ascii_case(name));
                let Some(st) = found else {
                    return Some(Err(err("SkinNotLicensed", format!("Character {char_id} has not activated a license for SKIN '{name}'"))));
                };
                let Some(lic) = ctx.sde.skin_licenses.get(&st.type_id) else {
                    return Some(Err(err("SkinNotLicensed", format!("Type {} is not mapped to any SDE SKIN", st.type_id))));
                };
                (st.type_id, lic.skin_id, st.name.clone())
            }
            _ => return Some(Err(err("SkinNotLicensed", "ApplySkinToShip requires a valid SKIN ID or design name"))),
        };
        let Some(skin_info) = ctx.sde.skins.get(&resolved_skin_id) else {
            return Some(Err(err("SkinNotLicensed", format!("Resolved SKIN ID {resolved_skin_id} not found in SDE skins"))));
        };
        if !skin_info.types.contains(&ship_type_id) {
            return Some(Err(err(
                "InvalidShipTypeForSkin",
                format!("SKIN '{}' (id {resolved_skin_id}) cannot be applied to ship type {ship_type_id}", skin_info.internal_name),
            )));
        }
        let now_unix = ctx.now_unix();
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let expired = purge_expired_skins_for_char(snap, char_id, now_unix);
            let active_ship = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0);
            let ship_item = snap.items.get(&active_ship).ok_or_else(|| err("ShipNotFound", format!("Active ship {active_ship} not found")))?;
            if ship_item.owner_id != char_id {
                return Err(err("OwnershipViolation", format!("Ship {active_ship} is not owned by character {char_id}")));
            }
            if ship_item.type_id != ship_type_id {
                return Err(err("ShipTypeMismatch", format!("Active ship {active_ship} has type {}, cannot apply skin for type {ship_type_id}", ship_item.type_id)));
            }
            let is_licensed = snap.licensed_skins.get(&char_id).map(|list| list.contains(&target_skin_id) || list.contains(&resolved_skin_id)).unwrap_or(false);
            if !is_licensed {
                return Err(err("SkinNotLicensed", format!("Character {char_id} has not activated a license for SKIN '{design_name}' ({target_skin_id})")));
            }
            snap.applied_ship_skins.insert(active_ship, target_skin_id);
            Ok((active_ship, target_skin_id, expired))
        });
        return match res {
            Ok((active_ship, skin_id, expired)) => {
                notify_skins_removed(session, &expired);
                session.push_notification("OnCurrentShipSkinChange", vec![PyRep::Integer(active_ship), PyRep::Integer(skin_id as i64)]);
                Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(active_ship)),
                    ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                    ("skinID", PyRep::Integer(skin_id as i64)),
                    ("designName", PyRep::String(design_name)),
                    ("sequenced", PyRep::Bool(true)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }
    match (service, method) {
        // parity: feat:admin.gm-skin-tools
        ("shipCosmeticsMgr", "RemoveSkin") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(err("InsufficientRole", "shipCosmeticsMgr.RemoveSkin requires GM role")));
            }
            let skin_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u32;
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.licensed_skins.get_mut(&char_id) {
                    list.retain(|&id| id != skin_id);
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }
        // parity: feat:admin.gm-skin-tools
        ("shipCosmeticsMgr", "GMExpireSkinLicense") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(err("InsufficientRole", "shipCosmeticsMgr.GMExpireSkinLicense requires GM role")));
            }
            let skin_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u32;
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.licensed_skins.get_mut(&char_id) {
                    list.retain(|&id| id != skin_id);
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }
        // parity: feat:cosmetics.skin-licenses
        ("shipCosmeticsMgr", "GetLicencedSkins") => Some(handle_get_licensed_skins(ctx, session, None)),
        ("shipCosmeticsMgr", "GetLicencedSkinsForShipType") => {
            let ship_type = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok());
            match ship_type {
                Some(t) if ctx.sde.types.get(&t).map(|ty| ty.category_id == 6).unwrap_or(false) => Some(handle_get_licensed_skins(ctx, session, Some(t))),
                _ => Some(Err(err("InvalidShipTypeForSkin", "GetLicencedSkinsForShipType requires a ship typeID present in the SDE"))),
            }
        }
        // parity: feat:ui.overview-presets
        ("overviewPresetMgr", "StoreLinkAndGetID") => Some(handle_store_link(ctx, session, args, LinkKind::OverviewPreset)),
        ("overviewPresetMgr", "GetStoredPreset") => Some(handle_get_link(ctx, session, args, LinkKind::OverviewPreset)),
        // parity: feat:ui.shared-settings
        ("sharedSettingsMgr", "StoreSettingLinkAndGetID") => Some(handle_store_link(ctx, session, args, LinkKind::SharedSetting)),
        ("sharedSettingsMgr", "GetStoredSharedSetting") => Some(handle_get_link(ctx, session, args, LinkKind::SharedSetting)),
        // parity: feat:ui.station-guests
        ("station", "GetGuests") => Some(handle_station_get_guests(ctx, session)),
        // parity: feat:ui.achievements
        ("monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount") => Some(handle_achievements_get(ctx, session)),
        ("monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters") => Some(handle_achievements_update(ctx, session, args)),
        // parity: feat:ui.contextual-offers
        ("contextualOfferMgr", "TrackOfferBuyPress")
        | ("contextualOfferMgr", "TrackOfferClosed")
        | ("contextualOfferMgr", "TrackOfferDelivered")
        | ("contextualOfferMgr", "TrackOfferPurchased")
        | ("contextualOfferMgr", "TrackOfferPurchasedSeen")
        | ("contextualOfferMgr", "TrackOfferSeen") => Some(handle_track_offer(ctx, session, method, args)),
        // parity: feat:ui.nes-intro
        ("nes_intro", "get_nes_intro_state") => Some(handle_nes_intro_get(ctx, session)),
        ("nes_intro", "skip_nes_intro") => Some(handle_nes_intro_skip(ctx, session)),
        // parity: feat:pve.crates
        ("crateService", "OpenCrate") => Some(handle_crate_open(ctx, session, args)),
        ("crateService", "ClaimLoot") => Some(handle_crate_claim_loot(ctx, session, args)),
        ("crateService", "ClaimSkillBundle") => Some(handle_crate_claim_skill_bundle(ctx, session, args)),
        // parity: feat:pve.milestones
        ("milestoneMgr", "ClaimRewards") => Some(handle_milestone_claim_rewards(ctx, session, args)),
        ("milestoneMgr", "ProcessCharacterLogon") => Some(handle_milestone_process_logon(ctx, session)),
        // parity: feat:pve.login-campaign
        ("loginCampaignManager", "get_client_campaign_state") => Some(handle_login_campaign_get_state(ctx, session)),
        ("loginCampaignManager", "claim_reward") => Some(handle_login_campaign_claim_reward(ctx, session)),
        // parity: feat:pve.seasonal-login
        ("seasonalLoginCampaignManager", "get_active_campaign") => Some(handle_seasonal_login_get_active_campaign(ctx, session)),
        ("seasonalLoginCampaignManager", "claim_reward") => Some(handle_seasonal_login_claim_reward(ctx, session)),
        ("seasonalLoginCampaignManager", "get_claim_history") => Some(handle_seasonal_login_get_claim_history(ctx, session)),
        _ => None,
    }
}

fn handle_get_licensed_skins(ctx: &ServerContext, session: &mut SessionState, ship_type: Option<u32>) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let (rows, expired) = ctx.db.try_with_mut_snapshot(|snap| {
        let expired = purge_expired_skins_for_char(snap, char_id, now_unix);
        let ext = load_cosmetics_ui_state(snap);
        let entries = ext.skin_license_entries.get(&char_id).cloned().unwrap_or_default();
        let licensed_raw = snap.licensed_skins.get(&char_id).cloned().unwrap_or_default();
        let mut seen = Vec::new();
        let mut rows = Vec::new();
        for sid in licensed_raw {
            let skin = ctx.sde.skins.get(&sid).or_else(|| ctx.sde.skin_licenses.get(&sid).and_then(|l| ctx.sde.skins.get(&l.skin_id)));
            let Some(skin) = skin else { continue };
            if seen.contains(&skin.skin_id) {
                continue;
            }
            if let Some(t) = ship_type {
                if !skin.types.contains(&t) {
                    continue;
                }
            }
            seen.push(skin.skin_id);
            let exp = entries.iter().find(|e| e.skin_id == skin.skin_id).and_then(|e| e.expires_at_unix);
            rows.push(PyRep::keyval([
                ("skinID", PyRep::Integer(i64::from(skin.skin_id))),
                ("skinMaterialID", PyRep::Integer(i64::from(skin.skin_material_id))),
                ("isPermanent", PyRep::Bool(exp.is_none())),
                ("expires", exp.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
            ]));
        }
        Ok::<_, BridgeRpcError>((rows, expired))
    })?;
    notify_skins_removed(session, &expired);
    Ok(PyRep::List(rows))
}

#[derive(Clone, Copy)]
enum LinkKind {
    OverviewPreset,
    SharedSetting,
}

fn link_map(ext: &mut CosmeticsUiExtendedState, kind: LinkKind) -> &mut HashMap<i64, StoredLinkRecord> {
    match kind {
        LinkKind::OverviewPreset => &mut ext.overview_presets,
        LinkKind::SharedSetting => &mut ext.shared_settings,
    }
}

/// `StoreLinkAndGetID(name, data)` / `StoreSettingLinkAndGetID(name, data)`: persists the
/// payload under a fresh server-wide link ID and returns it.
fn handle_store_link(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep], kind: LinkKind) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let name = args.first().and_then(|a| a.as_str()).map(str::trim).unwrap_or("").to_string();
    if name.is_empty() {
        return Err(err("InvalidLinkName", "A shared link requires a non-empty name"));
    }
    let data = args.get(1).filter(|d| !matches!(d, PyRep::None)).cloned().ok_or_else(|| err("EmptyLinkPayload", "A shared link requires a payload"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.next_link_id += 1;
        let link_id = ext.next_link_id;
        link_map(&mut ext, kind).insert(link_id, StoredLinkRecord { owner_char_id: char_id, name, data });
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::Integer(link_id))
    })
}

/// `GetStoredPreset(linkID)` / `GetStoredSharedSetting(linkID)`: any character holding the
/// link receives `(name, data)`.
fn handle_get_link(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep], kind: LinkKind) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let link_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingLinkId", "Stored link lookup requires a link ID"))?;
    ctx.db.with_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let rec = link_map(&mut ext, kind).get(&link_id).cloned().ok_or_else(|| err("StoredLinkNotFound", format!("Stored link {link_id} not found")))?;
        Ok(PyRep::Tuple(vec![PyRep::String(rec.name), rec.data]))
    })
}

/// `station.GetGuests()`: `(characterID, corporationID, allianceID, warFactionID)` for every
/// character docked in the caller's station.
fn handle_station_get_guests(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let station_id = session.station_id.ok_or_else(|| err("NotDockedInStation", "station.GetGuests requires being docked in a station"))?;
    ctx.db.with_snapshot(|snap| {
        let mut guests: Vec<_> = snap.characters.values().filter(|c| c.station_id == Some(station_id)).map(|c| (c.character_id, c.corporation_id, c.alliance_id, c.war_faction_id)).collect();
        guests.sort_by_key(|g| g.0);
        let opt = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
        Ok(PyRep::List(guests.into_iter().map(|(cid, corp, ally, fac)| PyRep::Tuple(vec![PyRep::Integer(cid), PyRep::Integer(corp), opt(ally), opt(fac)])).collect()))
    })
}

fn handle_achievements_get(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let mut done = ext.achievements_by_char.get(&char_id).cloned().unwrap_or_default();
        done.sort_unstable();
        let mut counters: Vec<(String, i64)> = ext.achievement_counters_by_char.get(&char_id).cloned().unwrap_or_default().into_iter().collect();
        counters.sort();
        Ok(PyRep::Tuple(vec![
            PyRep::List(done.into_iter().map(PyRep::Integer).collect()),
            PyRep::Dict(counters.into_iter().map(|(k, v)| (PyRep::String(k), PyRep::Integer(v))).collect()),
        ]))
    })
}

/// `UpdateClientAchievmentsAndCounters(achievementIDs, eventCounters)`: merges newly completed
/// achievement IDs and client event counters (counters only ever increase).
fn handle_achievements_update(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).filter(|&v| v > 0).collect(),
        _ => Vec::new(),
    };
    let counters: Vec<(String, i64)> = match args.get(1) {
        Some(PyRep::Dict(entries)) => entries
            .iter()
            .filter_map(|(k, v)| {
                let key = k.as_str().map(String::from).or_else(|| k.as_i64().map(|i| i.to_string()))?;
                Some((key, v.as_i64()?))
            })
            .collect(),
        _ => Vec::new(),
    };
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let done = ext.achievements_by_char.entry(char_id).or_default();
        for id in ids {
            if !done.contains(&id) {
                done.push(id);
            }
        }
        let ctr = ext.achievement_counters_by_char.entry(char_id).or_default();
        for (k, v) in counters {
            let slot = ctr.entry(k).or_insert(0);
            *slot = (*slot).max(v);
        }
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::None)
    })
}

/// `contextualOfferMgr.Track*(offerID)`: client telemetry; the server records the event.
fn handle_track_offer(ctx: &ServerContext, session: &mut SessionState, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let offer_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let log = ext.contextual_offer_events.entry(char_id).or_default();
        if log.len() >= MAX_TRACKED_OFFER_EVENTS {
            log.remove(0);
        }
        log.push((method.to_string(), offer_id, now_unix));
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::None)
    })
}

fn handle_nes_intro_get(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    Ok(ctx.db.with_snapshot(|snap| PyRep::Bool(load_cosmetics_ui_state(snap).nes_intro_skipped.contains_key(&char_id))))
}

fn handle_nes_intro_skip(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.nes_intro_skipped.entry(char_id).or_insert(now_unix);
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::None)
    })
}

fn handle_crate_open(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let has_crate = snap.items.get(&crate_id).is_some_and(|i| i.owner_id == char_id);
        if !has_crate {
            return Err(err("CrateNotFound", format!("Crate {crate_id} not found in inventory")));
        }
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(crate_id, now_unix);
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("opened", PyRep::Bool(true)),
            ("lootItems", PyRep::List(vec![
                PyRep::keyval([
                    ("typeID", PyRep::Integer(34)),
                    ("quantity", PyRep::Integer(1000)),
                ]),
                PyRep::keyval([
                    ("typeID", PyRep::Integer(36)),
                    ("quantity", PyRep::Integer(200)),
                ]),
            ])),
            ("skillBundleID", PyRep::Integer(1)),
        ]))
    })
}

fn handle_crate_claim_loot(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let crate_owner = snap.items.get(&crate_id).map(|i| i.owner_id);
        if crate_owner != Some(char_id) {
            return Err(err("CrateNotFound", format!("Crate {crate_id} not owned by character")));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        if claimed.contains(&crate_id) {
            return Err(err("LootAlreadyClaimed", format!("Loot from crate {crate_id} has already been claimed")));
        }
        claimed.push(crate_id);
        ext.opened_crates.remove(&crate_id);
        snap.items.remove(&crate_id);

        let dest_id = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .unwrap_or(60_003_760);
        let item_type = 34u32;
        let qty = 1_000i32;

        snap.next_entity_id += 1;
        let new_item_id = snap.next_entity_id;
        snap.items.insert(new_item_id, eve_db::InventoryItemRecord {
            item_id: new_item_id,
            type_id: item_type,
            owner_id: char_id,
            location_id: dest_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: qty,
            singleton: false,
            ..Default::default()
        });
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("deliveredItemID", PyRep::Integer(new_item_id)),
            ("typeID", PyRep::Integer(item_type as i64)),
            ("quantity", PyRep::Integer(qty as i64)),
        ]))
    })
}

fn handle_crate_claim_skill_bundle(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let bundle_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 || bundle_id != 1 {
        return Err(err("InvalidSkillBundle", "Valid opened crate and skill bundle ID 1 required"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let crate_owner = snap.items.get(&crate_id).map(|i| i.owner_id);
        if crate_owner != Some(char_id) {
            return Err(err("CrateNotFound", format!("Crate {crate_id} not owned by character")));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        let key = crate_id * 1000 + bundle_id;
        if claimed.contains(&key) || claimed.contains(&crate_id) {
            return Err(err("SkillBundleAlreadyClaimed", format!("Skill bundle {bundle_id} from crate {crate_id} already claimed")));
        }
        claimed.push(key);
        claimed.push(crate_id);
        ext.opened_crates.remove(&crate_id);
        snap.items.remove(&crate_id);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 25_000.0;
        let new_sp = ch.free_skill_points;
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("skillBundleID", PyRep::Integer(bundle_id)),
            ("spGranted", PyRep::Real(25_000.0)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
        ]))
    })
}

fn handle_milestone_process_logon(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let claimed = ext.claimed_milestones.get(&char_id).cloned().unwrap_or_default();
        Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(char_id)),
            ("activeMilestoneIDs", PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)])),
            ("claimedMilestoneIDs", PyRep::List(claimed.into_iter().map(PyRep::Integer).collect())),
        ]))
    })
}

fn handle_milestone_claim_rewards(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let milestone_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if milestone_id != 1 && milestone_id != 2 {
        return Err(err("InvalidMilestone", format!("Milestone {milestone_id} is not an active completed milestone")));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let claimed = ext.claimed_milestones.entry(char_id).or_default();
        if claimed.contains(&milestone_id) {
            return Err(err("MilestoneAlreadyClaimed", format!("Milestone {milestone_id} rewards have already been claimed")));
        }
        claimed.push(milestone_id);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 10_000.0;
        ch.balance = ch.balance.checked_add(eve_db::IskCents::from_isk(1_000_000)).unwrap_or(ch.balance);
        let new_sp = ch.free_skill_points;
        let new_bal = ch.balance.to_wire_f64();
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("milestoneID", PyRep::Integer(milestone_id)),
            ("spRewarded", PyRep::Real(10_000.0)),
            ("iskRewarded", PyRep::Real(1_000_000.0)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
            ("balance", PyRep::Real(new_bal)),
        ]))
    })
}

fn handle_login_campaign_get_state(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.login_campaign_claimed_days.get(&char_id).cloned().unwrap_or_default();
        let already_claimed_today = claimed_days.contains(&today);
        Ok(PyRep::keyval([
            ("campaignID", PyRep::Integer(101)),
            ("currentDay", PyRep::Integer(today as i64)),
            ("canClaimToday", PyRep::Bool(!already_claimed_today)),
            ("totalDaysClaimed", PyRep::Integer(claimed_days.len() as i64)),
        ]))
    })
}

fn handle_login_campaign_claim_reward(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.login_campaign_claimed_days.entry(char_id).or_default();
        if claimed_days.contains(&today) {
            return Err(err("RewardAlreadyClaimedToday", "Daily login reward has already been claimed for today"));
        }
        claimed_days.push(today);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 5_000.0;
        let new_sp = ch.free_skill_points;
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("campaignID", PyRep::Integer(101)),
            ("day", PyRep::Integer(today as i64)),
            ("spGranted", PyRep::Real(5_000.0)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
        ]))
    })
}

fn handle_seasonal_login_get_active_campaign(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.seasonal_claimed_days.get(&char_id).cloned().unwrap_or_default();
        Ok(PyRep::keyval([
            ("campaignID", PyRep::Integer(202)),
            ("endDateUnix", PyRep::Integer((now + 14 * 86_400) as i64)),
            ("daysRemaining", PyRep::Integer(14)),
            ("totalClaimed", PyRep::Integer(claimed_days.len() as i64)),
        ]))
    })
}

fn handle_seasonal_login_claim_reward(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.seasonal_claimed_days.entry(char_id).or_default();
        if claimed_days.contains(&today) {
            return Err(err("SeasonalRewardAlreadyClaimedToday", "Seasonal reward already claimed for today"));
        }
        claimed_days.push(today);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 15_000.0;
        let new_sp = ch.free_skill_points;
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("campaignID", PyRep::Integer(202)),
            ("day", PyRep::Integer(today as i64)),
            ("spGranted", PyRep::Real(15_000.0)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
        ]))
    })
}

fn handle_seasonal_login_get_claim_history(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.seasonal_claimed_days.get(&char_id).cloned().unwrap_or_default();
        let history: Vec<PyRep> = claimed_days.into_iter().map(|d| PyRep::Integer(d as i64)).collect();
        Ok(PyRep::List(history))
    })
}

