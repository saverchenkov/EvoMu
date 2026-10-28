//! `structure_upwell_ops` — Upwell structure unanchor/rename, vulnerability
//! schedule, asset safety, structure control, descriptions, deliveries, cargo drop-off, guests
//! and hangar view. Only RPC names present in the V24 client are dispatched here; anything
//! else falls through to `UnimplementedRPC`.
#![forbid(unsafe_code)]
use crate::router::ServerContext;
use eve_db::{InventoryFlag, UpwellStructureRecord, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const STRUCT_EXT_STATE_KEY: i64 = -7_700_001;
/// Asset safety: wraps become deliverable 5 days after creation (ledger acceptance text).
pub const ASSET_SAFETY_DELIVERY_DELAY_SECS: u64 = 5 * 86_400;
/// Asset safety: wraps not delivered within 20 days are auto-delivered to nearest NPC station (`feat:lifecycle.asset_safety-auto_delivery`).
pub const ASSET_SAFETY_AUTO_DELIVERY_SECS: u64 = 20 * 86_400;
/// Asset safety: delivering/retrieving a wrap outside same-system manual window costs 15% of its estimated value.
const ASSET_SAFETY_FEE_FRACTION: f64 = 0.15;
/// Unanchoring timer (ledger acceptance text: 7-day timer).
const UNANCHOR_TIMER_SECS: u64 = 7 * 86_400;
pub const ANSIBLEX_TYPE_ID: u32 = 35841;
pub const PROFILE_CHANGE_DELAY_SECS: u64 = 5 * 60;
pub fn reinforce_change_delay_secs(ctx: &ServerContext, sys_id: u32) -> u64 {
    if super::structure_lifecycle::space_class(ctx, sys_id) == super::structure_lifecycle::SpaceClass::HighSec { 30 * 86_400 } else { 7 * 86_400 }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSafetyWrapRecord {
    pub wrap_id: i64,
    pub owner_id: i64,
    pub is_corp: bool,
    pub source_structure_id: i64,
    pub source_structure_name: String,
    pub solar_system_id: u32,
    pub created_unix: u64,
    pub deliverable_after_unix: u64,
    pub delivered_to_id: Option<i64>,
    pub item_ids: Vec<i64>,
    /// DERIVED: sum of SDE `basePrice * quantity`; the live server uses market estimates.
    pub estimated_value_isk: f64,
    #[serde(default)]
    pub nearest_npc_station_id: Option<i64>,
    #[serde(default)]
    pub auto_delivered: bool,
    #[serde(default)]
    pub retrieval_fee_paid: bool,
}

/// Per-structure lifecycle bookkeeping used by `structure_lifecycle`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureLifecycleState {
    pub repair_until_unix: Option<u64>, pub fight_started_unix: Option<u64>, pub reinforce_jitter_secs: i64,
    pub last_fuel_tick_unix: Option<u64>, pub low_power_since_unix: Option<u64>, pub online_modules: Vec<i64>,
    #[serde(default)] pub unjittered_reinforce_exit_unix: Option<u64>,
    #[serde(default)] pub shield_fight_started_unix: Option<u64>,
    #[serde(default)] pub armor_reinforce_secs_applied: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessGroupRecord {
    pub group_id: i64, pub creator_id: i64, pub owner_corp_id: i64, pub name: String, pub description: String, pub members: HashMap<i64, i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureAccessProfile {
    pub profile_id: i64, pub owner_corp_id: i64, pub name: String, pub description: String, pub access_group_ids: HashSet<i64>,
    pub blocked_characters: HashSet<i64>, pub allowed_characters: HashSet<i64>, pub blocked_corporations: HashSet<i64>,
    pub allowed_corporations: HashSet<i64>, pub blocked_alliances: HashSet<i64>, pub allowed_alliances: HashSet<i64>,
    pub corp_only: bool, pub service_settings: HashMap<i64, i64>, pub general_settings: HashMap<i64, bool>,
    #[serde(default)] pub defense_group_ids: HashSet<i64>,
    #[serde(default)] pub defense_allowed_characters: HashSet<i64>,
    #[serde(default)] pub defense_blocked_characters: HashSet<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureExtendedState {
    pub unanchoring_until_unix: HashMap<i64, u64>, pub descriptions: HashMap<i64, String>, pub active_pilot_by_structure: HashMap<i64, i64>,
    pub pending_reinforce_schedule: HashMap<i64, (u8, u8, u64)>, pub asset_safety_wraps: HashMap<i64, AssetSafetyWrapRecord>,
    pub lifecycle: HashMap<i64, StructureLifecycleState>, pub war_hq_structures: std::collections::HashSet<i64>,
    pub jump_bridge_links: HashMap<i64, i64>, pub access_profiles: HashMap<i64, StructureAccessProfile>,
    pub profiles_by_id: HashMap<i64, StructureAccessProfile>, pub structure_profile_id: HashMap<i64, i64>, pub owner_groups: HashMap<i64, AccessGroupRecord>,
    #[serde(default)] pub hacked_reinforce_reveals: HashMap<i64, HashSet<i64>>,
    #[serde(default)] pub pending_profile_applications: HashMap<i64, (i64, u64)>,
    #[serde(default)] pub previous_ship_by_structure: HashMap<i64, i64>,
}

pub fn load_ext_state(snap: &WorldStateSnapshot) -> StructureExtendedState {
    snap.item_custom_names.get(&STRUCT_EXT_STATE_KEY).and_then(|raw| serde_json::from_str::<StructureExtendedState>(raw).ok()).unwrap_or_default()
}

pub fn save_ext_state(snap: &mut WorldStateSnapshot, state: &StructureExtendedState) {
    if let Ok(json) = serde_json::to_string(state) { snap.item_custom_names.insert(STRUCT_EXT_STATE_KEY, json); }
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError { BridgeRpcError { code: code.into(), message: message.into() } }
fn arg_i64(args: &[PyRep], idx: usize) -> Option<i64> { args.get(idx).and_then(|a| a.as_i64()) }

/// Applies a pending vulnerability-schedule change once its change delay has elapsed.
pub fn apply_pending_reinforce_schedule(ctx: &ServerContext, snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, now_unix: u64) {
    let Some(st) = snap.structures.get(&structure_id).cloned() else { return; };
    if st.state.starts_with("Reinforced") || st.state == "ArmorReinforced" || st.state == "HullReinforced" { return; }
    let Some(profile) = super::structure_lifecycle::structure_profile(ctx, st.type_id) else { return; };
    let power = super::structure_lifecycle::power_state(ctx, snap, ext, &st, now_unix);
    let max_shield = super::structure_lifecycle::effective_max_shield(ctx, snap, &profile, structure_id, power);
    let at_full = st.shield_hp + 1e-3 >= max_shield && st.armor_hp + 1e-3 >= profile.armor_hp;
    let not_rep = !ext.lifecycle.get(&structure_id).and_then(|l| l.repair_until_unix).is_some_and(|t| now_unix < t);
    if st.state != "ShieldVulnerable" || now_unix < st.reinforced_until_unix || !at_full || !not_rep { return; }
    if let Some(&(hour, day, apply_at)) = ext.pending_reinforce_schedule.get(&structure_id) {
        if now_unix >= apply_at {
            snap.structure_vulnerability_windows.insert(structure_id, (u64::from(hour), u64::from(day)));
            ext.pending_reinforce_schedule.remove(&structure_id);
        }
    }
}

pub fn apply_pending_profile_applications(ext: &mut StructureExtendedState, now_unix: u64) {
    let due: Vec<(i64, i64)> = ext.pending_profile_applications.iter().filter(|(_, &(_, t))| now_unix >= t).map(|(&s, &(p, _))| (s, p)).collect();
    for (sid, pid) in due {
        ext.pending_profile_applications.remove(&sid);
        if let Some(prof) = ext.profiles_by_id.get(&pid).cloned() {
            ext.structure_profile_id.insert(sid, pid);
            ext.access_profiles.insert(sid, prof);
        }
    }
}

pub async fn handle(ctx: &ServerContext, session: &mut SessionState, service: &str, method: &str, args: &[PyRep], _kwargs: &HashMap<String, PyRep>) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("structureDeployment", "Unanchor") => Some(handle_structure_unanchor(ctx, session, args)),
        ("structureDeployment", "CancelUnanchor") => Some(handle_structure_cancel_unanchor(ctx, session, args)),
        ("structureDeployment", "RenameStructure") => Some(handle_structure_rename(ctx, session, args)),
        ("structureVulnerability", "UpdateReinforceTimesFromClient") => Some(handle_update_reinforce_times(ctx, session, args)),
        // parity: feat:structures.asset-safety-manual
        ("structureAssetSafety", "MovePersonalAssetsToSafety") => Some(handle_move_assets_to_safety(ctx, session, args, false)),
        ("structureAssetSafety", "MoveCorpAssetsToSafety") => Some(handle_move_assets_to_safety(ctx, session, args, true)),
        ("structureAssetSafety", "GetItemsInSafetyForCharacter") => Some(handle_get_items_in_safety(ctx, session, false)),
        ("structureAssetSafety", "GetItemsInSafetyForCorp") => Some(handle_get_items_in_safety(ctx, session, true)),
        ("structureAssetSafety", "GetWrapNames") => Some(handle_get_wrap_names(ctx, session, args)),
        ("structureAssetSafety", "GetStructuresICanDeliverTo") => Some(handle_get_structures_i_can_deliver_to(ctx, session, args)),
        ("structureAssetSafety", "MoveSafetyWrapToStructure") => Some(handle_move_safety_wrap_to_structure(ctx, session, args)),
        // parity: feat:admin.gm-assetsafety-time
        ("structureAssetSafety", "MoveEjectTimeGM") => {
            if !session.has_gm_role() { return Some(Err(err("InsufficientRole", "structureAssetSafety.MoveEjectTimeGM requires GM role"))); }
            let (wrap_id, new_eject_time) = (args.first().and_then(|a| a.as_i64()).unwrap_or(0), args.get(1).and_then(|a| a.as_i64()).unwrap_or(0));
            if wrap_id > 0 {
                let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    let mut ext = load_ext_state(snap);
                    if let Some(wrap) = ext.asset_safety_wraps.get_mut(&wrap_id) { wrap.deliverable_after_unix = new_eject_time.max(0) as u64; save_ext_state(snap, &ext); }
                    Ok(())
                });
            }
            Some(Ok(PyRep::Bool(true)))
        }
        ("structureControl", "TakeControl") => Some(handle_take_control(ctx, session, args)),
        ("structureControl", "ReleaseControl") => Some(crate::services::structure_acl_defense_rules::handle_release_control(ctx, session, args)),
        ("structureControl", "GetStructurePilot") => Some(crate::services::structure_acl_defense_rules::handle_get_structure_pilot(ctx, session, args)),
        ("structureControl", "BoardStructure") => Some(crate::services::structure_acl_defense_rules::handle_board_structure(ctx, session, args)),
        ("structureControl", "EjectFromStructure") => Some(crate::services::structure_acl_defense_rules::handle_eject_from_structure(ctx, session, args)),
        ("structureSettings", "CharacterHasService") => Some(handle_character_has_service(ctx, session, args)),
        ("structureSettings", "CharacterCheckService") => Some(handle_character_check_service(ctx, session, args)),
        ("structureSettings", "CharacterGetService") => Some(handle_character_get_service(ctx, session, args)),
        ("structureSettings", "CharacterHasSetting") => Some(handle_character_has_setting(ctx, session, args)),
        ("structureSettings", "CharacterGetServices") => Some(handle_character_get_services(ctx, session, args)),
        ("structureSettings", "clear_memoized") | ("structureDirectory", "clear_memoized") => {
            // const-ok: client-side memoized cache invalidation notifications return None
            Some(Ok(PyRep::None))
        }
        // V24 client structureSettings profile configuration RPCs wired to ACL profile persistence
        ("structureSettings", "CreateProfile") => Some(handle_create_profile(ctx, session, args)),
        ("structureSettings", "SaveProfileSettings") => Some(handle_save_profile_settings(ctx, session, args)),
        ("structureSettings", "UpdateProfile") => Some(handle_update_profile(ctx, session, args)),
        ("structureSettings", "ChangeProfiles") => Some(handle_change_profiles(ctx, session, args)),
        ("ownerGroupManager", "Create") => Some(handle_create_owner_group(ctx, session, args)),
        ("ownerGroupManager", "AddMembers") => Some(handle_add_group_members(ctx, session, args)),
        ("ownerGroupManager", "DeleteMembers") => Some(handle_delete_group_members(ctx, session, args)),
        // parity: feat:structures.structure-description
        ("structureDirectory", "SetStructureDescription") => Some(handle_set_structure_description(ctx, session, args)),
        ("structureDirectory", "GetStructureDescription") => Some(handle_get_structure_description(ctx, session, args)),
        ("structureDirectory", "GetStructures") => Some(super::structure_lifecycle_rpc::handle_get_structures(ctx, session, false)),
        ("structureDirectory", "GetCorporationStructures") => Some(super::structure_lifecycle_rpc::handle_get_structures(ctx, session, true)),
        ("structureDirectory", "GetStructureInfo") => Some(super::structure_lifecycle_rpc::handle_get_structure_info(ctx, session, args)),
        // parity: feat:structures.deliveries
        ("structureDeliveries", "Deliver") => Some(super::structure_lifecycle_rpc::handle_structure_deliver(ctx, session, args)),
        // parity: feat:structures.cargo-dropoff
        ("structureCargoDelivery", "DropOffItems") => Some(super::structure_lifecycle_rpc::handle_cargo_dropoff(ctx, session, args).await),
        // parity: feat:structures.guests
        ("structureGuests", "GetGuests") => Some(handle_structure_get_guests(ctx, session, args)),
        // parity: feat:structures.hangar-view
        ("structureHangarViewMgr", "GetMyHangarViewState") => Some(handle_get_hangar_view_state(ctx, session)),
        ("jumpCloneSvc", "GetNumClonesInPilotsStructure") => Some(handle_get_num_clones_in_structure(ctx, session, args)),
        // parity: feat:navigation.ansiblex-jump
        ("structureJumpBridgeMgr", "CmdJumpThroughStructureStargate") => Some(super::structure_lifecycle_rpc::handle_cmd_jump_through_structure_stargate(ctx, session, args).await),
        // parity: feat:navigation.ansiblex-link
        ("structureJumpBridgeMgr", "GetLinkedStructure") => Some(super::structure_lifecycle_rpc::handle_get_linked_structure(ctx, session, args)),
        ("structureJumpBridgeMgr", "GetJbStructureDestination") => Some(super::structure_lifecycle_rpc::handle_get_jb_structure_destination(ctx, session, args)),
        ("structureDirectory", "GetJumpBridgesWithMyAccess") | ("structureDirectory", "GetNearbyJumpBridges") => Some(super::structure_lifecycle_rpc::handle_get_jump_bridges_with_my_access(ctx, session)),
        _ => None,
    }
}

pub(crate) fn require_structure_corp_member(snap: &WorldStateSnapshot, char_id: i64, structure_id: i64) -> Result<UpwellStructureRecord, BridgeRpcError> {
    let Some(st) = snap.structures.get(&structure_id).cloned() else {
        return Err(err("StructureNotFound", format!("Structure {structure_id} does not exist")));
    };
    if st.state == "Destroyed" {
        return Err(err("StructureDestroyed", format!("Structure {structure_id} has been destroyed")));
    }
    let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
    if st.owner_corp_id != caller_corp {
        return Err(err("StructureOwnershipViolation", format!("Character {char_id} (corp {caller_corp}) does not belong to owning corp {} of structure {structure_id}", st.owner_corp_id)));
    }
    Ok(st)
}

fn handle_structure_unanchor(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "Unanchor requires structureID"))?;
    let now_unix = ctx.now_unix();
    let (st, until, home_moved_chars) = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        let under_repair = ext.lifecycle.get(&structure_id).and_then(|l| l.repair_until_unix).is_some_and(|t| now_unix < t);
        if st.state.starts_with("Reinforced") || !matches!(st.state.as_str(), "ShieldVulnerable" | "Anchored") || now_unix < st.reinforced_until_unix || under_repair {
            return Err(err("CannotUnanchorReinforcedStructure", format!("Structure {structure_id} cannot be unanchored while reinforced or under repair")));
        }
        if ext.unanchoring_until_unix.contains_key(&structure_id) {
            return Err(err("StructureAlreadyUnanchoring", format!("Structure {structure_id} is already unanchoring")));
        }
        let until = now_unix + UNANCHOR_TIMER_SECS;
        ext.unanchoring_until_unix.insert(structure_id, until);
        save_ext_state(snap, &ext);
        let affected: Vec<i64> = snap.characters.values().filter(|c| c.home_station_id == structure_id).map(|c| c.character_id).collect();
        let mut moved = Vec::new();
        for cid in affected {
            let new_home = super::structure_lifecycle::resolve_char_school_or_nearest_station(ctx, snap, cid, st.solar_system_id);
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.home_station_id = new_home;
                moved.push(cid);
            }
        }
        Ok((st, until, moved))
    })?;
    ctx.notify(crate::router::NotificationTarget::Corporation(st.owner_corp_id), "OnCorporationStructuresUpdated", "corpid", vec![]);
    let state_args = vec![PyRep::Integer(i64::from(st.solar_system_id)), PyRep::Integer(structure_id), PyRep::String("Unanchoring".into())];
    ctx.notify(crate::router::NotificationTarget::SolarSystem(st.solar_system_id), "OnStructureStateChanged", "solarsystemid", state_args.clone());
    ctx.notify(crate::router::NotificationTarget::SolarSystem(st.solar_system_id), "OnCurrentStructureStateChanged", "solarsystemid", state_args);
    for cid in if home_moved_chars.is_empty() { vec![char_id] } else { home_moved_chars } {
        ctx.notify(crate::router::NotificationTarget::Character(cid), "OnHomeStationMovedFromStructure", "charid", vec![]);
    }
    Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("state", PyRep::String("Unanchoring".into())), ("unanchorCompleteUnix", PyRep::Integer(until as i64))]))
}

fn handle_structure_cancel_unanchor(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "CancelUnanchor requires structureID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        if ext.unanchoring_until_unix.remove(&structure_id).is_none() {
            return Err(err("StructureNotUnanchoring", format!("Structure {structure_id} does not have an active unanchoring timer")));
        }
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("state", PyRep::String(st.state)), ("unanchoringCancelled", PyRep::Bool(true))]))
    })
}

fn handle_structure_rename(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "RenameStructure requires structureID"))?;
    let new_name = args.get(1).and_then(|a| a.as_str()).map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= 100)
        .ok_or_else(|| err("InvalidStructureName", "Structure name must be 1..=100 non-whitespace characters"))?.to_string();
    let st = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        if let Some(st_mut) = snap.structures.get_mut(&structure_id) { st_mut.name = new_name.clone(); }
        snap.item_custom_names.insert(structure_id, new_name.clone());
        Ok(st)
    })?;
    ctx.notify(crate::router::NotificationTarget::Corporation(st.owner_corp_id), "OnCorporationStructuresUpdated", "corpid", vec![]);
    let slim_kv = PyRep::keyval([("itemID", PyRep::Integer(structure_id)), ("typeID", PyRep::Integer(i64::from(st.type_id))), ("ownerID", PyRep::Integer(st.owner_corp_id)), ("name", PyRep::String(new_name.clone()))]);
    ctx.notify(crate::router::NotificationTarget::SolarSystem(st.solar_system_id), "OnSlimItemChange", "solarsystemid", vec![PyRep::Integer(structure_id), slim_kv]);
    Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("name", PyRep::String(new_name))]))
}

fn handle_update_reinforce_times(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "UpdateReinforceTimesFromClient requires structureID"))?;
    let hour = arg_i64(args, 1).unwrap_or(-1);
    let weekday = arg_i64(args, 2).unwrap_or(-1);
    if !(0..24).contains(&hour) || !(0..7).contains(&weekday) {
        return Err(err("InvalidReinforceSchedule", format!("Reinforce hour ({hour}) must be 0..23 and weekday ({weekday}) 0..6")));
    }
    let now_unix = ctx.now_unix();
    let (st, cur_hour, cur_day, apply_after, hacked_chars) = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        apply_pending_reinforce_schedule(ctx, snap, &mut ext, structure_id, now_unix);
        let (ch, cd) = snap.structure_vulnerability_windows.get(&structure_id).copied().unwrap_or((18, 5));
        let base_now = if st.state != "ShieldVulnerable" || now_unix < st.reinforced_until_unix { now_unix.max(st.reinforced_until_unix) } else { now_unix };
        let apply_after = base_now + reinforce_change_delay_secs(ctx, st.solar_system_id);
        ext.pending_reinforce_schedule.insert(structure_id, (hour as u8, weekday as u8, apply_after));
        let hacked_chars: Vec<i64> = ext.hacked_reinforce_reveals.get(&structure_id).cloned().unwrap_or_default().into_iter().collect();
        save_ext_state(snap, &ext);
        Ok((st, ch as i64, cd as i64, apply_after, hacked_chars))
    })?;
    let hack_tuple = PyRep::Tuple(vec![PyRep::Integer(cur_day), PyRep::Integer(cur_hour), PyRep::Integer(weekday), PyRep::Integer(hour), PyRep::Integer(apply_after as i64)]);
    ctx.notify(crate::router::NotificationTarget::Corporation(st.owner_corp_id), "OnStructureHacked", "corpid", vec![PyRep::Integer(structure_id), hack_tuple.clone()]);
    for hc in hacked_chars {
        ctx.notify(crate::router::NotificationTarget::Character(hc), "OnStructureHacked", "charid", vec![PyRep::Integer(structure_id), hack_tuple.clone()]);
    }
    Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("nextReinforceHour", PyRep::Integer(hour)), ("nextReinforceWeekday", PyRep::Integer(weekday)), ("applyAfterUnix", PyRep::Integer(apply_after as i64))]))
}

fn handle_move_assets_to_safety(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep], is_corp: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 1).ok_or_else(|| err("MissingStructureId", "MoveAssetsToSafety requires (solarSystemID, structureID)"))?;
    if !is_corp && session.structure_id == Some(structure_id) {
        return Err(err("CannotAssetSafetyWhileDockedInTargetStructure", format!("Cannot manually move personal assets to safety while docked inside structure {structure_id}")));
    }
    let now_unix = ctx.now_unix();
    let (wrap_id, owner_id, sys_id, count) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(st) = snap.structures.get(&structure_id).cloned() else { return Err(err("StructureNotFound", format!("Structure {structure_id} not found"))); };
        if st.state == "Destroyed" { return Err(err("StructureDestroyed", format!("Structure {structure_id} has already been destroyed"))); }
        if super::structure_lifecycle::space_class(ctx, st.solar_system_id) == super::structure_lifecycle::SpaceClass::Wormhole {
            return Err(err("AssetSafetyUnavailableInWormhole", format!("Asset safety is unavailable in wormhole system {}", st.solar_system_id)));
        }
        let owner_id = if is_corp { snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0) } else { char_id };
        if is_corp {
            if st.owner_corp_id != owner_id {
                return Err(err("StructureOwnershipViolation", format!("Character {char_id} (corp {owner_id}) cannot move corporate assets from structure {structure_id} owned by corp {}", st.owner_corp_id)));
            }
            let has_dir = session.has_gm_role()
                || (session.corp_role & eve_db::corp_roles::ROLE_DIRECTOR) != 0
                || snap.corporations.get(&owner_id).is_some_and(|c| c.ceo_id == char_id || c.member_roles.get(&char_id).is_some_and(|&r| (r & eve_db::corp_roles::ROLE_DIRECTOR) != 0))
                || snap.corp_role_assignments.get(&(owner_id, char_id)).is_some_and(|a| (a.roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0);
            if !has_dir { return Err(err("InsufficientCorpRoles", "Moving corporate assets to safety requires Director or CEO role")); }
        }
        let all_active_ships: HashSet<i64> = snap.characters.values().map(|c| c.active_ship_id).filter(|&id| id > 0).chain(session.ship_id.into_iter().filter(|&id| id > 0)).collect();
        let has_locked_descendant = |root_id: i64| -> bool {
            let mut visited = vec![root_id];
            let mut front = vec![root_id];
            while !front.is_empty() {
                let mut next = Vec::new();
                for ch in snap.items.values() {
                    if !front.contains(&ch.location_id) || visited.contains(&ch.item_id) {
                        continue;
                    }
                    if ch.flag == InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(ch.item_id)
                    {
                        return true;
                    }
                    visited.push(ch.item_id);
                    next.push(ch.item_id);
                }
                front = next;
            }
            false
        };
        let mut item_ids: Vec<i64> = snap.items.values().filter(|i| {
            i.owner_id == owner_id && i.location_id == structure_id && !all_active_ships.contains(&i.item_id)
                && !matches!(i.flag as u16, 11..=34 | 87 | 92..=94 | 125..=132 | 163..=172 | 180)
                && i.flag != InventoryFlag::SpecializedFuelBay && i.flag != InventoryFlag::StructureFuel && i.flag != InventoryFlag::StructureDeedBay && i.flag != InventoryFlag::Locked
                && !snap.is_item_locked_in_contract_or_job(i.item_id)
                && !has_locked_descendant(i.item_id)
        }).map(|i| i.item_id).collect();
        item_ids.sort_unstable();
        if item_ids.is_empty() { return Err(err("NoAssetsToMoveToSafety", format!("Owner {owner_id} has no eligible items in structure {structure_id}"))); }
        let mut all_ids_for_val = item_ids.clone();
        let mut frontier = item_ids.clone();
        while !frontier.is_empty() {
            let children: Vec<i64> = snap
                .items
                .values()
                .filter(|i| frontier.contains(&i.location_id) && !all_ids_for_val.contains(&i.item_id))
                .map(|i| i.item_id)
                .collect();
            all_ids_for_val.extend(children.iter().copied());
            frontier = children;
        }
        let est_val: f64 = all_ids_for_val
            .iter()
            .filter_map(|id| snap.items.get(id))
            .map(|it| ctx.sde.types.get(&it.type_id).map(|t| t.base_price).unwrap_or(0.0) * f64::from(it.quantity.max(1)))
            .sum();
        snap.next_entity_id += 1;
        let wrap_id = snap.next_entity_id;
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = wrap_id;
                it.flag = InventoryFlag::Locked;
            }
        }
        let nearest_station = super::structure_lifecycle::nearest_npc_station_from_pos(ctx, st.solar_system_id, glam::DVec3::from_array(st.position));
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(wrap_id, AssetSafetyWrapRecord {
            wrap_id, owner_id, is_corp, source_structure_id: structure_id, source_structure_name: st.name, solar_system_id: st.solar_system_id,
            created_unix: now_unix, deliverable_after_unix: now_unix + ASSET_SAFETY_DELIVERY_DELAY_SECS, delivered_to_id: None,
            item_ids: item_ids.clone(), estimated_value_isk: est_val, nearest_npc_station_id: nearest_station,
            auto_delivered: false, retrieval_fee_paid: false,
        });
        save_ext_state(snap, &ext);
        Ok((wrap_id, owner_id, st.solar_system_id, item_ids.len()))
    })?;
    let as_args = vec![PyRep::Integer(owner_id), PyRep::Integer(i64::from(sys_id)), PyRep::Integer(structure_id)];
    ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnAssetSafetyCreated", "charid", as_args.clone());
    if is_corp { ctx.notify(crate::router::NotificationTarget::Corporation(owner_id), "OnAssetSafetyCreated", "corpid", as_args); }
    Ok(PyRep::keyval([("wrapID", PyRep::Integer(wrap_id)), ("structureID", PyRep::Integer(structure_id)), ("ownerID", PyRep::Integer(owner_id)), ("itemCount", PyRep::Integer(count as i64)), ("deliverableAfterUnix", PyRep::Integer((now_unix + ASSET_SAFETY_DELIVERY_DELAY_SECS) as i64))]))
}

/// Lazily auto-delivers wraps older than 20 days to the nearest NPC station in the allowed security band (`feat:lifecycle.asset_safety-auto_delivery`).
pub fn sync_auto_delivered_asset_safety_wraps(ctx: &ServerContext) {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut changed = false;
        let wrap_ids: Vec<i64> = ext.asset_safety_wraps.keys().copied().collect();
        for wid in wrap_ids {
            let Some(w) = ext.asset_safety_wraps.get(&wid).cloned() else { continue };
            if w.delivered_to_id.is_none() && now_unix >= w.created_unix.saturating_add(ASSET_SAFETY_AUTO_DELIVERY_SECS) {
                let dest = w.nearest_npc_station_id.or_else(|| super::structure_lifecycle::nearest_npc_station(ctx, w.solar_system_id));
                if let Some(station_id) = dest {
                    if let Some(wm) = ext.asset_safety_wraps.get_mut(&wid) {
                        wm.nearest_npc_station_id = Some(station_id);
                        wm.delivered_to_id = Some(station_id);
                        wm.auto_delivered = true;
                        changed = true;
                    }
                    snap.items.entry(wid).or_insert_with(|| eve_db::InventoryItemRecord {
                        item_id: wid,
                        type_id: 60,
                        owner_id: w.owner_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    });
                }
            }
        }
        if changed {
            save_ext_state(snap, &ext);
        }
    });
}

fn handle_get_items_in_safety(ctx: &ServerContext, session: &mut SessionState, is_corp: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    sync_auto_delivered_asset_safety_wraps(ctx);
    ctx.db.with_snapshot(|snap| {
        let owner_id = if is_corp { snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0) } else { char_id };
        let ext = load_ext_state(snap);
        let mut wraps: Vec<&AssetSafetyWrapRecord> = ext
            .asset_safety_wraps
            .values()
            .filter(|w| w.owner_id == owner_id && w.is_corp == is_corp && (w.delivered_to_id.is_none() || (w.auto_delivered && !w.retrieval_fee_paid)))
            .collect();
        wraps.sort_by_key(|w| w.wrap_id);
        let rows = wraps.into_iter().map(|w| PyRep::keyval([
            ("assetWrapID", PyRep::Integer(w.wrap_id)), ("ownerID", PyRep::Integer(w.owner_id)), ("solarSystemID", PyRep::Integer(i64::from(w.solar_system_id))),
            ("deliverableAfterUnix", PyRep::Integer(w.deliverable_after_unix as i64)), ("itemCount", PyRep::Integer(w.item_ids.len() as i64)),
            ("nearestNpcStationID", w.nearest_npc_station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("deliveredToID", w.delivered_to_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("autoDelivered", PyRep::Bool(w.auto_delivered)),
        ])).collect();
        Ok(PyRep::List(rows))
    })
}

fn handle_get_wrap_names(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    sync_auto_delivered_asset_safety_wraps(ctx);
    let ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    ctx.db.with_snapshot(|snap| {
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        let ext = load_ext_state(snap);
        Ok(PyRep::Dict(
            ids.into_iter()
                .filter_map(|wid| {
                    ext.asset_safety_wraps.get(&wid).filter(|w| {
                        w.owner_id == char_id
                            || (w.is_corp && caller_corp > 0 && w.owner_id == caller_corp)
                            || session.has_gm_role()
                    }).map(|w| (PyRep::Integer(wid), PyRep::String(w.source_structure_name.clone())))
                })
                .collect(),
        ))
    })
}

/// `GetStructuresICanDeliverTo(solarSystemID)`: live structures and NPC stations in that system.
fn handle_get_structures_i_can_deliver_to(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = arg_i64(args, 0).and_then(|v| u32::try_from(v).ok()).or(session.solar_system_id).ok_or_else(|| err("MissingSolarSystemId", "GetStructuresICanDeliverTo requires solarSystemID"))?;
    let mut rows: Vec<(i64, u32)> = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .filter(|s| s.solar_system_id == sys_id && s.state != "Destroyed" && evaluate_character_structure_access_in_snap(snap, s.structure_id, char_id))
            .map(|s| (s.structure_id, s.type_id))
            .collect()
    });
    rows.extend(ctx.sde.stations.values().filter(|s| s.solar_system_id == sys_id).map(|s| (s.station_id, s.station_type_id)));
    rows.sort_unstable();
    Ok(PyRep::List(rows.into_iter().map(|(id, type_id)| PyRep::keyval([("itemID", PyRep::Integer(id)), ("typeID", PyRep::Integer(i64::from(type_id))), ("solarSystemID", PyRep::Integer(i64::from(sys_id)))])).collect()))
}

fn handle_move_safety_wrap_to_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    sync_auto_delivered_asset_safety_wraps(ctx);
    let wrap_id = arg_i64(args, 0).ok_or_else(|| err("MissingWrapId", "MoveSafetyWrapToStructure requires wrapID"))?;
    let explicit_dest_id = arg_i64(args, 1);
    let now_unix = ctx.now_unix();
    let (owner_id, is_corp, bal_f64, resp) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let Some(wrap) = ext.asset_safety_wraps.get(&wrap_id).cloned() else { return Err(err("AssetSafetyWrapNotFound", format!("Asset safety wrap {wrap_id} not found"))); };
        let dest_id = explicit_dest_id
            .or(wrap.delivered_to_id)
            .or(wrap.nearest_npc_station_id)
            .ok_or_else(|| err("MissingDestinationId", "MoveSafetyWrapToStructure requires destination ID"))?;
        let dest_is_station = ctx.sde.stations.contains_key(&dest_id);
        if !dest_is_station && !snap.structures.get(&dest_id).map(|s| s.state != "Destroyed").unwrap_or(false) {
            return Err(err("InvalidAssetSafetyDestination", format!("Destination {dest_id} is not a station or live structure")));
        }
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if (wrap.is_corp && wrap.owner_id != caller_corp) || (!wrap.is_corp && wrap.owner_id != char_id) {
            return Err(err("NotAssetSafetyWrapOwner", format!("Wrap {wrap_id} is not owned by the caller")));
        }
        if wrap.is_corp {
            let has_dir = session.has_gm_role() || (session.corp_role & eve_db::corp_roles::ROLE_DIRECTOR) != 0
                || snap.corporations.get(&caller_corp).is_some_and(|c| c.ceo_id == char_id || c.member_roles.get(&char_id).is_some_and(|&r| (r & eve_db::corp_roles::ROLE_DIRECTOR) != 0))
                || snap.corp_role_assignments.get(&(caller_corp, char_id)).is_some_and(|a| (a.roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0);
            if !has_dir { return Err(err("InsufficientCorpRoles", "Delivering corporate asset safety wrap requires Director or CEO role")); }
        }
        if wrap.delivered_to_id.is_some() && (!wrap.auto_delivered || wrap.retrieval_fee_paid) {
            return Err(err("AssetSafetyWrapAlreadyDelivered", format!("Asset safety wrap {wrap_id} has already been delivered")));
        }
        if now_unix < wrap.deliverable_after_unix {
            return Err(err("AssetSafetyCooldownActive", format!("Wrap {wrap_id} deliverable after {} (now {now_unix})", wrap.deliverable_after_unix)));
        }
        if wrap.auto_delivered {
            if Some(dest_id) != wrap.delivered_to_id && Some(dest_id) != wrap.nearest_npc_station_id {
                return Err(err("InvalidAssetSafetyDestination", format!("Auto-delivered wrap {wrap_id} must be retrieved at destination station")));
            }
        }
        let dest_sys = if let Some(st) = snap.structures.get(&dest_id) { Some(st.solar_system_id) } else { ctx.sde.stations.get(&dest_id).map(|s| s.solar_system_id) };
        if dest_sys != Some(wrap.solar_system_id) && Some(dest_id) != wrap.nearest_npc_station_id && Some(dest_id) != wrap.delivered_to_id {
            return Err(err(
                "InvalidAssetSafetyDestination",
                format!(
                    "Destination {dest_id} is not in source system {} or nearest NPC station",
                    wrap.solar_system_id
                ),
            ));
        }
        if !dest_is_station && !evaluate_character_structure_access_in_snap(snap, dest_id, char_id) {
            return Err(err("InvalidAssetSafetyDestination", format!("Character {char_id} does not have docking access to structure {dest_id}")));
        }
        let fee_rate = if !wrap.auto_delivered && dest_sys == Some(wrap.solar_system_id) && now_unix < wrap.created_unix + ASSET_SAFETY_AUTO_DELIVERY_SECS {
            0.005
        } else {
            ASSET_SAFETY_FEE_FRACTION
        };
        let fee = eve_db::IskCents::from_f64_isk(wrap.estimated_value_isk * fee_rate);
        let (payer, bal_f64) = if wrap.is_corp {
            let corp = snap.corporations.get_mut(&wrap.owner_id).ok_or_else(|| err("CorpNotFound", format!("Corp {} not found", wrap.owner_id)))?;
            let cur = corp.division_balance(1);
            if cur < fee { return Err(err("InsufficientFunds", format!("Delivering corp wrap {wrap_id} requires fee {fee:?}"))); }
            let new_bal = cur.checked_sub(fee).unwrap_or(cur);
            corp.set_division_balance(1, new_bal);
            if fee.0 > 0 {
                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;
                let desc = format!("Asset Safety recovery fee for wrap {wrap_id} to destination {dest_id}");
                snap.corp_wallet_journals.entry((wrap.owner_id, 1)).or_default().push(eve_db::CorpWalletJournalRecord {
                    id: jid, date_unix: now_unix, ref_type: "AssetSafetyRecoveryTax".into(),
                    first_party_id: Some(wrap.owner_id), second_party_id: Some(dest_id),
                    amount: -fee.to_wire_f64(), balance: new_bal.to_wire_f64(), description: desc.clone(), reason: Some(desc.clone()),
                });
                snap.append_journal(wrap.owner_id, "AssetSafetyRecovery", eve_db::IskCents(-fee.0), new_bal, desc);
            }
            (wrap.owner_id, new_bal.to_wire_f64())
        } else {
            let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
            if ch.balance < fee { return Err(err("InsufficientFunds", format!("Delivering wrap {wrap_id} requires fee {fee:?}"))); }
            let new_bal = ch.balance.checked_sub(fee).unwrap_or(ch.balance);
            ch.balance = new_bal;
            if fee.0 > 0 {
                snap.append_journal(char_id, "AssetSafetyRecoveryTax", eve_db::IskCents(-fee.0), new_bal, format!("Asset Safety recovery fee for wrap {wrap_id} to destination {dest_id}"));
            }
            (char_id, new_bal.to_wire_f64())
        };
        snap.items.remove(&wrap_id);
        for &iid in &wrap.item_ids {
            if let Some(it) = snap.items.get_mut(&iid) { it.location_id = dest_id; it.flag = InventoryFlag::Deliveries; }
        }
        if let Some(w) = ext.asset_safety_wraps.get_mut(&wrap_id) {
            w.delivered_to_id = Some(dest_id);
            w.retrieval_fee_paid = true;
        }
        save_ext_state(snap, &ext);
        Ok((payer, wrap.is_corp, bal_f64, PyRep::keyval([
            ("wrapID", PyRep::Integer(wrap_id)), ("destinationID", PyRep::Integer(dest_id)),
            ("deliveredItemCount", PyRep::Integer(wrap.item_ids.len() as i64)), ("feePaidISK", PyRep::Real(fee.to_wire_f64())),
            ("autoDelivered", PyRep::Bool(wrap.auto_delivered)),
        ])))
    })?;
    ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnAssetSafetyDelivered", "charid", vec![PyRep::Integer(owner_id)]);
    if is_corp {
        ctx.notify(crate::router::NotificationTarget::Corporation(owner_id), "OnAssetSafetyDelivered", "corpid", vec![PyRep::Integer(owner_id)]);
        ctx.notify(crate::router::NotificationTarget::Corporation(owner_id), "OnAccountChange", "corpid", vec![PyRep::String("cash".into()), PyRep::Integer(owner_id), PyRep::Real(bal_f64)]);
    } else {
        ctx.notify_account_change(char_id, "cash", owner_id, bal_f64);
    }
    Ok(resp)
}

pub(crate) fn require_structure_control_role(snap: &WorldStateSnapshot, char_id: i64, st: &UpwellStructureRecord) -> Result<(), BridgeRpcError> {
    if let Some(corp) = snap.corporations.get(&st.owner_corp_id) {
        let member_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        const CTRL_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER | eve_db::corp_roles::ROLE_STARBASE_OPERATOR | eve_db::corp_roles::ROLE_STARBASE_CONFIG;
        if corp.ceo_id != 0 && char_id != corp.ceo_id && (member_role & CTRL_ROLES) == 0 {
            return Err(err("CorpRoleDenied", format!("Character {char_id} lacks structure defense/control role for structure {}", st.structure_id)));
        }
    }
    Ok(())
}

pub(crate) fn emit_ship_session_change(ctx: &ServerContext, session: &mut SessionState, char_id: i64, old_ship: Option<i64>, new_ship: Option<i64>) {
    let old_py = old_ship.filter(|&s| s > 0).map(PyRep::Integer).unwrap_or(PyRep::None);
    let new_py = new_ship.filter(|&s| s > 0).map(PyRep::Integer).unwrap_or(PyRep::None);
    let sess_rep = PyRep::keyval([
        ("charid", PyRep::Integer(char_id)), ("shipid", new_py.clone()),
        ("stationid", session.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("structureid", session.structure_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("solarsystemid2", session.solar_system_id.map(|s| PyRep::Integer(i64::from(s))).unwrap_or(PyRep::None)),
    ]);
    let args = vec![PyRep::Bool(false), sess_rep, PyRep::Dict(vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![old_py, new_py]))])];
    session.push_notification("OnSessionChanged", args.clone());
    ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnSessionChanged", "charid", args);
}

fn handle_take_control(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "TakeControl requires structureID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} does not exist")))?;
        if st.state == "Destroyed" { return Err(err("StructureDestroyed", format!("Structure {structure_id} has been destroyed"))); }
        if session.structure_id != Some(structure_id) {
            return Err(err("NotDockedInStructure", format!("Must be docked in structure {structure_id} to take control")));
        }
        let mut ext = load_ext_state(snap);
        apply_pending_profile_applications(&mut ext, ctx.now_unix());
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let has_defense_acl = if let Some(p) = ext.access_profiles.get(&structure_id) {
            if p.blocked_characters.contains(&char_id) || p.defense_blocked_characters.contains(&char_id) {
                return Err(err("DefenseAccessDenied", format!("Character {char_id} is blocked by structure profile")));
            }
            for &gid in &p.defense_group_ids {
                if let Some(grp) = ext.owner_groups.get(&gid) {
                    if grp.members.get(&char_id) == Some(&-1) {
                        return Err(err("DefenseAccessDenied", format!("Character {char_id} is blocked in defense access group {gid}")));
                    }
                }
            }
            p.defense_allowed_characters.contains(&char_id)
                || p.defense_group_ids.iter().any(|gid| {
                    ext.owner_groups.get(gid).is_some_and(|g| {
                        g.members.get(&char_id).is_some_and(|&r| r >= 0)
                            || (caller_corp > 0 && g.members.get(&caller_corp).is_some_and(|&r| r >= 0))
                    })
                })
        } else {
            false
        };

        if !has_defense_acl {
            if st.owner_corp_id != caller_corp {
                return Err(err("DefenseAccessDenied", format!("Character {char_id} does not belong to owning corp {} of structure {structure_id} and has no defense ACL permission", st.owner_corp_id)));
            }
            require_structure_control_role(snap, char_id, &st).map_err(|_| err("DefenseAccessDenied", format!("Character {char_id} lacks structure control role for structure {structure_id}")))?;
        }
        if let Some(&pilot) = ext.active_pilot_by_structure.get(&structure_id) {
            if pilot != char_id {
                return Err(err("StructureAlreadyControlled", format!("Structure {structure_id} is already controlled by pilot {pilot}")));
            }
        }
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        let prev = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id)).unwrap_or(0);
        if prev > 0 && prev != structure_id { ext.previous_ship_by_structure.insert(structure_id, prev); }
        save_ext_state(snap, &ext);
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = structure_id; }
        Ok(())
    })?;
    let old_ship = session.ship_id;
    session.ship_id = Some(structure_id);
    emit_ship_session_change(ctx, session, char_id, old_ship, Some(structure_id));
    Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("pilotID", PyRep::Integer(char_id)), ("success", PyRep::Bool(true))]))
}

fn handle_set_structure_description(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "SetStructureDescription requires structureID"))?;
    let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
    let st = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        ext.descriptions.insert(structure_id, desc);
        save_ext_state(snap, &ext);
        Ok(st)
    })?;
    ctx.notify(crate::router::NotificationTarget::Corporation(st.owner_corp_id), "OnProfileSettingsChange", "corpid", vec![PyRep::List(vec![PyRep::Integer(structure_id)])]);
    ctx.notify(crate::router::NotificationTarget::Corporation(st.owner_corp_id), "OnOwnerGroupsUpdated", "corpid", vec![PyRep::Dict(vec![(PyRep::Integer(st.owner_corp_id), PyRep::Integer(structure_id))])]);
    ctx.notify(crate::router::NotificationTarget::SolarSystem(st.solar_system_id), "OnDockingAccessChangedForCurrentSolarSystem", "solarsystemid", vec![]);
    Ok(PyRep::None)
}

fn handle_get_structure_description(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "GetStructureDescription requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        if !snap.structures.get(&structure_id).is_some_and(|s| s.state != "Destroyed") {
            return Err(err("StructureNotFound", format!("Structure {structure_id} does not exist")));
        }
        Ok(load_ext_state(snap).descriptions.get(&structure_id).cloned().map(PyRep::String).unwrap_or(PyRep::None))
    })
}

fn handle_structure_get_guests(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "structureGuests.GetGuests requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        let mut guests: Vec<(i64, i64, Option<i64>)> = snap.characters.values().filter(|c| c.structure_id == Some(structure_id)).map(|c| (c.character_id, c.corporation_id, c.alliance_id)).collect();
        guests.sort_by_key(|g| g.0);
        Ok(PyRep::List(guests.into_iter().map(|(cid, corp_id, ally_id)| PyRep::Tuple(vec![PyRep::Integer(cid), PyRep::Integer(corp_id), ally_id.map(PyRep::Integer).unwrap_or(PyRep::None)])).collect()))
    })
}

fn handle_get_hangar_view_state(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = session.structure_id.ok_or_else(|| err("NotDockedInStructure", "GetMyHangarViewState requires being docked in an Upwell structure"))?;
    let ship_id = session.ship_id;
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        let ship_type = ship_id.and_then(|sid| snap.items.get(&sid)).map(|i| PyRep::Integer(i64::from(i.type_id))).unwrap_or(PyRep::None);
        Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(char_id)), ("structureID", PyRep::Integer(structure_id)),
            ("structureTypeID", PyRep::Integer(i64::from(st.type_id))), ("shipID", ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)), ("shipTypeID", ship_type),
        ]))
    })
}

fn handle_get_num_clones_in_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "GetNumClonesInPilotsStructure requires structureID"))?;
    let count = ctx.db.with_snapshot(|snap| snap.jump_clones.get(&char_id).map(|clones| clones.iter().filter(|jc| jc.location_id == structure_id).count()).unwrap_or(0));
    Ok(PyRep::Integer(count as i64))
}

/// Maps SDE service module `type_id` to V24 structure service IDs.
pub fn service_id_for_module_type(type_id: u32) -> Option<i64> {
    match type_id {
        35892 => Some(1), // Market (`Standup Market Hub I`)
        35894 => Some(2), // Clone Bay (`Standup Cloning Center I`)
        35899 => Some(3), // Reprocessing (`Standup Reprocessing Facility I`)
        35878 => Some(4), // Manufacturing (`Standup Manufacturing Plant I`)
        35891 => Some(5), // Research (`Standup Hyasyoda Research Lab`)
        35886 => Some(6), // Invention (`Standup Invention Lab I`)
        45537..=45539 => Some(7), // Reactions (`Standup Composite/Biochemical/Hybrid Reactor I`)
        45009 => Some(8), // Moon Drilling (`Standup Moon Drill I`)
        35913 => Some(9), // Jump Bridge (`Standup Coaxial Regenerative Projector`)
        _ => None,
    }
}

/// Checks whether a structure allows market order placement (`feat:structures.service-market`).
pub fn structure_allows_market_orders(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> bool {
    use super::structure_lifecycle as life;
    let Some(st) = snap.structures.get(&structure_id) else { return false; };
    if !life::is_operational(&st.state) || !st.quantum_core_installed || st.fuel_blocks == 0 { return false; }
    let Some(profile) = life::structure_profile(ctx, st.type_id) else { return false; };
    !matches!(profile.hull_class, life::HullClass::Medium | life::HullClass::Flex)
        && snap.items.values().any(|i| i.location_id == structure_id && i.type_id == 35_892 && (164..=171).contains(&(i.flag as u16)) && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true) && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0)
}

pub fn evaluate_character_structure_access(_ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64, char_id: i64) -> bool {
    evaluate_character_structure_access_in_snap(snap, structure_id, char_id)
}

pub fn evaluate_character_structure_access_in_snap(snap: &WorldStateSnapshot, structure_id: i64, char_id: i64) -> bool {
    if snap.structure_docking_acl_denied.get(&structure_id).is_some_and(|s| s.contains(&char_id)) { return false; }
    let (char_corp, char_ally) = snap.characters.get(&char_id).map(|c| (c.corporation_id, c.alliance_id.unwrap_or(0))).unwrap_or((0, 0));
    let owner_corp = snap.structures.get(&structure_id).map(|s| s.owner_corp_id).unwrap_or(0);
    let ext = load_ext_state(snap);
    if let Some(prof) = ext.access_profiles.get(&structure_id) {
        if prof.blocked_characters.contains(&char_id) || prof.blocked_corporations.contains(&char_corp) || (char_ally > 0 && prof.blocked_alliances.contains(&char_ally)) { return false; }
        for &gid in &prof.access_group_ids {
            if let Some(grp) = ext.owner_groups.get(&gid) {
                if grp.members.get(&char_id) == Some(&-1) || grp.members.get(&char_corp) == Some(&-1) || (char_ally > 0 && grp.members.get(&char_ally) == Some(&-1)) { return false; }
            }
        }
        if prof.corp_only && owner_corp != 0 && char_corp != owner_corp { return false; }
        let has_group_allows = prof.access_group_ids.iter().any(|gid| ext.owner_groups.get(gid).is_some_and(|g| g.members.values().any(|&r| r >= 0)));
        let in_group = prof.access_group_ids.iter().any(|gid| ext.owner_groups.get(gid).is_some_and(|g| g.members.get(&char_id).is_some_and(|&r| r >= 0) || g.members.get(&char_corp).is_some_and(|&r| r >= 0) || (char_ally > 0 && g.members.get(&char_ally).is_some_and(|&r| r >= 0))));
        let has_allow = !prof.allowed_characters.is_empty() || !prof.allowed_corporations.is_empty() || !prof.allowed_alliances.is_empty() || has_group_allows;
        if has_allow && char_corp != owner_corp && !prof.allowed_characters.contains(&char_id) && !prof.allowed_corporations.contains(&char_corp) && (char_ally <= 0 || !prof.allowed_alliances.contains(&char_ally)) && !in_group { return false; }
        return true;
    }
    let is_player_corp = owner_corp >= 98_000_000 || snap.corporations.get(&owner_corp).is_some_and(|c| c.ceo_id != 0 || !c.members.is_empty());
    !is_player_corp || owner_corp == char_corp
}

pub(crate) fn require_corp_profile_manager(snap: &WorldStateSnapshot, session: &SessionState, char_id: i64, corp_id: i64) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() { return Ok(()); }
    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
    if corp_id != 0 && char_corp != corp_id { return Err(err("StructureOwnershipViolation", "Caller does not belong to the owning corporation")); }
    let req = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    let has_role = (session.corp_role & req) != 0 || snap.corporations.get(&corp_id).is_some_and(|c| c.ceo_id == char_id || c.member_roles.get(&char_id).is_some_and(|&r| (r & req) != 0));
    if !has_role { return Err(err("InsufficientCorpRoles", "Structure profile and ACL configuration requires Director or Station Manager role")); }
    Ok(())
}

pub(crate) fn require_structure_corp_manager(snap: &WorldStateSnapshot, session: &SessionState, char_id: i64, structure_id: i64) -> Result<UpwellStructureRecord, BridgeRpcError> {
    if session.has_gm_role() { return snap.structures.get(&structure_id).cloned().ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found"))); }
    let st = require_structure_corp_member(snap, char_id, structure_id)?;
    require_corp_profile_manager(snap, session, char_id, st.owner_corp_id)?;
    Ok(st)
}

pub(crate) fn validate_and_classify_entity(snap: &WorldStateSnapshot, entity_id: i64) -> Result<(bool, bool, bool), BridgeRpcError> {
    let (is_ally, is_corp, is_char) = (snap.alliances.contains_key(&entity_id), snap.corporations.contains_key(&entity_id), snap.characters.contains_key(&entity_id));
    if !is_ally && !is_corp && !is_char { return Err(err("InvalidAccessEntity", format!("Entity {entity_id} is not a known character, corporation, or alliance"))); }
    Ok((is_ally, is_corp, is_char))
}

pub(crate) fn apply_entity_to_profile(prof: &mut StructureAccessProfile, eid: i64, is_ally: bool, is_corp: bool, is_blocked: bool, is_add: bool) {
    let target_set = if is_ally { if is_blocked { &mut prof.blocked_alliances } else { &mut prof.allowed_alliances } }
        else if is_corp { if is_blocked { &mut prof.blocked_corporations } else { &mut prof.allowed_corporations } }
        else if is_blocked { &mut prof.blocked_characters } else { &mut prof.allowed_characters };
    if is_add { target_set.insert(eid); } else { target_set.remove(&eid); }
}

pub(crate) fn sync_assigned_structures_for_profile(ext: &mut StructureExtendedState, profile_id: i64) -> Vec<i64> {
    let Some(prof) = ext.profiles_by_id.get(&profile_id).cloned() else { return Vec::new(); };
    let mut updated = Vec::new();
    for (&sid, &pid) in &ext.structure_profile_id {
        if pid == profile_id {
            ext.access_profiles.insert(sid, prof.clone());
            updated.push(sid);
        }
    }
    updated
}

pub fn configure_structure_access_profile(
    ctx: &ServerContext, session: &SessionState, structure_id: i64, corp_only: Option<bool>,
    service_tax_rates: &[(i64, f64)], general_settings: &[(i64, bool)], blocked_entities: &[i64], allowed_entities: &[i64],
) -> Result<(), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let _ = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        let mut parsed_taxes = Vec::with_capacity(service_tax_rates.len());
        for &(sid, tax) in service_tax_rates {
            if !tax.is_finite() || !(0.0..=1.0).contains(&tax) {
                return Err(err("InvalidStructureTaxRate", "Structure service tax rate must be a finite number in 0.0..=1.0"));
            }
            parsed_taxes.push((sid, (tax * 10_000.0).round() as i64));
        }
        let classified_blocked: Vec<_> = blocked_entities.iter().map(|&eid| Ok((eid, validate_and_classify_entity(snap, eid)?))).collect::<Result<_, BridgeRpcError>>()?;
        let classified_allowed: Vec<_> = allowed_entities.iter().map(|&eid| Ok((eid, validate_and_classify_entity(snap, eid)?))).collect::<Result<_, BridgeRpcError>>()?;
        let mut ext = load_ext_state(snap);
        let prof = ext.access_profiles.entry(structure_id).or_default();
        if let Some(co) = corp_only { prof.corp_only = co; }
        for (sid, bps) in parsed_taxes { prof.service_settings.insert(sid, bps); }
        for &(gid, val) in general_settings { prof.general_settings.insert(gid, val); }
        prof.blocked_alliances.clear(); prof.blocked_corporations.clear(); prof.blocked_characters.clear();
        for (eid, (is_ally, is_corp, _)) in classified_blocked { apply_entity_to_profile(prof, eid, is_ally, is_corp, true, true); }
        prof.allowed_alliances.clear(); prof.allowed_corporations.clear(); prof.allowed_characters.clear();
        for (eid, (is_ally, is_corp, _)) in classified_allowed { apply_entity_to_profile(prof, eid, is_ally, is_corp, false, true); }
        save_ext_state(snap, &ext);
        Ok(())
    })
}

pub fn modify_structure_access_node(
    ctx: &ServerContext, session: &SessionState, structure_id: i64, entity_id: i64, is_blocked: bool, is_add: bool,
) -> Result<(), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let _ = require_structure_corp_manager(snap, session, char_id, structure_id)?;
        let (is_ally, is_corp, _) = validate_and_classify_entity(snap, entity_id)?;
        let mut ext = load_ext_state(snap);
        let prof = ext.access_profiles.entry(structure_id).or_default();
        apply_entity_to_profile(prof, entity_id, is_ally, is_corp, is_blocked, is_add);
        save_ext_state(snap, &ext);
        Ok(())
    })
}

fn handle_create_profile(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let name = args.first().and_then(|a| a.as_str()).unwrap_or("New Profile").to_string();
    let description = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
    ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        require_corp_profile_manager(snap, session, char_id, corp_id)?;
        let mut ext = load_ext_state(snap);
        let profile_id = ext.profiles_by_id.keys().copied().max().unwrap_or(70_000_000) + 1;
        ext.profiles_by_id.insert(profile_id, StructureAccessProfile { profile_id, owner_corp_id: corp_id, name, description, ..Default::default() });
        save_ext_state(snap, &ext);
        Ok(PyRep::Integer(profile_id))
    })
}

fn handle_update_profile(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let profile_id = arg_i64(args, 0).ok_or_else(|| err("MissingProfileId", "UpdateProfile requires profileID"))?;
    let name = args.get(1).and_then(|a| a.as_str()).map(str::to_string);
    let desc = args.get(2).and_then(|a| a.as_str()).map(str::to_string);
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let owner_corp = ext.profiles_by_id.get(&profile_id).map(|p| p.owner_corp_id).ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, owner_corp)?;
        if let Some(prof) = ext.profiles_by_id.get_mut(&profile_id) {
            if let Some(n) = name { prof.name = n; }
            if let Some(d) = desc { prof.description = d; }
        }
        sync_assigned_structures_for_profile(&mut ext, profile_id);
        save_ext_state(snap, &ext);
        Ok(PyRep::Bool(true))
    })
}

fn handle_change_profiles(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(xs) | PyRep::Tuple(xs)) => xs.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if structure_ids.is_empty() { return Err(err("MissingStructureId", "ChangeProfiles requires structureIDs")); }
    let profile_id = arg_i64(args, 1).ok_or_else(|| err("MissingProfileId", "ChangeProfiles requires profileID"))?;
    let (owner_corp, sys_ids) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext.profiles_by_id.get(&profile_id).cloned().ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, prof.owner_corp_id)?;
        let mut sys_ids = Vec::new();
        for &sid in &structure_ids {
            let st = require_structure_corp_manager(snap, session, char_id, sid)?;
            if prof.owner_corp_id != 0 && st.owner_corp_id != prof.owner_corp_id {
                return Err(err("StructureOwnershipViolation", "Profile and structure must belong to the same corporation"));
            }
            sys_ids.push(st.solar_system_id);
        }
        let now_unix = ctx.now_unix();
        for &sid in &structure_ids {
            if ext.structure_profile_id.contains_key(&sid) {
                ext.pending_profile_applications.insert(sid, (profile_id, now_unix + PROFILE_CHANGE_DELAY_SECS));
            } else {
                ext.structure_profile_id.insert(sid, profile_id);
                ext.access_profiles.insert(sid, prof.clone());
            }
        }
        save_ext_state(snap, &ext);
        Ok((prof.owner_corp_id, sys_ids))
    })?;
    ctx.notify(crate::router::NotificationTarget::Corporation(owner_corp), "OnProfileSettingsChange", "corpid", vec![PyRep::List(structure_ids.into_iter().map(PyRep::Integer).collect())]);
    for sys_id in sys_ids {
        ctx.notify(crate::router::NotificationTarget::SolarSystem(sys_id), "OnDockingAccessChangedForCurrentSolarSystem", "solarsystemid", vec![]);
    }
    Ok(PyRep::Bool(true))
}

fn handle_save_profile_settings(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let target_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingProfileId", "SaveProfileSettings requires profileID"))?;
    let mut corp_only = None;
    let mut service_taxes = Vec::new();
    let mut general_settings = Vec::new();
    let mut blocked = Vec::new();
    let mut allowed = Vec::new();
    let mut group_ids = Vec::new();
    let mut defense_group_ids = Vec::new();
    let payload = args.get(1).or_else(|| args.get(2));
    if let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload {
        for r in rows {
            if let PyRep::Tuple(items) | PyRep::List(items) = r {
                if let (Some(sid), Some(val)) = (items.first().and_then(|x| x.as_i64()), items.get(1)) {
                    if items.len() >= 3 {
                        if let Some(gid) = val.as_i64().filter(|&g| g > 0) {
                            if sid == 17 { defense_group_ids.push(gid); } else { group_ids.push(gid); }
                        }
                        if let Some(v2) = items.get(2) {
                            match v2 {
                                PyRep::Real(rate) => service_taxes.push((sid, *rate)),
                                PyRep::Bool(flag) => general_settings.push((sid, *flag)),
                                PyRep::Integer(v) => if sid <= 2 || sid == 17 || sid == 19 { general_settings.push((sid, *v != 0)); } else { service_taxes.push((sid, *v as f64)); },
                                _ => {}
                            }
                        }
                    } else {
                        match val {
                            PyRep::Real(rate) => service_taxes.push((sid, *rate)),
                            PyRep::Bool(flag) => general_settings.push((sid, *flag)),
                            PyRep::Integer(v) => if *v >= 80_000_001 {
                                if sid == 17 { defense_group_ids.push(*v); } else { group_ids.push(*v); }
                            } else {
                                service_taxes.push((sid, *v as f64));
                            },
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    let pairs: Vec<(String, PyRep)> = match payload {
        Some(PyRep::Dict(d)) => d.iter().filter_map(|(k, v)| k.as_str().map(|s| (s.to_string(), v.clone()))).collect(),
        Some(PyRep::KeyVal(kv)) => kv.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => Vec::new(),
    };
    let extract_ids = |rep: &PyRep| -> Vec<i64> {
        match rep {
            PyRep::List(xs) | PyRep::Tuple(xs) => xs.iter().filter_map(|x| x.as_i64()).collect(),
            PyRep::Integer(id) => vec![*id],
            _ => Vec::new(),
        }
    };
    for (k, v) in pairs {
        match k.as_str() {
            "corpOnly" | "corp_only" => corp_only = Some(matches!(v, PyRep::Bool(true) | PyRep::Integer(1))),
            "blocked" | "blockedEntities" => blocked = extract_ids(&v),
            "allowed" | "allowedEntities" => allowed = extract_ids(&v),
            "accessGroups" | "groupIDs" => group_ids.extend(extract_ids(&v)),
            "serviceSettings" | "services" => {
                if let PyRep::Dict(sd) = v {
                    for (sk, sv) in sd {
                        if let Some(sid) = sk.as_i64() {
                            let rate = match sv { PyRep::Real(r) => r, PyRep::Integer(i) => i as f64, _ => f64::NAN };
                            service_taxes.push((sid, rate));
                        }
                    }
                }
            }
            "generalSettings" | "settings" => {
                if let PyRep::Dict(gd) = v {
                    for (gk, gv) in gd {
                        if let Some(gid) = gk.as_i64() {
                            general_settings.push((gid, matches!(gv, PyRep::Bool(true) | PyRep::Integer(1))));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let is_standalone_profile = ctx.db.with_snapshot(|snap| load_ext_state(snap).profiles_by_id.contains_key(&target_id));
    if is_standalone_profile {
        let (_, char_id) = ServerContext::require_character(session)?;
        ctx.db.try_with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let owner_corp = ext.profiles_by_id.get(&target_id).map(|p| p.owner_corp_id).unwrap_or(0);
            require_corp_profile_manager(snap, session, char_id, owner_corp)?;
            let mut parsed_taxes = Vec::with_capacity(service_taxes.len());
            for &(sid, tax) in &service_taxes {
                if !tax.is_finite() || !(0.0..=1.0).contains(&tax) {
                    return Err(err("InvalidStructureTaxRate", "Structure service tax rate must be a finite number in 0.0..=1.0"));
                }
                parsed_taxes.push((sid, (tax * 10_000.0).round() as i64));
            }
            let classified_blocked: Vec<_> = blocked.iter().map(|&eid| Ok((eid, validate_and_classify_entity(snap, eid)?))).collect::<Result<_, BridgeRpcError>>()?;
            let classified_allowed: Vec<_> = allowed.iter().map(|&eid| Ok((eid, validate_and_classify_entity(snap, eid)?))).collect::<Result<_, BridgeRpcError>>()?;
            let mut group_members_to_sync = Vec::new();
            for &gid in group_ids.iter().chain(&defense_group_ids) {
                let grp = ext.owner_groups.get(&gid).ok_or_else(|| err("AccessGroupNotFound", format!("Access group {gid} not found")))?;
                if grp.owner_corp_id != 0 && grp.owner_corp_id != owner_corp {
                    return Err(err("StructureOwnershipViolation", "Cannot attach an access group owned by another corporation"));
                }
                if grp.owner_corp_id == 0 && grp.creator_id != char_id && !session.has_gm_role() {
                    return Err(err("StructureOwnershipViolation", "Cannot attach an access group owned by another character"));
                }
            }
            for &gid in &group_ids {
                if let Some(grp) = ext.owner_groups.get(&gid) {
                    for (&eid, &mtype) in &grp.members {
                        group_members_to_sync.push((eid, validate_and_classify_entity(snap, eid)?, mtype < 0));
                    }
                }
            }
            if let Some(prof) = ext.profiles_by_id.get_mut(&target_id) {
                if let Some(co) = corp_only { prof.corp_only = co; }
                for (sid, bps) in parsed_taxes { prof.service_settings.insert(sid, bps); }
                for &(gid, val) in &general_settings { prof.general_settings.insert(gid, val); }
                prof.access_group_ids.clear(); prof.blocked_characters.clear(); prof.allowed_characters.clear();
                prof.blocked_corporations.clear(); prof.allowed_corporations.clear(); prof.blocked_alliances.clear(); prof.allowed_alliances.clear();
                for &gid in &group_ids { prof.access_group_ids.insert(gid); }
                for (eid, (is_ally, is_corp, _), is_blk) in group_members_to_sync { apply_entity_to_profile(prof, eid, is_ally, is_corp, is_blk, true); }
                for (eid, (is_ally, is_corp, _)) in classified_blocked { apply_entity_to_profile(prof, eid, is_ally, is_corp, true, true); }
                for (eid, (is_ally, is_corp, _)) in classified_allowed { apply_entity_to_profile(prof, eid, is_ally, is_corp, false, true); }
                prof.defense_group_ids.clear();
                for &gid in &defense_group_ids { prof.defense_group_ids.insert(gid); }
                prof.defense_allowed_characters.clear();
                prof.defense_blocked_characters.clear();
            }
            sync_assigned_structures_for_profile(&mut ext, target_id);
            save_ext_state(snap, &ext);
            Ok(())
        })?;
    } else {
        configure_structure_access_profile(ctx, session, target_id, corp_only, &service_taxes, &general_settings, &blocked, &allowed)?;
    }
    Ok(PyRep::Bool(true))
}

fn handle_create_owner_group(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let name = args.first().and_then(|a| a.as_str()).unwrap_or("Access Group").to_string();
    let description = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
    let corp_owned = args.get(2).map(|a| matches!(a, PyRep::Bool(true) | PyRep::Integer(1))).unwrap_or(true);
    ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if corp_owned { require_corp_profile_manager(snap, session, char_id, corp_id)?; }
        let mut ext = load_ext_state(snap);
        let group_id = ext.owner_groups.keys().copied().max().unwrap_or(80_000_000) + 1;
        ext.owner_groups.insert(group_id, AccessGroupRecord { group_id, creator_id: char_id, owner_corp_id: if corp_owned { corp_id } else { 0 }, name: name.clone(), description: description.clone(), members: HashMap::new() });
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([("groupID", PyRep::Integer(group_id)), ("name", PyRep::String(name)), ("description", PyRep::String(description)), ("creatorID", PyRep::Integer(char_id)), ("corpOwned", PyRep::Bool(corp_owned))]))
    })
}

fn apply_group_member_batch(ctx: &ServerContext, session: &SessionState, args: &[PyRep], is_add: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_or_struct_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingGroupId", "ownerGroupManager requires groupID"))?;
    let explicit_blocked = args.get(2).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None });
    let default_blocked = explicit_blocked.unwrap_or(false);
    let members: Vec<(i64, bool)> = match args.get(1) {
        Some(PyRep::Dict(d)) => d.iter().filter_map(|(k, v)| k.as_i64().map(|eid| (eid, explicit_blocked.unwrap_or_else(|| v.as_i64().is_some_and(|t| t < 0) || matches!(v, PyRep::Bool(false)))))).collect(),
        Some(PyRep::List(xs) | PyRep::Tuple(xs)) => xs.iter().filter_map(|x| x.as_i64().map(|eid| (eid, default_blocked))).collect(),
        Some(PyRep::Integer(eid)) => vec![(*eid, default_blocked)],
        _ => return Err(err("MissingEntityId", "ownerGroupManager requires memberIDs")),
    };
    let notify_corp = ctx.db.try_with_mut_snapshot(|snap| {
        let classified: Vec<(i64, bool, (bool, bool, bool))> = members
            .iter()
            .map(|&(eid, blk)| Ok((eid, blk, validate_and_classify_entity(snap, eid)?)))
            .collect::<Result<_, BridgeRpcError>>()?;
        let mut ext = load_ext_state(snap);
        if let Some(grp) = ext.owner_groups.get(&group_or_struct_id) {
            let is_admin_or_dir = grp.creator_id == char_id
                || grp.members.get(&char_id) == Some(&1)
                || (grp.owner_corp_id != 0 && require_corp_profile_manager(snap, session, char_id, grp.owner_corp_id).is_ok())
                || session.has_gm_role();
            let is_mgr = is_admin_or_dir || grp.members.get(&char_id) == Some(&2);
            if !is_mgr {
                return Err(err("InsufficientCorpRoles", "Caller is not a manager or admin of this access group"));
            }
            for &(eid, blk, _) in &classified {
                if eid == grp.creator_id && (!is_add || blk) && !session.has_gm_role() {
                    return Err(err("InsufficientCorpRoles", "Cannot remove or block access group creator"));
                }
                if !is_admin_or_dir && (eid == grp.creator_id || matches!(grp.members.get(&eid).copied(), Some(1 | 2))) {
                    return Err(err("InsufficientCorpRoles", "ACL Managers cannot overwrite or remove Admins or Managers"));
                }
            }
            let owner_corp = grp.owner_corp_id;
            if let Some(grp_mut) = ext.owner_groups.get_mut(&group_or_struct_id) {
                for &(eid, blk, _) in &classified {
                    if is_add { grp_mut.members.insert(eid, if blk { -1 } else { 0 }); }
                    else { grp_mut.members.remove(&eid); }
                }
            }
            let linked_pids: Vec<i64> = ext.profiles_by_id.values().filter(|p| p.access_group_ids.contains(&group_or_struct_id)).map(|p| p.profile_id).collect();
            for pid in linked_pids {
                let attached_gids: Vec<i64> = ext.profiles_by_id.get(&pid).map(|p| p.access_group_ids.iter().copied().collect()).unwrap_or_default();
                for &(eid, blk, (is_ally, is_corp, _)) in &classified {
                    let still_in_other = |want_blk: bool| {
                        attached_gids.iter().any(|&gid| {
                            gid != group_or_struct_id
                                && ext.owner_groups.get(&gid).and_then(|g| g.members.get(&eid)).is_some_and(|&mt| (mt < 0) == want_blk)
                        })
                    };
                    if is_add || !still_in_other(blk) {
                        if let Some(prof) = ext.profiles_by_id.get_mut(&pid) {
                            apply_entity_to_profile(prof, eid, is_ally, is_corp, blk, is_add);
                        }
                    }
                    if !is_add && explicit_blocked.is_none() && !still_in_other(!blk) {
                        if let Some(prof) = ext.profiles_by_id.get_mut(&pid) {
                            apply_entity_to_profile(prof, eid, is_ally, is_corp, !blk, false);
                        }
                    }
                }
                sync_assigned_structures_for_profile(&mut ext, pid);
            }
            save_ext_state(snap, &ext);
            Ok(owner_corp)
        } else {
            let st = require_structure_corp_manager(snap, session, char_id, group_or_struct_id)?;
            let prof = ext.access_profiles.entry(group_or_struct_id).or_default();
            for (eid, blk, (is_ally, is_corp, _)) in classified {
                apply_entity_to_profile(prof, eid, is_ally, is_corp, blk, is_add);
                if !is_add && explicit_blocked.is_none() {
                    apply_entity_to_profile(prof, eid, is_ally, is_corp, !blk, false);
                }
            }
            save_ext_state(snap, &ext);
            Ok(st.owner_corp_id)
        }
    })?;
    if notify_corp > 0 {
        ctx.notify(crate::router::NotificationTarget::Corporation(notify_corp), "OnOwnerGroupsUpdated", "corpid", vec![PyRep::Dict(vec![(PyRep::Integer(group_or_struct_id), PyRep::Integer(if is_add { 1 } else { 2 }))])]);
    }
    Ok(PyRep::List(Vec::new()))
}

fn handle_add_group_members(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    apply_group_member_batch(ctx, session, args, true)
}

fn handle_delete_group_members(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    apply_group_member_batch(ctx, session, args, false)
}

pub(crate) fn is_structure_service_online(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64, service_id: i64) -> bool {
    use super::structure_lifecycle as life;
    let online = life::online_service_type_ids(ctx, snap, structure_id);
    online.iter().any(|&tid| i64::from(tid) == service_id || service_id_for_module_type(tid) == Some(service_id))
}

/// DERIVED: default 100 bps (1.0% = 0.01) Upwell structure market service tax rate returned by `CharacterGetService`.
const DEFAULT_STRUCTURE_SERVICE_TAX_BPS: i64 = 100;

fn handle_character_get_services(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "CharacterGetServices requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if !life::is_operational(&st.state) || !evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
            return Ok(PyRep::Dict(Vec::new()));
        }
        let ext = load_ext_state(snap);
        let prof = ext.access_profiles.get(&structure_id);
        let mut entries = Vec::new();
        for tid in life::online_service_type_ids(ctx, snap, structure_id) {
            if let Some(svc_id) = service_id_for_module_type(tid) {
                if !entries.iter().any(|(k, _)| k == &PyRep::Integer(svc_id)) {
                    let bps = prof.and_then(|p| p.service_settings.get(&svc_id).copied()).unwrap_or(DEFAULT_STRUCTURE_SERVICE_TAX_BPS);
                    entries.push((PyRep::Integer(svc_id), PyRep::Real(bps as f64 / 10_000.0)));
                }
            }
        }
        Ok(PyRep::Dict(entries))
    })
}

fn parse_struct_svc_args(session: &SessionState, args: &[PyRep], m: &str) -> Result<(i64, i64), BridgeRpcError> {
    if args.len() >= 2 {
        Ok((arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", m))?, arg_i64(args, 1).ok_or_else(|| err("MissingServiceId", m))?))
    } else if args.len() == 1 {
        Ok((session.structure_id.ok_or_else(|| err("MissingStructureId", m))?, arg_i64(args, 0).ok_or_else(|| err("MissingServiceId", m))?))
    } else {
        Err(err("MissingArguments", m))
    }
}

fn handle_character_has_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, service_id) = parse_struct_svc_args(session, args, "CharacterHasService")?;
    ctx.db.with_snapshot(|snap| {
        let Some(st) = snap.structures.get(&structure_id) else { return Ok(PyRep::Bool(false)); };
        if !life::is_operational(&st.state) || !evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
            return Ok(PyRep::Bool(false));
        }
        Ok(PyRep::Bool(is_structure_service_online(ctx, snap, structure_id, service_id)))
    })
}

fn handle_character_check_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, service_id) = parse_struct_svc_args(session, args, "CharacterCheckService")?;
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if !life::is_operational(&st.state) {
            return Err(err("StructureServiceAccessDenied", "Structure is not operational"));
        }
        if !evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
            return Err(err("StructureServiceAccessDenied", "Character is excluded by structure access list"));
        }
        if !is_structure_service_online(ctx, snap, structure_id, service_id) {
            return Err(err("StructureServiceAccessDenied", "Service is offline"));
        }
        Ok(PyRep::None)
    })
}

fn handle_character_get_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, service_id) = parse_struct_svc_args(session, args, "CharacterGetService")?;
    ctx.db.with_snapshot(|snap| {
        let Some(st) = snap.structures.get(&structure_id) else { return Ok(PyRep::None); };
        if !life::is_operational(&st.state)
            || !evaluate_character_structure_access(ctx, snap, structure_id, char_id)
            || !is_structure_service_online(ctx, snap, structure_id, service_id)
        {
            return Ok(PyRep::None);
        }
        let ext = load_ext_state(snap);
        let bps = ext.access_profiles.get(&structure_id).and_then(|p| p.service_settings.get(&service_id).copied()).unwrap_or(DEFAULT_STRUCTURE_SERVICE_TAX_BPS);
        Ok(PyRep::Real(bps as f64 / 10_000.0))
    })
}

fn handle_character_has_setting(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, setting_id) = parse_struct_svc_args(session, args, "CharacterHasSetting")?;
    ctx.db.with_snapshot(|snap| {
        if !snap.structures.contains_key(&structure_id) || !evaluate_character_structure_access(ctx, snap, structure_id, char_id) { return Ok(PyRep::Bool(false)); }
        let mut ext = load_ext_state(snap);
        apply_pending_profile_applications(&mut ext, ctx.now_unix());
        if let Some(&granted) = ext.access_profiles.get(&structure_id).and_then(|p| p.general_settings.get(&setting_id)) { return Ok(PyRep::Bool(granted)); }
        if matches!(setting_id, 1 | 2 | 19) { return Ok(PyRep::Bool(true)); }
        Ok(PyRep::Bool(false))
    })
}
