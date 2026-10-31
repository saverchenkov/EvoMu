//! `notify_emitters_batch8` — Real Gameplay & Service Notification Emitters (Batch 8: Seasons & Space/Fleet/Combat).
//!
//! Provides stateful service transition helpers and notification emitters for:
//! - Group 1: Seasonal lifecycle & challenge expiration (`OnSeasonStarted`, `OnSeasonEnded`,
//!   `OnSeasonSelectionCleared`, `OnSeasonalGoalsReset`, `OnChallengeExpired`).
//! - Group 4: Space combat, cloak, invulnerability, weapon bank ammo, fleet escalation,
//!   and mobile cyno beacon notifications (`OnRestoringInvulnUpdated`, `OnJumpCloakCancelled`,
//!   `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnFleetJumpBeaconDeployableChange`,
//!   `OnFleetEscalatingPathMessage`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::season_manager::{
    default_fsd_seasons, ensure_character_challenges, load_challenge_definitions,
    load_season_state_from_blobs, load_seasons_catalog, save_season_state_to_blobs,
    sync_character_challenges, with_season_state, SeasonConfig, SEASONS_CATALOG_KEY,
    SEASON_CONFIG_KEY,
};
use crate::system_actor::SystemHandle;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::BTreeMap;

const ANNOUNCED_SEASON_STATES_KEY: &str = "season_manager:announced_states";
const ANNOUNCED_SEASON_INIT_KEY: &str = "season_manager:announced_initialized";
const TICKS_PER_DAY_FT: i64 = 86_400 * 10_000_000;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub(crate) fn broadcast_char_event(
    ctx: &ServerContext,
    event: &str,
    args: Vec<PyRep>,
    fallback_char_id: Option<i64>,
) {
    let sent = ctx.notify(NotificationTarget::All, event, "charid", args.clone());
    if let Some(cid) = fallback_char_id {
        if cid > 0 && !sent.iter().any(|n| n.recipient_char_id == cid) {
            ctx.notify(NotificationTarget::Character(cid), event, "charid", args);
        }
    }
}

fn season_phase_label(cfg: &SeasonConfig, now_ft: i64) -> &'static str {
    if cfg.start_time_ft <= now_ft && now_ft < cfg.end_time_ft {
        "active"
    } else if now_ft >= cfg.end_time_ft {
        "ended"
    } else {
        "upcoming"
    }
}

/// Checks season start/end transitions against `now_blueos_filetime_ticks` and emits
/// `OnSeasonStarted`, `OnSeasonEnded`, and `OnSeasonSelectionCleared` when seasons transition.
pub fn check_and_emit_season_transitions(ctx: &ServerContext) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let (newly_started, newly_ended, cleared_chars) = ctx.db.with_mut_snapshot(|snap| {
        let unseeded = !snap.service_state_blobs.contains_key(SEASONS_CATALOG_KEY);
        let mut catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
        if let Some(cfg_json) = snap.service_state_blobs.get(SEASON_CONFIG_KEY) {
            if let Ok(cfg) = serde_json::from_str::<SeasonConfig>(cfg_json) {
                catalog.entry(cfg.season_id).or_insert(cfg);
            }
        }
        if unseeded {
            if let Ok(json) = serde_json::to_string(&catalog) {
                snap.service_state_blobs
                    .insert(SEASONS_CATALOG_KEY.into(), json);
            }
        }

        let initialized = snap
            .service_state_blobs
            .contains_key(ANNOUNCED_SEASON_INIT_KEY);
        let mut announced: BTreeMap<i64, String> = snap
            .service_state_blobs
            .get(ANNOUNCED_SEASON_STATES_KEY)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();

        if !initialized {
            for (&sid, cfg) in &catalog {
                announced.insert(sid, season_phase_label(cfg, now_ft).to_string());
            }
            snap.service_state_blobs
                .insert(ANNOUNCED_SEASON_INIT_KEY.into(), "1".into());
            if let Ok(json) = serde_json::to_string(&announced) {
                snap.service_state_blobs
                    .insert(ANNOUNCED_SEASON_STATES_KEY.into(), json);
            }
            return (Vec::new(), Vec::new(), Vec::new());
        }

        let mut started = Vec::new();
        let mut ended = Vec::new();
        let mut season_ids: Vec<i64> = catalog.keys().copied().collect();
        season_ids.sort_unstable();
        for sid in season_ids {
            let Some(cfg) = catalog.get(&sid) else { continue; };
            let cur = season_phase_label(cfg, now_ft);
            let prev = announced.get(&sid).map(String::as_str);
            if prev != Some("active") && cur == "active" {
                started.push(sid);
            } else if prev != Some("ended") && cur == "ended" {
                ended.push(sid);
            }
            announced.insert(sid, cur.to_string());
        }

        let mut cleared = Vec::new();
        if !ended.is_empty() {
            let mut char_ids: Vec<i64> = snap.characters.keys().copied().collect();
            char_ids.sort_unstable();
            for cid in char_ids {
                let mut st = load_season_state_from_blobs(snap, cid);
                if st.selected_season_id > 0 && ended.contains(&st.selected_season_id) {
                    st.selected_season_id = 0;
                    save_season_state_to_blobs(snap, cid, &st);
                    snap.service_state_blobs
                        .remove(&format!("active_season:{cid}"));
                    cleared.push(cid);
                }
            }
        }

        if let Ok(json) = serde_json::to_string(&announced) {
            snap.service_state_blobs
                .insert(ANNOUNCED_SEASON_STATES_KEY.into(), json);
        }
        (started, ended, cleared)
    });

    if !newly_started.is_empty() {
        broadcast_char_event(ctx, "OnSeasonStarted", vec![], None);
    }
    if !newly_ended.is_empty() {
        broadcast_char_event(ctx, "OnSeasonEnded", vec![], None);
    }
    for cid in cleared_chars {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSeasonSelectionCleared",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnChallengeExpired(challenge_id, new_challenge)` for each refreshed expired challenge.
pub fn emit_expired_challenges(ctx: &ServerContext, char_id: i64, expired: Vec<(i64, PyRep)>) {
    for (expired_id, new_challenge_kv) in expired {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnChallengeExpired",
            "charid",
            vec![PyRep::Integer(expired_id), new_challenge_kv],
        );
    }
}

/// Handles `seasonManager.select_season(None | 0)` to clear the character's selected season.
pub fn handle_select_season_clear_if_none_or_zero(
    ctx: &ServerContext,
    char_id: i64,
    arg0: Option<&PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(arg0, Some(PyRep::None) | Some(PyRep::Integer(0))) {
        return None;
    }
    let res = with_season_state(ctx, char_id, |st, snap| {
        st.selected_season_id = 0;
        snap.service_state_blobs
            .remove(&format!("active_season:{char_id}"));
        Ok(())
    });
    match res {
        Ok(()) => {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSeasonSelectionCleared",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::Bool(true)))
        }
        Err(e) => Some(Err(e)),
    }
}

fn start_season_impl(
    ctx: &ServerContext,
    caller_char_id: i64,
    season_id: i64,
    duration_days: i64,
) -> Result<PyRep, BridgeRpcError> {
    if season_id <= 0 {
        return Err(rpc_err("InvalidSeasonId", "season_id must be a positive integer"));
    }
    if duration_days <= 0 {
        return Err(rpc_err("InvalidArgument", "duration_days must be a positive integer"));
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    let end_ft = now_ft.saturating_add(duration_days.saturating_mul(TICKS_PER_DAY_FT));
    ctx.db.with_mut_snapshot(|snap| {
        let mut catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
        let mut cfg = catalog
            .get(&season_id)
            .cloned()
            .unwrap_or_else(|| SeasonConfig {
                season_id,
                template_id: season_id,
                season_name: format!("Season {season_id}"),
                ..SeasonConfig::default()
            });
        cfg.season_id = season_id;
        cfg.start_time_ft = now_ft;
        cfg.end_time_ft = end_ft;
        catalog.insert(season_id, cfg.clone());
        if let Ok(json) = serde_json::to_string(&catalog) {
            snap.service_state_blobs
                .insert(SEASONS_CATALOG_KEY.into(), json);
        }
        if let Ok(json) = serde_json::to_string(&cfg) {
            snap.service_state_blobs
                .insert(SEASON_CONFIG_KEY.into(), json);
        }
        let mut announced: BTreeMap<i64, String> = snap
            .service_state_blobs
            .get(ANNOUNCED_SEASON_STATES_KEY)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        announced.insert(season_id, "active".into());
        snap.service_state_blobs
            .insert(ANNOUNCED_SEASON_INIT_KEY.into(), "1".into());
        if let Ok(json) = serde_json::to_string(&announced) {
            snap.service_state_blobs
                .insert(ANNOUNCED_SEASON_STATES_KEY.into(), json);
        }
    });
    broadcast_char_event(ctx, "OnSeasonStarted", vec![], Some(caller_char_id));
    Ok(PyRep::keyval([
        ("seasonID", PyRep::Integer(season_id)),
        ("started", PyRep::Bool(true)),
        ("endTime", PyRep::Integer(end_ft)),
    ]))
}

fn end_season_impl(
    ctx: &ServerContext,
    caller_char_id: i64,
    season_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    if season_id <= 0 {
        return Err(rpc_err("InvalidSeasonId", "season_id must be a positive integer"));
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    let cleared_chars = ctx.db.try_with_mut_snapshot(|snap| {
        let mut catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
        let Some(cfg) = catalog.get_mut(&season_id) else {
            return Err(rpc_err(
                "InvalidSeasonId",
                format!("Unknown season ID {season_id}"),
            ));
        };
        cfg.end_time_ft = now_ft;
        if let Ok(json) = serde_json::to_string(&catalog) {
            snap.service_state_blobs
                .insert(SEASONS_CATALOG_KEY.into(), json);
        }
        let mut announced: BTreeMap<i64, String> = snap
            .service_state_blobs
            .get(ANNOUNCED_SEASON_STATES_KEY)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        announced.insert(season_id, "ended".into());
        snap.service_state_blobs
            .insert(ANNOUNCED_SEASON_INIT_KEY.into(), "1".into());
        if let Ok(json) = serde_json::to_string(&announced) {
            snap.service_state_blobs
                .insert(ANNOUNCED_SEASON_STATES_KEY.into(), json);
        }

        let mut cleared = Vec::new();
        let mut char_ids: Vec<i64> = snap.characters.keys().copied().collect();
        char_ids.sort_unstable();
        for cid in char_ids {
            let mut st = load_season_state_from_blobs(snap, cid);
            if st.selected_season_id == season_id {
                st.selected_season_id = 0;
                save_season_state_to_blobs(snap, cid, &st);
                snap.service_state_blobs
                    .remove(&format!("active_season:{cid}"));
                cleared.push(cid);
            }
        }
        Ok(cleared)
    })?;

    broadcast_char_event(ctx, "OnSeasonEnded", vec![], Some(caller_char_id));
    for &cid in &cleared_chars {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSeasonSelectionCleared",
            "charid",
            vec![],
        );
    }
    Ok(PyRep::keyval([
        ("seasonID", PyRep::Integer(season_id)),
        ("ended", PyRep::Bool(true)),
        ("clearedCharacters", PyRep::Integer(cleared_chars.len() as i64)),
    ]))
}

fn reset_seasonal_goals_impl(ctx: &ServerContext, target_cid: i64) -> Result<PyRep, BridgeRpcError> {
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&target_cid) {
            return Err(rpc_err(
                "CharacterNotFound",
                format!("Character {target_cid} not found"),
            ));
        }
        let defs = load_challenge_definitions(snap);
        let mut st = load_season_state_from_blobs(snap, target_cid);
        st.season_points = 0;
        for ch in st.challenges.values_mut() {
            if !ch.claimed {
                ch.progress = 0;
                ch.completed = false;
            }
        }
        let _ = ensure_character_challenges(&mut st, &defs, snap, target_cid, now_ft);
        sync_character_challenges(&mut st, snap, target_cid);
        save_season_state_to_blobs(snap, target_cid, &st);
        Ok(())
    })?;
    ctx.notify(
        NotificationTarget::Character(target_cid),
        "OnSeasonalGoalsReset",
        "charid",
        vec![],
    );
    Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(target_cid)),
        ("reset", PyRep::Bool(true)),
        ("seasonPoints", PyRep::Integer(0)),
    ]))
}

/// Handles GM `/season` slash commands (`start`, `end`, `clear_selection`, `reset_goals`).
pub fn handle_season_slash(
    ctx: &ServerContext,
    caller_char_id: i64,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let rest = cmd.strip_prefix("/season")?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.first().copied() {
        Some("start") => {
            let Some(sid) = parts.get(1).and_then(|s| s.parse::<i64>().ok()).filter(|&id| id > 0) else {
                return Some(Err(rpc_err(
                    "InvalidSlashArgument",
                    "Usage: /season start <season_id> [duration_days]",
                )));
            };
            let days = match parts.get(2) {
                Some(s) => match s.parse::<i64>().ok().filter(|&d| d > 0) {
                    Some(d) => d,
                    None => {
                        return Some(Err(rpc_err(
                            "InvalidSlashArgument",
                            "duration_days must be a positive integer",
                        )));
                    }
                },
                None => 30,
            };
            Some(start_season_impl(ctx, caller_char_id, sid, days))
        }
        Some("end") => {
            let Some(sid) = parts.get(1).and_then(|s| s.parse::<i64>().ok()).filter(|&id| id > 0) else {
                return Some(Err(rpc_err(
                    "InvalidSlashArgument",
                    "Usage: /season end <season_id>",
                )));
            };
            Some(end_season_impl(ctx, caller_char_id, sid))
        }
        Some("clear_selection" | "clear") => {
            let target_cid = match parts.get(1) {
                Some(s) => match s.parse::<i64>().ok().filter(|&id| id > 0) {
                    Some(id) => id,
                    None => {
                        return Some(Err(rpc_err(
                            "InvalidSlashArgument",
                            "Usage: /season clear_selection [char_id]",
                        )));
                    }
                },
                None => caller_char_id,
            };
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&target_cid));
            if !exists {
                return Some(Err(rpc_err(
                    "CharacterNotFound",
                    format!("Character {target_cid} not found"),
                )));
            }
            handle_select_season_clear_if_none_or_zero(ctx, target_cid, Some(&PyRep::None))
        }
        Some("reset_goals" | "reset") => {
            let target_cid = match parts.get(1) {
                Some(s) => match s.parse::<i64>().ok().filter(|&id| id > 0) {
                    Some(id) => id,
                    None => {
                        return Some(Err(rpc_err(
                            "InvalidSlashArgument",
                            "Usage: /season reset_goals [char_id]",
                        )));
                    }
                },
                None => caller_char_id,
            };
            Some(reset_seasonal_goals_impl(ctx, target_cid))
        }
        _ => Some(Err(rpc_err(
            "InvalidSlashArgument",
            "Usage: /season <start|end|clear_selection|reset_goals> ...",
        ))),
    }
}

/// Checks catalog diff on `seasonManager.reload_fsd_data` and emits `OnSeasonStarted` / `OnSeasonEnded`.
pub fn reconcile_seasons_on_fsd_reload(ctx: &ServerContext, caller_char_id: i64) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let _ = default_fsd_seasons(&ctx.sde, now_ft);
    check_and_emit_season_transitions(ctx);
    broadcast_char_event(ctx, "OnSeasonsFsdDataChanged", vec![], Some(caller_char_id));
}

// ============================================================================
// Group 4: Space Combat, Invulnerability, Weapon Bank Ammo, Fleet & Deployables
// ============================================================================

/// Emits `OnRestoringInvulnUpdated(shipID, endTimeFt, durationFt)` when undock or gate/jump
/// invulnerability starts.
pub fn emit_restoring_invuln_updated(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    duration_ft: i64,
) {
    let end_ft = ctx.now_blueos_filetime_ticks().saturating_add(duration_ft);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnRestoringInvulnUpdated",
        "charid",
        vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(end_ft),
            PyRep::Integer(duration_ft),
        ],
    );
}

/// Breaks gate cloak and undock invulnerability on `ship_id` and emits `OnJumpCloakCancelled` /
/// `OnInvulnCancelled` if active.
pub async fn break_gate_cloak_and_notify(
    ctx: &ServerContext,
    handle: &SystemHandle,
    char_id: i64,
    ship_id: i64,
) -> bool {
    let had_gate_cloak = handle.break_gate_cloak(ship_id).await.unwrap_or(false);
    if had_gate_cloak {
        crate::services::notify_emitters_batch3_space::emit_warp_cloak_or_invuln_cancelled(
            ctx, char_id, ship_id, true, false,
        );
    }
    if let Ok(Some(mut cs)) = handle.get_combat_state(ship_id).await {
        if cs.undock_invulnerable_remaining_ticks > 0 {
            cs.undock_invulnerable_remaining_ticks = 0;
            let _ = handle.set_combat_state(ship_id, cs).await;
            crate::services::notify_emitters_batch3_space::emit_warp_cloak_or_invuln_cancelled(
                ctx, char_id, ship_id, false, true,
            );
        }
    }
    had_gate_cloak
}

/// Emits `OnChargeLoadedToModule()` and, for any affected grouped weapon bank master,
/// `OnAmmoInBankChanged(masterID)`.
pub fn emit_charge_and_bank_ammo_loaded(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    loaded_mod_ids: &[i64],
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnChargeLoadedToModule",
        "charid",
        vec![],
    );
    emit_bank_ammo_changed_if_grouped(ctx, char_id, ship_id, loaded_mod_ids);
}

/// Emits `OnAmmoInBankChanged(masterID)` for each weapon bank master on `ship_id` whose members
/// intersect `mod_ids`.
pub fn emit_bank_ammo_changed_if_grouped(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    mod_ids: &[i64],
) {
    let mut masters: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.weapon_banks
            .get(&ship_id)
            .map(|banks| {
                banks
                    .iter()
                    .filter(|(master, slaves)| {
                        mod_ids.contains(master) || slaves.iter().any(|s| mod_ids.contains(s))
                    })
                    .map(|(&master, _)| master)
                    .collect()
            })
            .unwrap_or_default()
    });
    masters.sort_unstable();
    masters.dedup();
    for master_id in masters {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnAmmoInBankChanged",
            "charid",
            vec![PyRep::Integer(master_id)],
        );
    }
}

/// Resolves `char_id`'s current fleet ID from `snap.fleets` if any.
pub fn char_fleet_id(ctx: &ServerContext, char_id: i64) -> Option<i64> {
    ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .find(|f| f.boss_id == char_id || f.members.contains(&char_id))
            .map(|f| f.fleet_id)
    })
}

/// Emits `OnFleetJumpBeaconDeployableChange(..., active=false)` when a Mobile Cyno Beacon is scooped or unanchored.
pub fn on_deployable_deactivated(ctx: &ServerContext, session_fleet_id: Option<i64>, item_id: i64) {
    let info = ctx.db.with_snapshot(|snap| {
        let st = crate::services::mobile_deployables::read_deployable_state_snap(snap);
        let rec = st.deployables.get(&item_id)?;
        if rec.group_id != crate::services::mobile_deployables::GROUP_MOBILE_CYNO_BEACON {
            return None;
        }
        let fleet_id = session_fleet_id.or_else(|| {
            snap.fleets
                .values()
                .find(|f| f.boss_id == rec.owner_id || f.members.contains(&rec.owner_id))
                .map(|f| f.fleet_id)
        })?;
        Some((fleet_id, rec.solar_system_id, rec.owner_id))
    });
    if let Some((fleet_id, sys_id, owner_id)) = info {
        crate::services::notify_emitters_batch3_space::emit_fleet_jump_beacon_deployable_change(
            ctx,
            Some(fleet_id),
            item_id,
            sys_id,
            owner_id,
            false,
        );
    }
}

/// Emits `OnFleetEscalatingPathMessage(charID, journalEntryTemplateID)` to the character's fleet
/// when an escalation expedition is discovered or advances a step.
pub fn emit_fleet_escalating_path_if_in_fleet(
    ctx: &ServerContext,
    char_id: i64,
    dungeon_id: u32,
) {
    let Some(fleet_id) = char_fleet_id(ctx, char_id) else {
        return;
    };
    ctx.notify(
        NotificationTarget::Fleet(fleet_id),
        "OnFleetEscalatingPathMessage",
        "fleetid",
        vec![
            PyRep::Integer(char_id),
            PyRep::Integer(i64::from(dungeon_id)),
        ],
    );
}

/// Handles `dogmaIM.LoadAmmo`, emitting `OnChargeBeingLoadedToModule`, `OnChargeLoadedToModule`,
/// and `OnAmmoInBankChanged` for any grouped weapon bank masters.
pub(crate) fn handle_load_ammo(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    use eve_db::InventoryFlag;

    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
    let owns_ship = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
    if !owns_ship {
        return Err(rpc_err(
            "ShipOwnershipViolation",
            format!("Ship {ship_id} is not owned by character {char_id}"),
        ));
    }
    let requested_module_ids: Vec<i64> = match args.get(1) {
        Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    };
    let charge_item_ids: Vec<i64> = match args.get(2) {
        Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    };
    let Some(&first_charge_id) = charge_item_ids.first() else {
        return Err(rpc_err("InvalidArgument", "LoadAmmo requires charge itemID"));
    };

    let (charge_type_id, avail_qty) = ctx.db.with_snapshot(|snap| {
        let Some(stack) = snap.items.get(&first_charge_id) else {
            return Err(rpc_err("ItemNotFound", format!("Charge item {first_charge_id} not found")));
        };
        if stack.owner_id != char_id {
            return Err(rpc_err(
                "ItemOwnershipViolation",
                format!("Charge item {first_charge_id} is not owned by character {char_id}"),
            ));
        }
        let current_dock = session.station_id.or(session.structure_id).or_else(|| {
            snap.characters
                .get(&char_id)
                .and_then(|c| c.station_id.or(c.structure_id))
        });
        let in_ship_ammo_hold = stack.owner_id == char_id
            && stack.quantity > 0
            && stack.flag != InventoryFlag::Locked
            && stack.location_id == ship_id
            && matches!(stack.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold)
            && snap.is_ship_colocated_with_character(char_id, ship_id, current_dock)
            && !snap.is_item_locked_in_contract_or_job(first_charge_id);
        if !snap.is_item_consumable_by(stack, char_id, current_dock, Some(ship_id))
            && !in_ship_ammo_hold
        {
            return Err(rpc_err(
                "AmmoNotReachable",
                format!("Charge item {first_charge_id} must be unlocked in your current station hangar or active ship cargo/ammo hold and not in a contract"),
            ));
        }
        Ok((stack.type_id, stack.quantity))
    })?;
    let charge_vol = ctx
        .sde
        .types
        .get(&charge_type_id)
        .map(|t| t.volume.max(0.0001))
        .unwrap_or(0.0025);

    let all_target_modules: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for &req_id in &requested_module_ids {
            let resolved_mod_id = if snap.items.contains_key(&req_id) {
                req_id
            } else if let Some(found) = snap.items.values().find(|i| {
                i.owner_id == char_id && i.location_id == ship_id && (i.flag as i64) == req_id
            }) {
                found.item_id
            } else {
                req_id
            };
            if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&resolved_mod_id))
            {
                for &m in bank {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            } else if !out.contains(&resolved_mod_id) {
                out.push(resolved_mod_id);
            }
        }
        out
    });
    if all_target_modules.is_empty() {
        return Ok(PyRep::None);
    }

    for &mod_id in &all_target_modules {
        let Some(mod_item) = ctx.db.with_snapshot(|snap| snap.items.get(&mod_id).cloned()) else {
            return Err(rpc_err("ModuleNotFound", format!("Target module {mod_id} not found")));
        };
        if mod_item.owner_id != char_id
            || mod_item.location_id != ship_id
            || !matches!(mod_item.flag as u16, 11..=34 | 125..=132)
        {
            return Err(rpc_err(
                "ModuleOwnershipViolation",
                format!("Target module {mod_id} is not fitted on ship {ship_id} owned by character {char_id}"),
            ));
        }
        if let (Some(wt), Some(ct)) = (
            ctx.sde.types.get(&mod_item.type_id),
            ctx.sde.types.get(&charge_type_id),
        ) {
            let aar_paste_ok = matches!(wt.group_id, 1199 | 1698) && ct.type_id == 28668;
            let cap_booster_ok = matches!(wt.group_id, 76 | 1156 | 1697)
                && ct.group_id == 87
                && ct.volume <= wt.capacity.max(1.0) + 1e-6;
            let wubble_probe_ok = wt.group_id == 589
                && matches!(ct.group_id, 548 | 4088)
                && ct.volume <= wt.capacity.max(1.0) + 1e-6;
            if !ServerContext::is_charge_compatible_with_weapon(wt, ct)
                && !aar_paste_ok
                && !cap_booster_ok
                && !wubble_probe_ok
            {
                return Err(rpc_err(
                    "IncompatibleAmmo",
                    format!("Charge {} is incompatible with weapon {}", ct.name, wt.name),
                ));
            }
            if matches!(ct.group_id, 482 | 663) {
                ctx.db.with_snapshot(|snap| {
                    ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, charge_type_id)
                })?;
            }
        }
    }

    let mut reload_time_ms = 10_000.0;
    let mut first_mod_group_id = 0u32;
    let mut per_mod_caps: Vec<(i64, i32)> = Vec::with_capacity(all_target_modules.len());
    for (idx, &mod_id) in all_target_modules.iter().enumerate() {
        let mut cap_rounds = 1i32;
        if let Some(mod_item) = ctx.db.with_snapshot(|snap| snap.items.get(&mod_id).cloned()) {
            if let Some(mod_t) = ctx.sde.types.get(&mod_item.type_id) {
                if idx == 0 {
                    first_mod_group_id = mod_t.group_id;
                    if let Some(&rt) = mod_t.default_attributes.get(&1795) {
                        reload_time_ms = rt;
                    }
                    if mod_t.group_id == 1770 {
                        let cbs_lvl = ctx
                            .character_skill_levels(char_id)
                            .get(&43728)
                            .copied()
                            .unwrap_or(0)
                            .min(5);
                        reload_time_ms = 60_000.0 * (1.0 - 0.10 * (cbs_lvl as f64));
                    }
                }
                let cap = mod_t.capacity.max(0.0);
                if cap > 0.0 {
                    cap_rounds = (cap / charge_vol).floor() as i32;
                }
            }
        }
        per_mod_caps.push((mod_id, cap_rounds.max(1)));
    }

    let num_mods = all_target_modules.len() as i32;
    if avail_qty < num_mods {
        return Err(rpc_err(
            "InsufficientQuantity",
            format!("Need at least {num_mods} charges, but only {avail_qty} available in stack"),
        ));
    }
    let fair_share = (avail_qty / num_mods).max(1);
    let mut mod_allocations: Vec<(i64, i32)> = Vec::with_capacity(per_mod_caps.len());
    let mut total_needed = 0i32;
    for &(mod_id, cap_rounds) in &per_mod_caps {
        let alloc = cap_rounds.min(fair_share).max(1);
        mod_allocations.push((mod_id, alloc));
        total_needed += alloc;
    }

    let cargo_cap_opt = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
        .filter(|&c| c > 0.0)
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .and_then(|s| ctx.sde.types.get(&s.type_id))
                    .map(|t| t.capacity)
                    .filter(|&c| c > 0.0)
            })
        });

    ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(cargo_cap) = cargo_cap_opt {
            let source_in_cargo = snap
                .items
                .get(&first_charge_id)
                .is_some_and(|s| s.location_id == ship_id && s.flag == InventoryFlag::Cargo);
            let current_cargo_used: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                .map(|i| {
                    let v = ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0);
                    v * (i.quantity as f64)
                })
                .sum();
            let removed_from_cargo_vol = if source_in_cargo {
                charge_vol * (total_needed as f64)
            } else {
                0.0
            };
            let mut unloaded_to_cargo_vol = 0.0;
            for &(mod_id, _) in &mod_allocations {
                if let Some(mod_item) = snap.items.get(&mod_id) {
                    let slot_flag = mod_item.flag;
                    for prev in snap.items.values().filter(|i| {
                        i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id
                    }) {
                        let pv = ctx.sde.types.get(&prev.type_id).map(|t| t.volume).unwrap_or(0.0);
                        unloaded_to_cargo_vol += pv * (prev.quantity as f64);
                    }
                }
            }
            if unloaded_to_cargo_vol > 0.0
                && (current_cargo_used - removed_from_cargo_vol + unloaded_to_cargo_vol)
                    > cargo_cap + 1e-4
            {
                return Err(rpc_err(
                    "NotEnoughCargoSpace",
                    format!("Not enough cargo space on ship {ship_id} to unload existing charges ({unloaded_to_cargo_vol:.1} m³ needed)"),
                ));
            }
        }

        if let Some(stack) = snap.items.get_mut(&first_charge_id) {
            if stack.quantity > total_needed {
                stack.quantity -= total_needed;
                stack.version += 1;
            } else {
                snap.items.remove(&first_charge_id);
            }
        }
        for &(mod_id, alloc_qty) in &mod_allocations {
            let Some(mod_item) = snap.items.get(&mod_id).cloned() else {
                continue;
            };
            if mod_item.owner_id != char_id
                || mod_item.location_id != ship_id
                || !matches!(mod_item.flag as u16, 11..=34 | 125..=132)
            {
                continue;
            }
            let slot_flag = mod_item.flag;
            let prev_charges: Vec<i64> = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id)
                .map(|i| i.item_id)
                .collect();
            for pid in prev_charges {
                if let Some(pi) = snap.items.get_mut(&pid) {
                    pi.flag = InventoryFlag::Cargo;
                }
            }
            snap.next_entity_id += 1;
            let new_charge_id = snap.next_entity_id;
            snap.items.insert(
                new_charge_id,
                eve_db::InventoryItemRecord {
                    item_id: new_charge_id,
                    type_id: charge_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: slot_flag,
                    quantity: alloc_qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        Ok(())
    })?;

    let is_in_space = session.station_id.is_none() && session.structure_id.is_none();
    let is_script = ctx.sde.types.get(&charge_type_id).is_some_and(|t| t.group_id == 908);
    if is_in_space && !is_script && first_mod_group_id != 481 && reload_time_ms > 0.0 {
        let reload_secs = (reload_time_ms / 1000.0).ceil() as u64;
        crate::services::inventory_fitting::set_modules_reload_until(
            ctx,
            &all_target_modules,
            ctx.now_unix_secs() + reload_secs,
        );
    }

    let _ = ctx.notify(
        NotificationTarget::Character(char_id),
        "OnChargeBeingLoadedToModule",
        "charid",
        vec![
            PyRep::List(all_target_modules.iter().map(|&id| PyRep::Integer(id)).collect()),
            PyRep::Integer(charge_type_id as i64),
            PyRep::Real(reload_time_ms),
        ],
    );
    emit_charge_and_bank_ammo_loaded(ctx, char_id, ship_id, &all_target_modules);
    ctx.invalidate_ship_fitting(ship_id);

    Ok(PyRep::keyval([
        ("reloadTimeMs", PyRep::Real(reload_time_ms)),
        ("totalLoaded", PyRep::Integer(total_needed as i64)),
    ]))
}
