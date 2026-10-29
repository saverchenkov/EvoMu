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
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub setting_type: String,
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
    #[serde(default)]
    pub characters_warped: Vec<i64>,
    #[serde(default)]
    pub login_campaign_claimed_days: HashMap<i64, Vec<u64>>,
    #[serde(default)]
    pub seasonal_claimed_days: HashMap<i64, Vec<u64>>,
    #[serde(default)]
    pub active_login_campaign_id: Option<i64>,
    #[serde(default)]
    pub active_seasonal_campaign_id: Option<i64>,
    #[serde(default)]
    pub milestone_logons: HashMap<i64, u64>,
    #[serde(default)]
    pub community_track_qa_mode: bool,
    #[serde(default)]
    pub client_telemetry_events: Vec<String>,
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
        let opt_ship_tid = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0).and_then(|id| u32::try_from(id).ok());
        if skin_type.category_id == 2118 {
            return Some(crate::services::quasar_cosmetics_live::activate_ship_emblem_license(
                ctx, session, char_id, requested_skin_id, opt_ship_tid,
            ));
        }
        if skin_type.category_id != 91 {
            return Some(Err(err(
                "InvalidSkinLicenseType",
                format!("Type {requested_skin_id} (category {}) is not a Permanent SKIN or Ship Cosmetic license (category 91/2118)", skin_type.category_id),
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
        let Some(raw_ship_arg) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(err("InvalidShipTypeForSkin", "ApplySkinToShip requires shipID or shipTypeID")));
        };
        let (explicit_ship_id, ship_type_id) = if let Some(tid) = u32::try_from(raw_ship_arg).ok().filter(|t| ctx.sde.types.get(t).is_some_and(|ty| ty.category_id == 6)) {
            (None, tid)
        } else if let Some((owner, tid)) = ctx.db.with_snapshot(|s| s.items.get(&raw_ship_arg).map(|i| (i.owner_id, i.type_id))) {
            if !ctx.sde.types.get(&tid).is_some_and(|ty| ty.category_id == 6) {
                return Some(Err(err("InvalidShipTypeForSkin", format!("Item {raw_ship_arg} (type {tid}) is not a valid ship"))));
            }
            if owner != char_id {
                return Some(Err(err("OwnershipViolation", format!("Ship {raw_ship_arg} is not owned by character {char_id}"))));
            }
            (Some(raw_ship_arg), tid)
        } else {
            return Some(Err(err("InvalidShipTypeForSkin", format!("Invalid ship or ship typeID {raw_ship_arg}"))));
        };
        let now_unix = ctx.now_unix();
        if matches!(args.get(1), Some(PyRep::None) | Some(PyRep::Integer(0))) {
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let expired = purge_expired_skins_for_char(snap, char_id, now_unix);
                let target_ship = explicit_ship_id.unwrap_or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0));
                let ship_item = snap.items.get(&target_ship).ok_or_else(|| err("ShipNotFound", format!("Ship {target_ship} not found")))?;
                if ship_item.owner_id != char_id {
                    return Err(err("OwnershipViolation", format!("Ship {target_ship} is not owned by character {char_id}")));
                }
                if ship_item.type_id != ship_type_id {
                    return Err(err("ShipTypeMismatch", format!("Ship {target_ship} has type {}, expected {ship_type_id}", ship_item.type_id)));
                }
                let prev = snap.applied_ship_skins.remove(&target_ship);
                Ok((target_ship, prev, expired))
            });
            return match res {
                Ok((target_ship, prev_skin, expired)) => {
                    notify_skins_removed(session, &expired);
                    session.push_notification("OnCurrentShipSkinChange", vec![PyRep::Integer(target_ship), PyRep::None]);
                    crate::services::quasar_cosmetics_live::broadcast_ship_skin_cosmetic_change(ctx, char_id, target_ship, ship_type_id, prev_skin, None).await;
                    crate::services::quasar_cosmetics_live::publish_ship_skin_set(&ctx.quasar_gateway, &ctx.db, &ctx.sde, target_ship, now_unix);
                    Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(target_ship)),
                        ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                        ("skinID", PyRep::None),
                        ("designName", PyRep::None),
                        ("sequenced", PyRep::Bool(false)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            };
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
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let expired = purge_expired_skins_for_char(snap, char_id, now_unix);
            let active_ship = explicit_ship_id.unwrap_or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0));
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
            let prev = snap.applied_ship_skins.insert(active_ship, target_skin_id);
            Ok((active_ship, target_skin_id, prev, expired))
        });
        return match res {
            Ok((active_ship, skin_id, prev_skin, expired)) => {
                notify_skins_removed(session, &expired);
                session.push_notification("OnCurrentShipSkinChange", vec![PyRep::Integer(active_ship), PyRep::Integer(skin_id as i64)]);
                crate::services::quasar_cosmetics_live::broadcast_ship_skin_cosmetic_change(ctx, char_id, active_ship, ship_type_id, prev_skin, Some(skin_id)).await;
                // Quasar ship SKIN state (`eve_public.cosmetic.ship.api.SetNotice`) for observers.
                crate::services::quasar_cosmetics_live::publish_ship_skin_set(&ctx.quasar_gateway, &ctx.db, &ctx.sde, active_ship, now_unix);
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
            let licensee_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.licensed_skins.get_mut(&licensee_id) {
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
            let licensee_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.licensed_skins.get_mut(&licensee_id) {
                    list.retain(|&id| id != skin_id);
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }
        // parity: feat:cosmetics.first-party-skins
        ("shipCosmeticsMgr", "GetFirstPartySkinData") => Some(handle_get_first_party_skin_data(ctx, session, args)),
        // parity: feat:cosmetics.skin-licenses
        ("shipCosmeticsMgr", "GetLicencedSkins") => Some(handle_get_licensed_skins(ctx, session, None)),
        ("shipCosmeticsMgr", "GetLicencedSkinsForShipType") => {
            let ship_type = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok());
            match ship_type {
                Some(t) if ctx.sde.types.get(&t).map(|ty| ty.category_id == 6).unwrap_or(false) => Some(handle_get_licensed_skins(ctx, session, Some(t))),
                _ => Some(Err(err("InvalidShipTypeForSkin", "GetLicencedSkinsForShipType requires a ship typeID present in the SDE"))),
            }
        }
        ("shipCosmeticsMgr", "GetEnabledCosmetics") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let dict = ctx.db.with_snapshot(|snap| {
                crate::services::quasar_cosmetics_live::enabled_cosmetics_dict_from_snapshot(
                    snap,
                    ship_id.max(0) as u64,
                )
            });
            Some(Ok(dict))
        }
        // parity: feat:ui.overview-presets
        ("overviewPresetMgr", "StoreLinkAndGetID") => Some(handle_store_link(ctx, session, args, LinkKind::OverviewPreset)),
        ("overviewPresetMgr", "GetStoredPreset") => Some(handle_get_link(ctx, session, args, LinkKind::OverviewPreset)),
        // parity: feat:ui.shared-settings
        ("sharedSettingsMgr", "StoreSettingLinkAndGetID") => Some(handle_store_link(ctx, session, args, LinkKind::SharedSetting)),
        ("sharedSettingsMgr", "GetStoredSharedSetting") => Some(handle_get_link(ctx, session, args, LinkKind::SharedSetting)),
        // parity: feat:ui.station-guests
        ("station", "GetGuests") => Some(handle_station_get_guests(ctx, session)),
        // parity: feat:ui.achievements, rpc:monolithTaskTrackerMgr.HasEverWarped
        ("monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount") => Some(handle_achievements_get(ctx, session)),
        ("monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters") => Some(handle_achievements_update(ctx, session, args)),
        ("monolithTaskTrackerMgr", "HasEverWarped") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let warped = ctx.db.with_snapshot(|snap| {
                load_cosmetics_ui_state(snap).characters_warped.contains(&char_id)
            });
            Some(Ok(PyRep::Bool(warped)))
        }
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

        ("crateService", "OpenCrate") => Some(handle_crate_open(ctx, session, args)),
        ("crateService", "ClaimLoot") => Some(handle_crate_claim_loot(ctx, session, args)),
        ("crateService", "ClaimSkillBundle") => Some(handle_crate_claim_skill_bundle(ctx, session, args)),
        ("crateService", "ClaimFitting") => Some(handle_crate_claim_fitting(ctx, session, args)),
        ("crateService", "ClaimLootFromCrateStack") => Some(handle_crate_claim_loot_stack(ctx, session, args)),
        ("crateService", "GetCrateLocation") => Some(handle_crate_get_location(ctx, session, args)),
        ("crateService", "PeekInsideFixedCrate") => Some(handle_crate_peek(ctx, session, args)),
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

fn compute_payload_hash(payload_str: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h1 = DefaultHasher::new();
    payload_str.hash(&mut h1);
    let v1 = h1.finish();
    let mut h2 = DefaultHasher::new();
    v1.hash(&mut h2);
    payload_str.len().hash(&mut h2);
    let v2 = h2.finish();
    format!("{:016x}{:016x}", v1, v2)
}

/// `StoreLinkAndGetID(yaml_str)` / `StoreSettingLinkAndGetID(setting_type, yaml_str)`: persists
/// the payload under a fresh server-wide link ID and hash, returning the appropriate KeyVal.
fn handle_store_link(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kind: LinkKind,
) -> Result<PyRep, BridgeRpcError> {
    const MAX_PAYLOAD_BYTES: usize = 65_536; // 64 KiB cap per payload
    const MAX_STORED_LINKS: usize = 10_000;   // Bounded storage entries
    let (_, char_id) = ServerContext::require_character(session)?;
    let (name, data_rep, setting_type) = if args.len() >= 2 {
        let first_str = args.first().and_then(|a| a.as_str()).map(str::trim).unwrap_or("").to_string();
        if first_str.is_empty() {
            return Err(err("InvalidLinkName", "A shared link requires a non-empty name"));
        }
        let data = args.get(1).filter(|d| !matches!(d, PyRep::None)).cloned().ok_or_else(|| err("EmptyLinkPayload", "A shared link requires a payload"))?;
        (first_str.clone(), data, first_str)
    } else if let Some(single_data) = args.first().filter(|d| !matches!(d, PyRep::None)) {
        ("default".to_string(), single_data.clone(), "default".to_string())
    } else {
        return Err(err("EmptyLinkPayload", "A shared link requires a payload"));
    };

    let payload_str = match &data_rep {
        PyRep::String(s) => s.clone(),
        PyRep::None => return Err(err("EmptyLinkPayload", "A shared link requires a payload")),
        other => format!("{other:?}"),
    };
    if payload_str.len() > MAX_PAYLOAD_BYTES {
        return Err(err("PayloadTooLarge", "Shared setting link payload exceeds 64 KiB limit"));
    }
    let hash = compute_payload_hash(&payload_str);

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let map = link_map(&mut ext, kind);
        if map.len() >= MAX_STORED_LINKS {
            if let Some(&oldest_id) = map.keys().min() {
                map.remove(&oldest_id);
            }
        }
        ext.next_link_id += 1;
        let link_id = ext.next_link_id;
        link_map(&mut ext, kind).insert(
            link_id,
            StoredLinkRecord {
                owner_char_id: char_id,
                name,
                data: PyRep::String(payload_str),
                hash: hash.clone(),
                setting_type: setting_type.clone(),
            },
        );
        save_cosmetics_ui_state(snap, &ext);
        match kind {
            LinkKind::OverviewPreset => Ok(PyRep::keyval([
                ("sqID", PyRep::Integer(link_id)),
                ("hashvalue", PyRep::String(hash)),
            ])),
            LinkKind::SharedSetting => Ok(PyRep::keyval([
                ("settingType", PyRep::String(setting_type)),
                ("key", PyRep::String(hash)),
                ("linkID", PyRep::Integer(link_id)),
            ])),
        }
    })
}

/// `GetStoredPreset(linkID_or_hash)` / `GetStoredSharedSetting(linkID_or_key)`: any character
/// holding the link or hash receives the stored YAML string.
fn handle_get_link(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kind: LinkKind,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let first = args.first().ok_or_else(|| err("MissingLinkId", "Stored link lookup requires a link ID or hash"))?;
    ctx.db.with_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let map = link_map(&mut ext, kind);
        let rec = match first {
            PyRep::Integer(id) => map.get(id).cloned(),
            PyRep::String(s) => map
                .values()
                .find(|r| r.hash == *s || r.name == *s)
                .cloned(),
            _ => None,
        };
        let rec = rec.ok_or_else(|| err("StoredLinkNotFound", "Stored link not found"))?;
        match rec.data {
            PyRep::String(s) => Ok(PyRep::String(s)),
            other => Ok(PyRep::String(format!("{other:?}"))),
        }
    })
}

/// `station.GetGuests()`: `(characterID, corporationID, allianceID, warFactionID)` for every
/// online character docked in the caller's station.
fn handle_station_get_guests(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_id) = ServerContext::require_character(session)?;
    let station_id = session.station_id.ok_or_else(|| err("NotDockedInStation", "station.GetGuests requires being docked in a station"))?;
    ctx.db.with_snapshot(|snap| {
        let mut guests: Vec<_> = snap.characters.values()
            .filter(|c| c.station_id == Some(station_id) && (c.character_id == caller_id || ctx.is_character_online(c.character_id, c.user_id)))
            .map(|c| (c.character_id, c.corporation_id, c.alliance_id, c.war_faction_id))
            .collect();
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
        // `monolithTaskTrackerSvc.FetchMyAchievementStatus`
        // (`packages/monolith_task_tracker/client/monolithTaskTrackerSvc.pyj:70-74`)
        // indexes `achievementAndEventInfo['completedDict']` and `achievementAndEventInfo['eventDict']`.
        Ok(PyRep::Dict(vec![
            (
                PyRep::String("completedDict".into()),
                PyRep::Dict(
                    done.into_iter()
                        .map(|id| (PyRep::Integer(id), PyRep::Bool(true)))
                        .collect(),
                ),
            ),
            (
                PyRep::String("eventDict".into()),
                PyRep::Dict(
                    counters
                        .into_iter()
                        .map(|(k, v)| (PyRep::String(k), PyRep::Integer(v)))
                        .collect(),
                ),
            ),
        ]))
    })
}

/// `UpdateClientAchievmentsAndCounters(achievementIDs, eventCounters)`: merges newly completed
/// achievement IDs and client event counters (counters only ever increase).
fn handle_achievements_update(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).filter(|&v| v > 0).collect(),
        Some(PyRep::Dict(entries)) => entries.iter().filter_map(|(k, _)| k.as_i64()).filter(|&v| v > 0).collect(),
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

#[derive(Debug, Clone)]
struct CrateLootSpec {
    pub items: Vec<(u32, i32)>,
    pub skill_bundle: Option<(i64, f64)>,
    pub fitting_ship_type_id: Option<u32>,
    pub fittings: Vec<u32>,
}

fn get_crate_specs(ctx: &ServerContext) -> &'static HashMap<u32, CrateLootSpec> {
    static CRATE_SPECS: std::sync::OnceLock<HashMap<u32, CrateLootSpec>> = std::sync::OnceLock::new();
    CRATE_SPECS.get_or_init(|| {
        let mut map = HashMap::new();
        if let Ok(cfsd) = ctx.sde.load_cfsd_nested("res:/staticdata/crates.fsdbinary") {
            for (_k, v) in cfsd {
                let Some(type_id) = v.get("typeID").and_then(|t| t.as_u64()).map(|t| t as u32) else {
                    continue;
                };
                let mut items = Vec::new();
                if let Some(list) = v.get("fixedLootItems").and_then(|l| l.as_array()) {
                    for item_val in list {
                        if let Some(tid) = item_val.as_u64() {
                            items.push((tid as u32, 1));
                        }
                    }
                }
                let skill_bundle = v
                    .get("skillBundleID")
                    .and_then(|b| b.as_i64())
                    .map(|bid| (bid, 25_000.0));

                let mut fitting_ship_type_id = None;
                let mut fittings = Vec::new();
                if let Some(fits) = v.get("fittings").and_then(|f| f.as_array()) {
                    for fit_val in fits {
                        if let Some(fid) = fit_val.as_u64() {
                            fittings.push(fid as u32);
                        }
                    }
                    if !fits.is_empty() {
                        if let Some(crate_type) = ctx.sde.types.get(&type_id) {
                            let name_lower = crate_type.name.to_lowercase();
                            let mut matches: Vec<_> = ctx
                                .sde
                                .types
                                .values()
                                .filter(|t| t.category_id == 6 && t.published && name_lower.contains(&t.name.to_lowercase()))
                                .collect();
                            matches.sort_by_key(|t| std::cmp::Reverse(t.name.len()));
                            if let Some(ship) = matches.first() {
                                fitting_ship_type_id = Some(ship.type_id);
                            }
                        }
                    }
                }
                map.insert(
                    type_id,
                    CrateLootSpec {
                        items,
                        skill_bundle,
                        fitting_ship_type_id,
                        fittings,
                    },
                );
            }
        }
        map
    })
}

// Crate loot tables and ship fitting specs are decoded directly from `res:/staticdata/crates.fsdbinary`
// via SdeRegistry::load_cfsd_nested, extracting authentic fixedLootItems, skillBundleID, and fittings.
fn crate_loot_spec(ctx: &ServerContext, type_id: u32) -> Option<CrateLootSpec> {
    let specs = get_crate_specs(ctx);
    if let Some(spec) = specs.get(&type_id) {
        if !spec.items.is_empty() || spec.skill_bundle.is_some() || spec.fitting_ship_type_id.is_some() || !spec.fittings.is_empty() {
            return Some(spec.clone());
        }
    }

    let t = ctx.sde.types.get(&type_id)?;
    let is_crate = (t.category_id == 17 && t.group_id == 1818)
        || (t.category_id == 63
            && (t.group_id == 1194 || t.group_id == 4093 || t.group_id == 4139))
        || (t.name.contains("Crate")
            || t.name.contains("Box")
            || t.name.contains("Cache")
            || t.name.contains("Bundle")
            || t.name.contains("Package"));
    if !is_crate {
        return None;
    }

    // Direct type_materials if present in SDE
    if let Some(mats) = ctx.sde.type_materials.get(&type_id).filter(|m| !m.is_empty()) {
        let items: Vec<(u32, i32)> = mats
            .iter()
            .map(|m| (m.material_type_id, (m.quantity.max(1) as i32)))
            .collect();
        return Some(CrateLootSpec {
            items,
            skill_bundle: None,
            fitting_ship_type_id: None,
            fittings: Vec::new(),
        });
    }

    None
}

fn require_owned_crate(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    crate_id: i64,
) -> Result<(eve_db::InventoryItemRecord, CrateLootSpec), BridgeRpcError> {
    let Some(item) = snap.items.get(&crate_id).cloned() else {
        return Err(err("CrateNotFound", format!("Crate {crate_id} not found in inventory")));
    };
    if item.owner_id != char_id {
        return Err(err("CrateNotFound", format!("Crate {crate_id} not owned by character")));
    }
    let current_dock = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.station_id.or(c.structure_id))
        .or_else(|| session.station_id.or(session.structure_id));
    let current_ship = session.ship_id.or_else(|| {
        snap.characters
            .get(&char_id)
            .map(|c| c.active_ship_id)
            .filter(|&sid| sid > 0)
    });
    if !snap.is_item_consumable_by(&item, char_id, current_dock, current_ship) {
        return Err(err(
            "InvalidCrateLocation",
            format!("Crate {crate_id} is not reachable in your current hangar/cargo or is locked in a contract"),
        ));
    }
    let Some(spec) = crate_loot_spec(ctx, item.type_id) else {
        return Err(err(
            "InvalidCrateType",
            format!("Item {crate_id} (type {}) is not an openable crate", item.type_id),
        ));
    };
    Ok((item, spec))
}

fn handle_crate_open(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let (_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(crate_id, now_unix);
        save_cosmetics_ui_state(snap, &ext);
        let loot_items = spec
            .items
            .iter()
            .map(|&(tid, q)| {
                PyRep::keyval([
                    ("typeID", PyRep::Integer(i64::from(tid))),
                    ("quantity", PyRep::Integer(i64::from(q))),
                ])
            })
            .collect();
        let bundle_rep = spec
            .skill_bundle
            .map(|(bid, _)| PyRep::Integer(bid))
            .unwrap_or(PyRep::None);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("opened", PyRep::Bool(true)),
            ("lootItems", PyRep::List(loot_items)),
            ("skillBundleID", bundle_rep),
        ]))
    })
}

fn handle_crate_claim_loot(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let char_dock = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(session_dock);
        let allowed_dest_id = char_dock.ok_or_else(|| {
            err(
                "DockingRequired",
                "Must be docked in a station or structure to claim crate loot",
            )
        })?;
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;

        if let Some(dest_override) = args.get(1).and_then(|a| a.as_i64()) {
            if dest_override != allowed_dest_id {
                return Err(err(
                    "InvalidCrateLocation",
                    "Crate loot can only be delivered to the character's current station hangar",
                ));
            }
        }
        if let Some(flag_override) = args.get(2).and_then(|a| a.as_i64()) {
            if flag_override != i64::from(eve_db::InventoryFlag::Hangar as u16) {
                return Err(err(
                    "InvalidCrateLocation",
                    "Crate loot can only be delivered into the station hangar",
                ));
            }
        }

        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        if claimed.contains(&crate_id) {
            return Err(err("LootAlreadyClaimed", format!("Loot from crate {crate_id} has already been claimed")));
        }
        ext.opened_crates.remove(&crate_id);
        if crate_item.quantity > 1 {
            if let Some(it_mut) = snap.items.get_mut(&crate_id) {
                it_mut.quantity -= 1;
                it_mut.version += 1;
            }
        } else {
            claimed.push(crate_id);
            snap.items.remove(&crate_id);
        }

        let mut first_delivered = (0_i64, 0_u32, 0_i32);
        for &(item_type, qty) in &spec.items {
            snap.next_entity_id += 1;
            let new_item_id = snap.next_entity_id;
            snap.items.insert(
                new_item_id,
                eve_db::InventoryItemRecord {
                    item_id: new_item_id,
                    type_id: item_type,
                    owner_id: char_id,
                    location_id: allowed_dest_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
            if first_delivered.0 == 0 {
                first_delivered = (new_item_id, item_type, qty);
            }
        }
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("deliveredItemID", PyRep::Integer(first_delivered.0)),
            ("typeID", PyRep::Integer(i64::from(first_delivered.1))),
            ("quantity", PyRep::Integer(i64::from(first_delivered.2))),
        ]))
    })
}

fn handle_get_first_party_skin_data(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (licensee_id, skin_id) = match args {
        [s] => (
            char_id,
            s.as_i64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        ),
        [l, s, ..] => (
            l.as_i64().unwrap_or(char_id),
            s.as_i64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        ),
        _ => (char_id, 0),
    };
    if licensee_id != char_id && !session.has_gm_role() {
        return Err(err(
            "CharacterOwnershipMismatch",
            format!("Cannot query skin data for character {licensee_id} from session {char_id}"),
        ));
    }
    if skin_id == 0 {
        return Ok(PyRep::None);
    }
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let _ = purge_expired_skins_for_char(snap, licensee_id, now_unix);
        let ext = load_cosmetics_ui_state(snap);
        let entries = ext.skin_license_entries.get(&licensee_id).cloned().unwrap_or_default();
        let licensed_raw = snap.licensed_skins.get(&licensee_id).cloned().unwrap_or_default();
        if !licensed_raw.contains(&skin_id) {
            return Ok(PyRep::None);
        }
        let exp = entries.iter().find(|e| e.skin_id == skin_id).and_then(|e| e.expires_at_unix);
        Ok(PyRep::keyval([
            ("skinID", PyRep::Integer(skin_id as i64)),
            ("licenseeID", PyRep::Integer(licensee_id)),
            ("expires", exp.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
            ("isSingleUse", PyRep::Bool(false)),
        ]))
    })
}

fn handle_crate_get_location(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    ctx.db.with_snapshot(|snap| {
        let (crate_item, _spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let dest_id = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .unwrap_or(crate_item.location_id);
        let flag_hangar = 4i64; // InventoryFlag::Hangar
        Ok(PyRep::Tuple(vec![PyRep::Integer(dest_id), PyRep::Integer(flag_hangar)]))
    })
}

fn handle_crate_peek(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    ctx.db.with_snapshot(|snap| {
        let (_crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let entries = spec
            .items
            .into_iter()
            .map(|(tid, qty)| {
                PyRep::Tuple(vec![
                    PyRep::Integer(i64::from(tid)),
                    PyRep::Integer(i64::from(qty)),
                    PyRep::None,
                ])
            })
            .collect();
        Ok(PyRep::List(entries))
    })
}

fn handle_crate_claim_fitting(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let fitting_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 || fitting_id <= 0 {
        return Err(err("InvalidFittingClaim", "Valid opened crate ID and fitting ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    ctx.db.try_with_mut_snapshot(|snap| {
        let dest_id = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(session_dock)
            .ok_or_else(|| {
                err(
                    "DockingRequired",
                    "Must be docked in a station or structure to claim crate fitting",
                )
            })?;
        let mut ext = load_cosmetics_ui_state(snap);
        let key = crate_id.saturating_mul(10_000).saturating_add(fitting_id);
        if ext.claimed_crate_loot.get(&char_id).map(|v| v.contains(&key)).unwrap_or(false) {
            return Err(err("FittingAlreadyClaimed", format!("Fitting {fitting_id} from crate {crate_id} already claimed")));
        }
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let ship_type_id = spec
            .fitting_ship_type_id
            .ok_or_else(|| err("InvalidCrateFitting", "This crate does not contain a ship fitting"))?;
        if !spec.fittings.contains(&(fitting_id as u32)) {
            return Err(err(
                "InvalidCrateFitting",
                format!("Fitting {fitting_id} is not valid for crate type {}", crate_item.type_id),
            ));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        ext.opened_crates.remove(&crate_id);
        if crate_item.quantity > 1 {
            if let Some(it_mut) = snap.items.get_mut(&crate_id) {
                it_mut.quantity -= 1;
                it_mut.version += 1;
            }
        } else {
            claimed.push(key);
            claimed.push(crate_id);
            snap.items.remove(&crate_id);
        }
        // SYNTHETIC: Deliver packaged ship hull cleanly to station hangar without hardcoded civilian modules
        // because SDE does not specify fitting modules for sealed crate hulls.
        snap.next_entity_id += 1;
        let ship_item_id = snap.next_entity_id;
        snap.items.insert(
            ship_item_id,
            eve_db::InventoryItemRecord {
                item_id: ship_item_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: dest_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false, // Packaged ship hull
                ..Default::default()
            },
        );
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("fittingID", PyRep::Integer(fitting_id)),
            ("deliveredItemID", PyRep::Integer(ship_item_id)),
            ("claimed", PyRep::Bool(true)),
        ]))
    })
}

fn handle_crate_claim_loot_stack(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let qty = args
        .get(1)
        .and_then(|a| a.as_i64())
        .unwrap_or(1)
        .clamp(1, i64::from(i32::MAX)) as i32;
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    ctx.db.try_with_mut_snapshot(|snap| {
        let dest_id = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(session_dock)
            .ok_or_else(|| {
                err(
                    "DockingRequired",
                    "Must be docked in a station or structure to claim crate loot",
                )
            })?;
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        if spec.items.is_empty() {
            return Err(err(
                "InvalidCrateType",
                format!("Crate {crate_id} (type {}) does not contain fixed loot items", crate_item.type_id),
            ));
        }
        let actual_to_open = qty.min(crate_item.quantity);
        if actual_to_open <= 0 {
            return Err(err("NoCratesToOpen", "No crates available in stack"));
        }

        if crate_item.quantity <= actual_to_open {
            snap.items.remove(&crate_id);
        } else if let Some(item_mut) = snap.items.get_mut(&crate_id) {
            item_mut.quantity -= actual_to_open;
            item_mut.version += 1;
        }

        for (item_type, base_qty) in spec.items {
            snap.next_entity_id += 1;
            let new_item_id = snap.next_entity_id;
            snap.items.insert(
                new_item_id,
                eve_db::InventoryItemRecord {
                    item_id: new_item_id,
                    type_id: item_type,
                    owner_id: char_id,
                    location_id: dest_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: base_qty.saturating_mul(actual_to_open),
                    singleton: false,
                    ..Default::default()
                },
            );
        }

        Ok(PyRep::Tuple(vec![
            PyRep::Integer(actual_to_open as i64),
            PyRep::Integer(qty as i64),
        ]))
    })
}

fn handle_crate_claim_skill_bundle(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let bundle_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 || bundle_id <= 0 {
        return Err(err("InvalidSkillBundle", "Valid opened crate and skill bundle ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    ctx.db.try_with_mut_snapshot(|snap| {
        let _dock_loc = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(session_dock)
            .ok_or_else(|| {
                err(
                    "DockingRequired",
                    "Must be docked in a station or structure to claim crate skill bundle",
                )
            })?;
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let Some((expected_bundle_id, sp_grant)) = spec.skill_bundle else {
            return Err(err("InvalidSkillBundle", format!("Crate {crate_id} does not contain a skill bundle")));
        };
        if bundle_id != expected_bundle_id {
            return Err(err("InvalidSkillBundle", format!("Skill bundle {bundle_id} does not match crate bundle {expected_bundle_id}")));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        let key = crate_id.saturating_mul(1000).saturating_add(bundle_id);
        if claimed.contains(&key) || claimed.contains(&crate_id) {
            return Err(err("SkillBundleAlreadyClaimed", format!("Skill bundle {bundle_id} from crate {crate_id} already claimed")));
        }
        ext.opened_crates.remove(&crate_id);
        if crate_item.quantity > 1 {
            if let Some(it_mut) = snap.items.get_mut(&crate_id) {
                it_mut.quantity -= 1;
                it_mut.version += 1;
            }
        } else {
            claimed.push(key);
            claimed.push(crate_id);
            snap.items.remove(&crate_id);
        }
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += sp_grant;
        let new_sp = ch.free_skill_points;
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([
            ("crateID", PyRep::Integer(crate_id)),
            ("skillBundleID", PyRep::Integer(bundle_id)),
            ("spGranted", PyRep::Real(sp_grant)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
        ]))
    })
}

