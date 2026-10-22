//! `structure_upwell_ops` — Upwell structure unanchor/rename, vulnerability
//! schedule, asset safety, structure control, descriptions, deliveries, cargo drop-off, guests
//! and hangar view. Only RPC names present in the V24 client are dispatched here; anything
//! else falls through to `UnimplementedRPC`.
#![forbid(unsafe_code)]
use crate::router::ServerContext;
use eve_db::{InventoryFlag, UpwellStructureRecord, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const STRUCT_EXT_STATE_KEY: i64 = -7_700_001;
/// Asset safety: wraps become deliverable 5 days after creation (ledger acceptance text).
pub const ASSET_SAFETY_DELIVERY_DELAY_SECS: u64 = 5 * 86_400;
/// Asset safety: delivering a wrap costs 15% of its estimated value (ledger acceptance text).
const ASSET_SAFETY_FEE_FRACTION: f64 = 0.15;
/// Unanchoring timer (ledger acceptance text: 7-day timer).
const UNANCHOR_TIMER_SECS: u64 = 7 * 86_400;
/// Vulnerability schedule changes take effect after this delay.
const REINFORCE_CHANGE_DELAY_SECS: u64 = 86_400;
/// DERIVED: maximum distance (m) between ship and structure for cargo drop-off; mirrors the
/// 2,500 m docking perimeter used by `structureDocking.Dock` plus the structure radius.
const CARGO_DROPOFF_EXTRA_RANGE_M: f64 = 2_500.0;

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
    /// NPC station the wrap is auto-delivered to (set for wraps created by structure loss).
    #[serde(default)]
    pub nearest_npc_station_id: Option<i64>,
}

/// Per-structure lifecycle bookkeeping used by `structure_lifecycle`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureLifecycleState {
    /// Unix time at which the running repair timer completes (None = no repair pending).
    pub repair_until_unix: Option<u64>,
    /// Unix time at which the current vulnerable layer fight began (fixes reinforcement exit dates).
    pub fight_started_unix: Option<u64>,
    /// Optional reinforcement exit jitter offset in seconds (clamped to hull jitter window and UTC day).
    pub reinforce_jitter_secs: i64,
    /// Last hourly fuel tick boundary already charged.
    pub last_fuel_tick_unix: Option<u64>,
    /// Since when the operational structure has had no online service module.
    pub low_power_since_unix: Option<u64>,
    /// Service modules whose onlining fuel requirement has already been validated.
    pub online_modules: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StructureExtendedState {
    pub unanchoring_until_unix: HashMap<i64, u64>,
    pub descriptions: HashMap<i64, String>,
    pub active_pilot_by_structure: HashMap<i64, i64>,
    pub pending_reinforce_schedule: HashMap<i64, (u8, u8, u64)>,
    pub asset_safety_wraps: HashMap<i64, AssetSafetyWrapRecord>,
    pub lifecycle: HashMap<i64, StructureLifecycleState>,
    pub war_hq_structures: std::collections::HashSet<i64>,
    pub jump_bridge_links: HashMap<i64, i64>,
}

pub fn load_ext_state(snap: &WorldStateSnapshot) -> StructureExtendedState {
    snap.item_custom_names
        .get(&STRUCT_EXT_STATE_KEY)
        .and_then(|raw| serde_json::from_str::<StructureExtendedState>(raw).ok())
        .unwrap_or_default()
}

pub fn save_ext_state(snap: &mut WorldStateSnapshot, state: &StructureExtendedState) {
    if let Ok(json) = serde_json::to_string(state) {
        snap.item_custom_names.insert(STRUCT_EXT_STATE_KEY, json);
    }
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

fn arg_i64(args: &[PyRep], idx: usize) -> Option<i64> {
    args.get(idx).and_then(|a| a.as_i64())
}

/// Applies a pending vulnerability-schedule change once its change delay has elapsed.
pub fn apply_pending_reinforce_schedule(snap: &mut WorldStateSnapshot, ext: &mut StructureExtendedState, structure_id: i64, now_unix: u64) {
    if let Some(&(hour, day, apply_at)) = ext.pending_reinforce_schedule.get(&structure_id) {
        if now_unix >= apply_at {
            snap.structure_vulnerability_windows.insert(structure_id, (u64::from(hour), u64::from(day)));
            ext.pending_reinforce_schedule.remove(&structure_id);
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
            // const-ok: GM tool returns true on acknowledgement
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "structureAssetSafety.MoveEjectTimeGM requires GM role".into(),
                }));
            }
            Some(Ok(PyRep::Bool(true)))
        }
        ("structureControl", "TakeControl") => Some(handle_take_control(ctx, session, args)),
        ("structureControl", "ReleaseControl") => Some(handle_release_control(ctx, session, args)),
        ("structureControl", "GetStructurePilot") => Some(handle_get_structure_pilot(ctx, session, args)),
        ("structureControl", "BoardStructure") => Some(handle_board_structure(ctx, session, args)),
        ("structureControl", "EjectFromStructure") => Some(handle_eject_from_structure(ctx, session, args)),
        ("structureSettings", "CharacterHasService")
        | ("structureSettings", "CharacterCheckService")
        | ("structureSettings", "CharacterGetService")
        | ("structureSettings", "CharacterHasSetting") => Some(handle_character_has_service(ctx, session, args)),
        ("structureSettings", "CharacterGetServices") => Some(handle_character_get_services(ctx, session, args)),
        // const-ok: client-side memoized cache invalidation notifications return None
        ("structureSettings", "clear_memoized") | ("structureDirectory", "clear_memoized") => Some(Ok(PyRep::None)),
        // parity: feat:structures.structure-description
        ("structureDirectory", "SetStructureDescription") => Some(handle_set_structure_description(ctx, session, args)),
        ("structureDirectory", "GetStructureDescription") => Some(handle_get_structure_description(ctx, session, args)),
        ("structureDirectory", "GetStructures") | ("structureDirectory", "GetCorporationStructures") => Some(handle_get_structures(ctx, session)),
        ("structureDirectory", "GetStructureInfo") => Some(handle_get_structure_info(ctx, session, args)),
        // parity: feat:structures.deliveries
        ("structureDeliveries", "Deliver") => Some(handle_structure_deliver(ctx, session, args)),
        // parity: feat:structures.cargo-dropoff
        ("structureCargoDelivery", "DropOffItems") => Some(handle_cargo_dropoff(ctx, session, args).await),
        // parity: feat:structures.guests
        ("structureGuests", "GetGuests") => Some(handle_structure_get_guests(ctx, session, args)),
        // parity: feat:structures.hangar-view
        ("structureHangarViewMgr", "GetMyHangarViewState") => Some(handle_get_hangar_view_state(ctx, session)),
        ("jumpCloneSvc", "GetNumClonesInPilotsStructure") => Some(handle_get_num_clones_in_structure(ctx, session, args)),
        // parity: feat:navigation.ansiblex-jump
        ("structureJumpBridgeMgr", "CmdJumpThroughStructureStargate") => {
            Some(handle_cmd_jump_through_structure_stargate(ctx, session, args).await)
        }
        // parity: feat:navigation.ansiblex-link
        ("structureJumpBridgeMgr", "GetLinkedStructure") => {
            Some(handle_get_linked_structure(ctx, session, args))
        }
        ("structureJumpBridgeMgr", "GetJbStructureDestination") => {
            Some(handle_get_jb_structure_destination(ctx, session, args))
        }
        ("structureDirectory", "GetJumpBridgesWithMyAccess")
        | ("structureDirectory", "GetNearbyJumpBridges") => {
            Some(handle_get_jump_bridges_with_my_access(ctx, session))
        }
        _ => None,
    }
}

fn require_structure_corp_member(snap: &WorldStateSnapshot, char_id: i64, structure_id: i64) -> Result<UpwellStructureRecord, BridgeRpcError> {
    let Some(st) = snap.structures.get(&structure_id).cloned() else {
        return Err(err("StructureNotFound", format!("Structure {structure_id} does not exist")));
    };
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
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        if st.state == "ArmorReinforced" || st.state == "HullReinforced" || now_unix < st.reinforced_until_unix {
            return Err(err("CannotUnanchorReinforcedStructure", format!("Structure {structure_id} cannot be unanchored while reinforced")));
        }
        let mut ext = load_ext_state(snap);
        if ext.unanchoring_until_unix.contains_key(&structure_id) {
            return Err(err("StructureAlreadyUnanchoring", format!("Structure {structure_id} is already unanchoring")));
        }
        let until = now_unix + UNANCHOR_TIMER_SECS;
        ext.unanchoring_until_unix.insert(structure_id, until);
        save_ext_state(snap, &ext);
        let mut moved = Vec::new();
        for ch in snap.characters.values_mut() {
            if ch.home_station_id == structure_id {
                ch.home_station_id = 60_003_760;
                moved.push(ch.character_id);
            }
        }
        Ok((st, until, moved))
    })?;
    ctx.notify(
        crate::router::NotificationTarget::Corporation(st.owner_corp_id),
        "OnCorporationStructuresUpdated",
        "corpid",
        vec![],
    );
    let state_args = vec![
        PyRep::Integer(i64::from(st.solar_system_id)),
        PyRep::Integer(structure_id),
        PyRep::String("Unanchoring".into()),
    ];
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(st.solar_system_id),
        "OnStructureStateChanged",
        "solarsystemid",
        state_args.clone(),
    );
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(st.solar_system_id),
        "OnCurrentStructureStateChanged",
        "solarsystemid",
        state_args,
    );
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(st.solar_system_id),
        "OnStructureRemoved",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(st.solar_system_id)), PyRep::Integer(structure_id)],
    );
    ctx.notify(
        crate::router::NotificationTarget::Corporation(st.owner_corp_id),
        "OnStructureRemovedFromSpace",
        "corpid",
        vec![PyRep::Integer(structure_id)],
    );
    if home_moved_chars.is_empty() {
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnHomeStationMovedFromStructure",
            "charid",
            vec![],
        );
    } else {
        for cid in home_moved_chars {
            ctx.notify(
                crate::router::NotificationTarget::Character(cid),
                "OnHomeStationMovedFromStructure",
                "charid",
                vec![],
            );
        }
    }
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("state", PyRep::String("Unanchoring".into())),
        ("unanchorCompleteUnix", PyRep::Integer(until as i64)),
    ]))
}

fn handle_structure_cancel_unanchor(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "CancelUnanchor requires structureID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        if ext.unanchoring_until_unix.remove(&structure_id).is_none() {
            return Err(err("StructureNotUnanchoring", format!("Structure {structure_id} does not have an active unanchoring timer")));
        }
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(structure_id)),
            ("state", PyRep::String(st.state)),
            ("unanchoringCancelled", PyRep::Bool(true)),
        ]))
    })
}

fn handle_structure_rename(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "RenameStructure requires structureID"))?;
    let new_name = args
        .get(1)
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= 100)
        .ok_or_else(|| err("InvalidStructureName", "Structure name must be 1..=100 non-whitespace characters"))?
        .to_string();
    let st = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        if let Some(st_mut) = snap.structures.get_mut(&structure_id) {
            st_mut.name = new_name.clone();
        }
        snap.item_custom_names.insert(structure_id, new_name.clone());
        Ok(st)
    })?;
    ctx.notify(
        crate::router::NotificationTarget::Corporation(st.owner_corp_id),
        "OnCorporationStructuresUpdated",
        "corpid",
        vec![],
    );
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(st.solar_system_id),
        "OnSlimItemChange",
        "solarsystemid",
        vec![
            PyRep::Integer(structure_id),
            PyRep::keyval([
                ("itemID", PyRep::Integer(structure_id)),
                ("typeID", PyRep::Integer(i64::from(st.type_id))),
                ("ownerID", PyRep::Integer(st.owner_corp_id)),
                ("name", PyRep::String(new_name.clone())),
            ]),
        ],
    );
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
    let (st, cur_hour, cur_day, apply_after) = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        apply_pending_reinforce_schedule(snap, &mut ext, structure_id, now_unix);
        let (ch, cd) = snap.structure_vulnerability_windows.get(&structure_id).copied().unwrap_or((18, 5));
        let apply_after = now_unix + REINFORCE_CHANGE_DELAY_SECS;
        ext.pending_reinforce_schedule.insert(structure_id, (hour as u8, weekday as u8, apply_after));
        save_ext_state(snap, &ext);
        Ok((st, ch as i64, cd as i64, apply_after))
    })?;
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(st.solar_system_id),
        "OnStructureHacked",
        "solarsystemid",
        vec![
            PyRep::Integer(structure_id),
            PyRep::Tuple(vec![
                PyRep::Integer(cur_day),
                PyRep::Integer(cur_hour),
                PyRep::Integer(weekday),
                PyRep::Integer(hour),
                PyRep::Integer(apply_after as i64),
            ]),
        ],
    );
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("nextReinforceHour", PyRep::Integer(hour)),
        ("nextReinforceWeekday", PyRep::Integer(weekday)),
        ("applyAfterUnix", PyRep::Integer(apply_after as i64)),
    ]))
}

fn handle_move_assets_to_safety(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep], is_corp: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 1).ok_or_else(|| err("MissingStructureId", "MoveAssetsToSafety requires (solarSystemID, structureID)"))?;
    let now_unix = ctx.now_unix();
    let active_ship = session.ship_id.unwrap_or(0);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(st) = snap.structures.get(&structure_id).cloned() else {
            return Err(err("StructureNotFound", format!("Structure {structure_id} not found")));
        };
        let owner_id = if is_corp { snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0) } else { char_id };
        let mut item_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.owner_id == owner_id && i.location_id == structure_id && i.item_id != active_ship)
            .map(|i| i.item_id)
            .collect();
        item_ids.sort_unstable();
        if item_ids.is_empty() {
            return Err(err("NoAssetsToMoveToSafety", format!("Owner {owner_id} has no eligible items in structure {structure_id}")));
        }
        snap.next_entity_id += 1;
        let wrap_id = snap.next_entity_id;
        let mut est_val = 0.0;
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                let base = ctx.sde.types.get(&it.type_id).map(|t| t.base_price).unwrap_or(0.0);
                est_val += base * f64::from(it.quantity.max(1));
                it.location_id = wrap_id;
                it.flag = InventoryFlag::Locked;
            }
        }
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            wrap_id,
            AssetSafetyWrapRecord {
                wrap_id,
                owner_id,
                is_corp,
                source_structure_id: structure_id,
                source_structure_name: st.name,
                solar_system_id: st.solar_system_id,
                created_unix: now_unix,
                deliverable_after_unix: now_unix + ASSET_SAFETY_DELIVERY_DELAY_SECS,
                delivered_to_id: None,
                item_ids: item_ids.clone(),
                estimated_value_isk: est_val,
                nearest_npc_station_id: None,
            },
        );
        save_ext_state(snap, &ext);
        Ok((wrap_id, owner_id, st.solar_system_id, item_ids.len()))
    })?;
    let (wrap_id, owner_id, sys_id, count) = res;
    let as_args = vec![
        PyRep::Integer(owner_id),
        PyRep::Integer(i64::from(sys_id)),
        PyRep::Integer(structure_id),
    ];
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAssetSafetyCreated",
        "charid",
        as_args.clone(),
    );
    if is_corp {
        ctx.notify(
            crate::router::NotificationTarget::Corporation(owner_id),
            "OnAssetSafetyCreated",
            "corpid",
            as_args,
        );
    }
    Ok(PyRep::keyval([
        ("wrapID", PyRep::Integer(wrap_id)),
        ("ownerID", PyRep::Integer(owner_id)),
        ("itemCount", PyRep::Integer(count as i64)),
        ("deliverableAfterUnix", PyRep::Integer((now_unix + ASSET_SAFETY_DELIVERY_DELAY_SECS) as i64)),
    ]))
}

fn handle_get_items_in_safety(ctx: &ServerContext, session: &mut SessionState, is_corp: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.with_snapshot(|snap| {
        let owner_id = if is_corp { snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0) } else { char_id };
        let ext = load_ext_state(snap);
        let mut wraps: Vec<&AssetSafetyWrapRecord> = ext
            .asset_safety_wraps
            .values()
            .filter(|w| w.owner_id == owner_id && w.is_corp == is_corp && w.delivered_to_id.is_none())
            .collect();
        wraps.sort_by_key(|w| w.wrap_id);
        let rows = wraps
            .into_iter()
            .map(|w| {
                PyRep::keyval([
                    ("assetWrapID", PyRep::Integer(w.wrap_id)),
                    ("ownerID", PyRep::Integer(w.owner_id)),
                    ("solarSystemID", PyRep::Integer(i64::from(w.solar_system_id))),
                    ("deliverableAfterUnix", PyRep::Integer(w.deliverable_after_unix as i64)),
                    ("itemCount", PyRep::Integer(w.item_ids.len() as i64)),
                ])
            })
            .collect();
        Ok(PyRep::List(rows))
    })
}

fn handle_get_wrap_names(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let entries = ids
            .into_iter()
            .filter_map(|wid| ext.asset_safety_wraps.get(&wid).map(|w| (PyRep::Integer(wid), PyRep::String(w.source_structure_name.clone()))))
            .collect();
        Ok(PyRep::Dict(entries))
    })
}

/// `GetStructuresICanDeliverTo(solarSystemID)`: live structures and NPC stations in that system.
fn handle_get_structures_i_can_deliver_to(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let sys_id = arg_i64(args, 0).and_then(|v| u32::try_from(v).ok()).or(session.solar_system_id).ok_or_else(|| err("MissingSolarSystemId", "GetStructuresICanDeliverTo requires solarSystemID"))?;
    let mut rows: Vec<(i64, u32)> = ctx.db.with_snapshot(|snap| snap.structures.values().filter(|s| s.solar_system_id == sys_id && s.state != "Destroyed").map(|s| (s.structure_id, s.type_id)).collect());
    rows.extend(ctx.sde.stations.values().filter(|s| s.solar_system_id == sys_id).map(|s| (s.station_id, s.station_type_id)));
    rows.sort_unstable();
    Ok(PyRep::List(
        rows.into_iter()
            .map(|(id, type_id)| PyRep::keyval([("itemID", PyRep::Integer(id)), ("typeID", PyRep::Integer(i64::from(type_id))), ("solarSystemID", PyRep::Integer(i64::from(sys_id)))]))
            .collect(),
    ))
}

fn handle_move_safety_wrap_to_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let wrap_id = arg_i64(args, 0).ok_or_else(|| err("MissingWrapId", "MoveSafetyWrapToStructure requires wrapID"))?;
    let dest_id = arg_i64(args, 1).ok_or_else(|| err("MissingDestinationId", "MoveSafetyWrapToStructure requires destination ID"))?;
    let dest_is_station = ctx.sde.stations.contains_key(&dest_id);
    let now_unix = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        if !dest_is_station && !snap.structures.get(&dest_id).map(|s| s.state != "Destroyed").unwrap_or(false) {
            return Err(err("InvalidAssetSafetyDestination", format!("Destination {dest_id} is not a station or live structure")));
        }
        let mut ext = load_ext_state(snap);
        let Some(wrap) = ext.asset_safety_wraps.get(&wrap_id).cloned() else {
            return Err(err("AssetSafetyWrapNotFound", format!("Asset safety wrap {wrap_id} not found")));
        };
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        if (wrap.is_corp && wrap.owner_id != caller_corp) || (!wrap.is_corp && wrap.owner_id != char_id) {
            return Err(err("NotAssetSafetyWrapOwner", format!("Wrap {wrap_id} is not owned by the caller")));
        }
        if wrap.delivered_to_id.is_some() {
            return Err(err("AssetSafetyWrapAlreadyDelivered", format!("Asset safety wrap {wrap_id} has already been delivered")));
        }
        if now_unix < wrap.deliverable_after_unix {
            return Err(err("AssetSafetyCooldownActive", format!("Wrap {wrap_id} deliverable after {} (now {now_unix})", wrap.deliverable_after_unix)));
        }
        let fee = eve_db::IskCents::from_f64_isk(wrap.estimated_value_isk * ASSET_SAFETY_FEE_FRACTION);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        if ch.balance < fee {
            return Err(err("InsufficientFunds", format!("Delivering wrap {wrap_id} requires a 15% fee")));
        }
        ch.balance = ch.balance.checked_sub(fee).unwrap_or(ch.balance);
        for &iid in &wrap.item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = dest_id;
                it.flag = InventoryFlag::Deliveries;
            }
        }
        if let Some(w) = ext.asset_safety_wraps.get_mut(&wrap_id) {
            w.delivered_to_id = Some(dest_id);
        }
        save_ext_state(snap, &ext);
        Ok((
            wrap.owner_id,
            wrap.is_corp,
            PyRep::keyval([
                ("wrapID", PyRep::Integer(wrap_id)),
                ("destinationID", PyRep::Integer(dest_id)),
                ("deliveredItemCount", PyRep::Integer(wrap.item_ids.len() as i64)),
                ("feePaidISK", PyRep::Real(fee.to_wire_f64())),
            ]),
        ))
    })?;
    let (owner_id, is_corp, resp) = res;
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAssetSafetyDelivered",
        "charid",
        vec![PyRep::Integer(owner_id)],
    );
    if is_corp {
        ctx.notify(
            crate::router::NotificationTarget::Corporation(owner_id),
            "OnAssetSafetyDelivered",
            "corpid",
            vec![PyRep::Integer(owner_id)],
        );
    }
    Ok(resp)
}

fn require_structure_control_role(snap: &WorldStateSnapshot, char_id: i64, st: &UpwellStructureRecord) -> Result<(), BridgeRpcError> {
    if let Some(corp) = snap.corporations.get(&st.owner_corp_id) {
        let member_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        if corp.ceo_id != 0 && char_id != corp.ceo_id && member_role == 0 {
            return Err(err("CorpRoleDenied", format!("Character {char_id} lacks a corporation role required to control structure {}", st.structure_id)));
        }
    }
    Ok(())
}

fn handle_take_control(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "TakeControl requires structureID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        require_structure_control_role(snap, char_id, &st)?;
        let mut ext = load_ext_state(snap);
        if let Some(&pilot) = ext.active_pilot_by_structure.get(&structure_id) {
            if pilot != char_id {
                return Err(err("StructureAlreadyControlled", format!("Structure {structure_id} is already controlled by pilot {pilot}")));
            }
        }
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("pilotID", PyRep::Integer(char_id))]))
    })
}

fn handle_board_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "BoardStructure requires structureID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        require_structure_control_role(snap, char_id, &st)?;
        let mut ext = load_ext_state(snap);
        if let Some(&pilot) = ext.active_pilot_by_structure.get(&structure_id) {
            if pilot != char_id {
                return Err(err("StructureAlreadyControlled", format!("Structure {structure_id} is already controlled by pilot {pilot}")));
            }
        }
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(structure_id)),
            ("pilotID", PyRep::Integer(char_id)),
            ("boarded", PyRep::Bool(true)),
        ]))
    })
}

fn handle_eject_from_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "EjectFromStructure requires structureID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err("NotControllingStructure", format!("Character {char_id} is not boarding/controlling structure {structure_id}")));
        }
        ext.active_pilot_by_structure.remove(&structure_id);
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(structure_id)),
            ("pilotID", PyRep::None),
            ("boarded", PyRep::Bool(false)),
        ]))
    })
}

fn handle_release_control(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "ReleaseControl requires structureID"))?;
    let target_id = arg_i64(args, 1).unwrap_or(0);
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err("NotControllingStructure", format!("Character {char_id} does not control structure {structure_id}")));
        }
        ext.active_pilot_by_structure.remove(&structure_id);
        save_ext_state(snap, &ext);
        Ok(())
    })?;
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnStructureTargetRemoved",
        "charid",
        vec![PyRep::Integer(structure_id), PyRep::Integer(target_id)],
    );
    Ok(PyRep::None)
}

fn handle_get_structure_pilot(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "GetStructurePilot requires structureID"))?;
    Ok(ctx.db.with_snapshot(|snap| load_ext_state(snap).active_pilot_by_structure.get(&structure_id).copied().map(PyRep::Integer).unwrap_or(PyRep::None)))
}

fn handle_set_structure_description(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "SetStructureDescription requires structureID"))?;
    let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
    let st = ctx.db.try_with_mut_snapshot(|snap| {
        let st = require_structure_corp_member(snap, char_id, structure_id)?;
        let mut ext = load_ext_state(snap);
        ext.descriptions.insert(structure_id, desc);
        save_ext_state(snap, &ext);
        Ok(st)
    })?;
    ctx.notify(
        crate::router::NotificationTarget::Corporation(st.owner_corp_id),
        "OnProfileSettingsChange",
        "corpid",
        vec![PyRep::List(vec![PyRep::Integer(structure_id)])],
    );
    ctx.notify(
        crate::router::NotificationTarget::Corporation(st.owner_corp_id),
        "OnOwnerGroupsUpdated",
        "corpid",
        vec![PyRep::Dict(vec![(PyRep::Integer(st.owner_corp_id), PyRep::Integer(structure_id))])],
    );
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(st.solar_system_id),
        "OnDockingAccessChangedForCurrentSolarSystem",
        "solarsystemid",
        vec![],
    );
    Ok(PyRep::None)
}

fn handle_get_structure_description(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "GetStructureDescription requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        if !snap.structures.contains_key(&structure_id) {
            return Err(err("StructureNotFound", format!("Structure {structure_id} does not exist")));
        }
        Ok(load_ext_state(snap).descriptions.get(&structure_id).cloned().map(PyRep::String).unwrap_or(PyRep::None))
    })
}

fn handle_structure_deliver(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "structureDeliveries.Deliver requires structureID"))?;
    let item_id = arg_i64(args, 1).ok_or_else(|| err("MissingItemId", "structureDeliveries.Deliver requires itemID"))?;
    let recipient_id = arg_i64(args, 2).ok_or_else(|| err("MissingRecipientId", "structureDeliveries.Deliver requires recipient characterID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&recipient_id) {
            return Err(err("RecipientNotFound", format!("Recipient character {recipient_id} not found")));
        }
        let Some(it) = snap.items.get_mut(&item_id) else {
            return Err(err("ItemNotFound", format!("Item {item_id} not found")));
        };
        if it.owner_id != char_id || it.location_id != structure_id {
            return Err(err("ItemNotInStructureHangar", format!("Item {item_id} must be owned by {char_id} inside structure {structure_id}")));
        }
        it.owner_id = recipient_id;
        it.flag = InventoryFlag::Deliveries;
        it.version += 1;
        Ok(PyRep::None)
    })
}

async fn handle_cargo_dropoff(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("MustBeInSpace", "structureCargoDelivery.DropOffItems requires a ship in space"));
    }
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "No active ship in space"))?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "DropOffItems requires structureID"))?;
    let item_ids: Vec<i64> = match args.get(1) {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        return Err(err("NoItemsSpecified", "DropOffItems requires at least one itemID"));
    }
    let sys_id = session.solar_system_id.unwrap_or(0);
    let (st_pos, st_type) = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&structure_id).filter(|s| s.solar_system_id == sys_id && s.state != "Destroyed").map(|s| (DVec3::from_array(s.position), s.type_id)))
        .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not in solar system {sys_id}")))?;
    let st_radius = ctx.sde.types.get(&st_type).map(|t| t.radius).unwrap_or(0.0);
    let (_, ship_ball) = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| err("DestinyQueryFailed", e))?
        .ok_or_else(|| err("ShipNotInSpace", "Active ship not in space"))?;
    if ship_ball.position.distance(st_pos) > st_radius + CARGO_DROPOFF_EXTRA_RANGE_M {
        return Err(err("OutsideCargoDropoffRange", "Ship is too far from the structure to drop off cargo"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        for &iid in &item_ids {
            let ok = snap.items.get(&iid).map(|it| it.owner_id == char_id && it.location_id == ship_id).unwrap_or(false);
            if !ok {
                return Err(err("ItemNotInShipCargo", format!("Item {iid} is not in active ship {ship_id} cargo")));
            }
        }
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = structure_id;
                it.flag = InventoryFlag::Deliveries;
                it.version += 1;
            }
        }
        Ok(PyRep::None)
    })
}

fn handle_structure_get_guests(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "structureGuests.GetGuests requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        let mut guests: Vec<(i64, i64, Option<i64>)> =
            snap.characters.values().filter(|c| c.structure_id == Some(structure_id)).map(|c| (c.character_id, c.corporation_id, c.alliance_id)).collect();
        guests.sort_by_key(|g| g.0);
        let rows = guests
            .into_iter()
            .map(|(cid, corp_id, ally_id)| PyRep::Tuple(vec![PyRep::Integer(cid), PyRep::Integer(corp_id), ally_id.map(PyRep::Integer).unwrap_or(PyRep::None)]))
            .collect();
        Ok(PyRep::List(rows))
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
            ("characterID", PyRep::Integer(char_id)),
            ("structureID", PyRep::Integer(structure_id)),
            ("structureTypeID", PyRep::Integer(i64::from(st.type_id))),
            ("shipID", ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("shipTypeID", ship_type),
        ]))
    })
}

fn handle_get_num_clones_in_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "GetNumClonesInPilotsStructure requires structureID"))?;
    let count = ctx.db.with_snapshot(|snap| snap.jump_clones.values().map(|clones| clones.iter().filter(|jc| jc.location_id == structure_id).count()).sum::<usize>());
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

/// Checks whether a structure allows market order placement (`feat:structures.service-market`):
/// Medium/Flex structures can never host a market hub; Large/XL structures that have any fitted
/// service module or are lifecycle-managed must have an online `Standup Market Hub I` (`35892`).
pub fn structure_allows_market_orders(ctx: &ServerContext, snap: &WorldStateSnapshot, structure_id: i64) -> bool {
    use super::structure_lifecycle as life;
    let Some(st) = snap.structures.get(&structure_id) else {
        return false;
    };
    if !life::is_operational(&st.state) {
        return false;
    }
    let Some(profile) = life::structure_profile(ctx, st.type_id) else {
        return false;
    };
    if matches!(profile.hull_class, life::HullClass::Medium | life::HullClass::Flex) {
        return false;
    }
    let online_types = life::online_service_type_ids(ctx, snap, structure_id);
    if online_types.contains(&35892) {
        return true;
    }
    let has_any_service_item = snap.items.values().any(|i| i.location_id == structure_id && (164..=171).contains(&(i.flag as u16)));
    let is_lifecycle_tracked = load_ext_state(snap).lifecycle.contains_key(&structure_id);
    !(has_any_service_item || is_lifecycle_tracked)
}

fn handle_character_get_services(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "CharacterGetServices requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if !life::is_operational(&st.state) {
            return Ok(PyRep::Dict(Vec::new()));
        }
        let mut entries = Vec::new();
        for tid in life::online_service_type_ids(ctx, snap, structure_id) {
            if let Some(svc_id) = service_id_for_module_type(tid) {
                entries.push((PyRep::Integer(svc_id), PyRep::Integer(1)));
            }
            entries.push((PyRep::Integer(i64::from(tid)), PyRep::Integer(1)));
        }
        Ok(PyRep::Dict(entries))
    })
}

fn handle_character_has_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "CharacterHasService requires structureID"))?;
    let service_id = arg_i64(args, 1).ok_or_else(|| err("MissingServiceId", "CharacterHasService requires serviceID"))?;
    ctx.db.with_snapshot(|snap| {
        let Some(st) = snap.structures.get(&structure_id) else {
            return Ok(PyRep::Bool(false));
        };
        if !life::is_operational(&st.state) {
            return Ok(PyRep::Bool(false));
        }
        let online = life::online_service_type_ids(ctx, snap, structure_id);
        let has = online.iter().any(|&tid| i64::from(tid) == service_id || service_id_for_module_type(tid) == Some(service_id));
        Ok(PyRep::Bool(has))
    })
}

fn build_structure_dir_entry(ctx: &ServerContext, snap: &WorldStateSnapshot, ext: &StructureExtendedState, st: &UpwellStructureRecord, now: u64) -> PyRep {
    use super::structure_lifecycle as life;
    let online_types = life::online_service_type_ids(ctx, snap, st.structure_id);
    let mut services = Vec::new();
    for &tid in &online_types {
        if let Some(svc_id) = service_id_for_module_type(tid) {
            services.push((PyRep::Integer(svc_id), PyRep::Integer(1)));
        }
    }
    let power = life::power_state(ctx, snap, ext, st, now);
    PyRep::keyval([
        ("structureID", PyRep::Integer(st.structure_id)),
        ("typeID", PyRep::Integer(i64::from(st.type_id))),
        ("ownerID", PyRep::Integer(st.owner_corp_id)),
        ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
        ("itemName", PyRep::String(st.name.clone())),
        ("state", PyRep::String(st.state.clone())),
        ("powerState", PyRep::String(power.as_str().into())),
        ("fuelBlocks", PyRep::Integer(i64::from(st.fuel_blocks))),
        ("services", PyRep::Dict(services)),
        ("onlineServiceTypeIDs", PyRep::List(online_types.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
        ("inSpace", PyRep::Bool(st.state != life::STATE_DESTROYED)),
    ])
}

fn handle_get_structures(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let _ = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let mut list: Vec<&UpwellStructureRecord> = snap.structures.values().filter(|s| s.state != life::STATE_DESTROYED).collect();
        list.sort_by_key(|s| s.structure_id);
        let entries = list
            .into_iter()
            .map(|st| (PyRep::Integer(st.structure_id), build_structure_dir_entry(ctx, snap, &ext, st, now)))
            .collect();
        Ok(PyRep::Dict(entries))
    })
}

fn handle_get_structure_info(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use super::structure_lifecycle as life;
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "GetStructureInfo requires structureID"))?;
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).filter(|s| s.state != life::STATE_DESTROYED).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        let ext = load_ext_state(snap);
        Ok(build_structure_dir_entry(ctx, snap, &ext, st, now))
    })
}

pub const ANSIBLEX_TYPE_ID: u32 = 35841;
pub const LIQUID_OZONE_TYPE_ID: u32 = 16273;
pub const ANSIBLEX_JUMP_OZONE_COST: i32 = 500;

fn resolve_linked_structure_id(snap: &WorldStateSnapshot, ext: &StructureExtendedState, structure_id: i64) -> Option<i64> {
    if let Some(&linked) = ext.jump_bridge_links.get(&structure_id) {
        if snap.structures.contains_key(&linked) {
            return Some(linked);
        }
    }
    let ansiblex: Vec<i64> = snap
        .structures
        .values()
        .filter(|s| (s.type_id == ANSIBLEX_TYPE_ID || s.name.contains("Ansiblex")) && s.state != "Destroyed")
        .map(|s| s.structure_id)
        .collect();
    if ansiblex.len() == 2 && ansiblex.contains(&structure_id) {
        return ansiblex.into_iter().find(|&id| id != structure_id);
    }
    None
}

fn handle_get_linked_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "GetLinkedStructure requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let linked = resolve_linked_structure_id(snap, &ext, structure_id)
            .ok_or_else(|| err("NoLinkedStructure", format!("Structure {structure_id} is not linked to any destination gate")))?;
        Ok(PyRep::Integer(linked))
    })
}

fn handle_get_jb_structure_destination(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "GetJbStructureDestination requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let linked_id = resolve_linked_structure_id(snap, &ext, structure_id)
            .ok_or_else(|| err("NoLinkedStructure", format!("Structure {structure_id} is not linked to any destination gate")))?;
        let dest_sys = snap.structures.get(&linked_id)
            .map(|s| s.solar_system_id)
            .ok_or_else(|| err("LinkedStructureNotFound", format!("Destination structure {linked_id} not found")))?;
        Ok(PyRep::Integer(i64::from(dest_sys)))
    })
}

fn handle_get_jump_bridges_with_my_access(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let corp_id = session.corporation_id.unwrap_or_else(|| {
        ctx.db.get_character(char_id).map(|c| c.corporation_id).unwrap_or(0)
    });
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let mut results = Vec::new();
        for st in snap.structures.values() {
            if (st.type_id == ANSIBLEX_TYPE_ID || st.name.contains("Ansiblex")) && st.state != "Destroyed" {
                if crate::services::space_destiny::space_nav_travel::structure_access_allowed(ctx, char_id, corp_id, st) {
                    if let Some(linked_id) = resolve_linked_structure_id(snap, &ext, st.structure_id) {
                        let dest_sys = snap.structures.get(&linked_id).map(|s| s.solar_system_id).unwrap_or(0);
                        results.push(PyRep::keyval([
                            ("structureID", PyRep::Integer(st.structure_id)),
                            ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                            ("linkedStructureID", PyRep::Integer(linked_id)),
                            ("destinationSolarSystemID", PyRep::Integer(i64::from(dest_sys))),
                        ]));
                    }
                }
            }
        }
        Ok(PyRep::List(results))
    })
}

async fn handle_cmd_jump_through_structure_stargate(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("ShipDocked", "Cannot jump through structure stargate while docked"));
    }
    let structure_id = arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", "CmdJumpThroughStructureStargate requires structureID"))?;
    let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no solarSystemID"))?;
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Session has no active ship"))?;

    let now_unix = ctx.now_unix();
    if ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&char_id)
        .is_some_and(|st| st.is_weapon_timer_active(now_unix))
    {
        return Err(err("WeaponsTimerActive", "Cannot jump through structure stargate while weapons timer is active"));
    }

    let corp_id = session.corporation_id.unwrap_or_else(|| {
        ctx.db.get_character(char_id).map(|c| c.corporation_id).unwrap_or(0)
    });

    // Pre-validate structure, access, destination, and ship class before checking Ballpark proximity
    let (st_pos, st_radius, linked_id, dest_sys_id) = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if st.solar_system_id != sys_id || st.state == "Destroyed" {
            return Err(err("StructureNotOperational", "Structure stargate is not active in this solar system"));
        }
        if !crate::services::space_destiny::space_nav_travel::structure_access_allowed(ctx, char_id, corp_id, &st) {
            return Err(err("AccessDenied", "Structure access list denies jump bridge usage"));
        }
        let ext = load_ext_state(snap);
        let linked = resolve_linked_structure_id(snap, &ext, structure_id)
            .ok_or_else(|| err("NoLinkedStructure", "Ansiblex has no linked destination stargate"))?;
        let dest_st = snap.structures.get(&linked).cloned()
            .ok_or_else(|| err("DestinationStructureNotFound", "Linked destination structure not found"))?;
        if dest_st.state == "Destroyed" {
            return Err(err("DestinationStructureDestroyed", "Linked destination stargate is destroyed"));
        }
        if let Some(ship_item) = snap.items.get(&ship_id) {
            let ship_group = ctx.sde.types.get(&ship_item.type_id).map(|t| t.group_id).unwrap_or(0);
            // Titans (30) and Supercarriers (659) are barred from using Ansiblex jump gates
            if matches!(ship_group, 30 | 659) {
                return Err(err("ShipNotAllowedThroughAnsiblex", "Supercarriers and Titans cannot jump through an Ansiblex jump gate"));
            }
        }
        let rad = ctx.sde.types.get(&st.type_id).map(|t| t.radius).unwrap_or(500.0);
        Ok((glam::DVec3::from_array(st.position), rad, linked, dest_st.solar_system_id))
    })?;

    // Check ship existence and proximity in space BEFORE mutating Liquid Ozone fuel (`SF-02`)
    let old_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((cur_tick, ship_ball)) = old_handle.get_ball_snapshot(ship_id).await.map_err(|e| err("DestinyQueryFailed", e))? else {
        return Err(err("ShipNotInSpace", "Ship is not in space"));
    };
    if ship_ball.position.distance(st_pos) > st_radius + 5_000.0 {
        return Err(err("OutsideJumpBridgeRange", format!("Ship is {:.1} m from gate (must be within {:.1} m)", ship_ball.position.distance(st_pos), st_radius + 5000.0)));
    }
    if let Some(active) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        Some((sys_id, cur_tick)),
    ) {
        return Err(err(
            "SessionChangeInProgress",
            format!("Cannot jump through structure stargate while 10s timer from '{}' is active", active.reason),
        ));
    }

    // Transactionally consume Liquid Ozone after all preconditions pass
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ozone_found = false;
        for item in snap.items.values_mut() {
            if item.location_id == structure_id && item.type_id == LIQUID_OZONE_TYPE_ID && item.quantity >= ANSIBLEX_JUMP_OZONE_COST {
                item.quantity -= ANSIBLEX_JUMP_OZONE_COST;
                item.version += 1;
                ozone_found = true;
                break;
            }
        }
        if !ozone_found {
            return Err(err("InsufficientLiquidOzone", format!("Ansiblex gate requires at least {ANSIBLEX_JUMP_OZONE_COST} units of Liquid Ozone fuel")));
        }
        Ok(())
    })?;

    // Move character and ship to destination
    let updated_ch = ctx.db.set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
        .map_err(|e| err("SystemTransitionFailed", e.to_string()))?;
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(sys_id), dest_sys_id);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(ship_ball);
    let dest_st_pos = ctx.db.with_snapshot(|snap| {
        snap.structures.get(&linked_id).map(|s| glam::DVec3::from_array(s.position)).unwrap_or(glam::DVec3::ZERO)
    });
    new_ball.position = dest_st_pos + glam::DVec3::new(1_500.0, 0.0, 0.0);
    new_ball.velocity = glam::DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = eve_destiny::MovementMode::Stop;
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    let new_tick = new_handle.spawn_ball_with_combat(new_ball, combat_state).await.unwrap_or(0);
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys_id,
        new_tick,
        "jump_bridge",
    );

    session.solar_system_id = Some(updated_ch.solar_system_id);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;

    let new_beyonce = ctx.bind_object(session, "beyonce", dest_sys_id as i64)?;
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce as i64)),
    ]))
}
