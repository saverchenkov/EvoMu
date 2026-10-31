//! `structure_control_ops` — Structure command/gunnery control (`TakeControl`, `BoardStructure`,
//! `ReleaseControl`, `EjectFromStructure`, `GetStructurePilot`), undock control release, and
//! SDE-driven unpiloted Standup weapon auto-defense ticks (`feat:structures.board-structure`,
//! `feat:structures.defense-settings`).
#![forbid(unsafe_code)]

use super::station_structure::structure_lifecycle::{is_operational, structure_profile, HullClass};
use super::station_structure::structure_upwell_ops::{
    self as ops, apply_pending_profile_applications_with_snap, emit_ship_session_change,
    load_ext_state, require_structure_control_role, save_ext_state,
};
use super::structure_acl_defense_rules::evaluate_auto_defense_target;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::WorldStateSnapshot;
use eve_destiny::{DamageVector, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

/// Maximum boarding proximity (m) beyond structure radius for FLEX structures (`2,500 m`).
pub const FLEX_BOARDING_PROXIMITY_M: f64 = 2_500.0;
/// Standup Point Defense Battery (`35926` / `47327`) maximum point-blank range (`25,000 m`).
pub const STANDUP_PDB_RANGE_M: f64 = 25_000.0;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

fn arg_i64(args: &[PyRep], idx: usize) -> Option<i64> {
    args.get(idx).and_then(|a| a.as_i64())
}

pub(crate) fn can_character_control_structure(
    snap: &WorldStateSnapshot,
    ext: &ops::StructureExtendedState,
    st: &eve_db::UpwellStructureRecord,
    char_id: i64,
) -> bool {
    verify_control_acl_or_role(snap, ext, st, char_id).is_ok()
}

fn verify_control_acl_or_role(
    snap: &WorldStateSnapshot,
    ext: &ops::StructureExtendedState,
    st: &eve_db::UpwellStructureRecord,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    let caller_corp = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let caller_ally = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.alliance_id)
        .or_else(|| snap.corporations.get(&caller_corp).and_then(|c| c.alliance_id))
        .unwrap_or(0);
    let has_defense_acl = if let Some(p) = ext.access_profiles.get(&st.structure_id) {
        if p.blocked_characters.contains(&char_id)
            || p.defense_blocked_characters.contains(&char_id)
            || (caller_corp > 0 && p.blocked_corporations.contains(&caller_corp))
            || (caller_ally > 0 && p.blocked_alliances.contains(&caller_ally))
        {
            return Err(err(
                "DefenseAccessDenied",
                format!("Character {char_id} (corp {caller_corp}, alliance {caller_ally}) is blocked by structure profile"),
            ));
        }
        for &gid in &p.defense_group_ids {
            if let Some(grp) = ext.owner_groups.get(&gid) {
                if grp.members.get(&char_id) == Some(&-1)
                    || (caller_corp > 0 && grp.members.get(&caller_corp) == Some(&-1))
                    || (caller_ally > 0 && grp.members.get(&caller_ally) == Some(&-1))
                {
                    return Err(err(
                        "DefenseAccessDenied",
                        format!("Character {char_id} is blocked in defense access group {gid}"),
                    ));
                }
            }
        }
        p.defense_allowed_characters.contains(&char_id)
            || p.defense_group_ids.iter().any(|gid| {
                ext.owner_groups.get(gid).is_some_and(|g| {
                    g.members.get(&0).is_some_and(|&r| r >= 0)
                        || g.members.get(&char_id).is_some_and(|&r| r >= 0)
                        || (caller_corp > 0 && g.members.get(&caller_corp).is_some_and(|&r| r >= 0))
                        || (caller_ally > 0 && g.members.get(&caller_ally).is_some_and(|&r| r >= 0))
                })
            })
    } else {
        false
    };

    if !has_defense_acl {
        if st.owner_corp_id != caller_corp {
            return Err(err(
                "DefenseAccessDenied",
                format!(
                    "Character {char_id} does not belong to owning corp {} of structure {} and has no defense ACL permission",
                    st.owner_corp_id, st.structure_id
                ),
            ));
        }
        require_structure_control_role(snap, char_id, st)?;
    }
    Ok(())
}

async fn engage_structure_control(
    ctx: &ServerContext,
    session: &mut SessionState,
    structure_id: i64,
    char_id: i64,
    board_mode: bool,
) -> Result<u32, BridgeRpcError> {
    let (sys_id, is_flex, struct_pos, struct_radius) = ctx.db.with_snapshot(|snap| {
        let st = snap
            .structures
            .get(&structure_id)
            .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} does not exist")))?;
        if st.state == "Destroyed" {
            return Err(err(
                "StructureDestroyed",
                format!("Structure {structure_id} has been destroyed"),
            ));
        }
        let prof = structure_profile(ctx, st.type_id);
        let is_flex = prof.as_ref().is_some_and(|p| p.hull_class == HullClass::Flex);
        let radius = ctx
            .sde
            .types
            .get(&st.type_id)
            .map(|t| t.radius.max(1_000.0))
            .unwrap_or(5_000.0);
        Ok((st.solar_system_id, is_flex, DVec3::from_array(st.position), radius))
    })?;

    if is_flex {
        let ship_id = session
            .ship_id
            .ok_or_else(|| err("NoActiveShip", "Must be in an active ship in space to control a FLEX structure"))?;
        if session.solar_system_id != Some(sys_id) {
            return Err(err(
                "OutOfBoardingRange",
                format!("Must be in solar system {sys_id} to control FLEX structure {structure_id}"),
            ));
        }
        let ship_pos = ctx
            .systems
            .get_or_spawn(sys_id)
            .get_ball_snapshot(ship_id)
            .await
            .map_err(|e| err("DestinyQueryFailed", e))?
            .map(|(_, b)| b.position)
            .or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.item_positions.get(&ship_id).copied().map(DVec3::from_array))
            })
            .ok_or_else(|| {
                err(
                    "OutOfBoardingRange",
                    format!("Ship {ship_id} has no live position in solar system {sys_id}"),
                )
            })?;
        let max_range = struct_radius + FLEX_BOARDING_PROXIMITY_M;
        if ship_pos.distance(struct_pos) > max_range {
            return Err(err(
                "OutOfBoardingRange",
                format!(
                    "Ship {ship_id} must be within {:.0} m of FLEX structure {structure_id} to assume control",
                    max_range
                ),
            ));
        }
    } else if session.structure_id != Some(structure_id) {
        return Err(err(
            "NotDockedInStructure",
            format!("Must be docked in structure {structure_id} to take control"),
        ));
    }

    let now_unix = ctx.now_unix();
    let (released, verify_err) = ctx.db.try_with_mut_snapshot(|snap| {
        let st = snap
            .structures
            .get(&structure_id)
            .cloned()
            .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} does not exist")))?;
        if st.state == "Destroyed" {
            return Err(err(
                "StructureDestroyed",
                format!("Structure {structure_id} has been destroyed"),
            ));
        }
        let mut ext = load_ext_state(snap);
        let released = apply_pending_profile_applications_with_snap(snap, &mut ext, now_unix);
        if let Err(e) = verify_control_acl_or_role(snap, &ext, &st, char_id) {
            return Ok((released, Some(e)));
        }
        let already_ctrl = ext.active_pilot_by_structure.get(&structure_id) == Some(&char_id);
        if let Some(&pilot) = ext.active_pilot_by_structure.get(&structure_id) {
            if pilot != char_id {
                return Err(err(
                    "StructureAlreadyControlled",
                    format!("Structure {structure_id} is already controlled by pilot {pilot}"),
                ));
            }
        }
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        let prev = session
            .ship_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id))
            .unwrap_or(0);
        if prev > 0 && prev != structure_id && !snap.structures.contains_key(&prev) {
            if already_ctrl {
                ext.previous_ship_by_structure.entry(structure_id).or_insert(prev);
            } else {
                ext.previous_ship_by_structure.insert(structure_id, prev);
            }
        }
        save_ext_state(snap, &ext);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = structure_id;
        }
        Ok((released, None))
    })?;
    notify_released_structure_controllers(ctx, &released);
    if let Some(e) = verify_err {
        return Err(e);
    }

    let old_ship = session.ship_id;
    session.ship_id = Some(structure_id);
    emit_ship_session_change(ctx, session, char_id, old_ship, Some(structure_id));
    let _ = (sys_id, board_mode);
    Ok(sys_id)
}

fn precheck_saved_in_space_ball(ctx: &ServerContext, explicit_structure_id: Option<i64>, char_id: i64) -> Option<bool> {
    let query = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let sid = explicit_structure_id.or_else(|| {
            ext.active_pilot_by_structure.iter().find_map(|(&s, &p)| (p == char_id).then_some(s))
        })?;
        if snap.characters.get(&char_id).is_none_or(|c| c.structure_id == Some(sid) || c.station_id.is_some()) {
            return None;
        }
        let sys_id = snap.structures.get(&sid).map(|s| s.solar_system_id).unwrap_or(0);
        let saved_prev = ext.previous_ship_by_structure.get(&sid).copied().unwrap_or(0);
        (sys_id > 0 && saved_prev > 0).then_some((sys_id, saved_prev))
    });
    query.map(|(sys_id, saved_prev)| ctx.systems.get_or_spawn(sys_id).get_ball_position_sync(saved_prev).is_some())
}

#[allow(clippy::too_many_arguments)]
fn is_valid_pilot_fallback_ship(
    ctx: Option<&ServerContext>,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    sys_id: u32,
    char_id: i64,
    is_docked: bool,
    in_space_ball_alive: Option<bool>,
    item: &eve_db::InventoryItemRecord,
) -> bool {
    item.owner_id == char_id
        && item.singleton
        && item.item_id != structure_id
        && !snap.structures.contains_key(&item.item_id)
        && !snap.is_item_locked_in_contract_or_job(item.item_id)
        && !matches!(item.flag, eve_db::InventoryFlag::Locked | eve_db::InventoryFlag::Deliveries)
        && ctx.map_or(true, |c| c.sde.types.get(&item.type_id).is_some_and(|t| t.category_id == 6))
        && if is_docked {
            item.location_id == structure_id && item.flag == eve_db::InventoryFlag::Hangar
        } else {
            sys_id > 0 && item.location_id == i64::from(sys_id) && in_space_ball_alive.unwrap_or(true)
        }
}

fn find_valid_pilot_fallback_ship(
    ctx: Option<&ServerContext>,
    snap: &WorldStateSnapshot,
    ext: &ops::StructureExtendedState,
    structure_id: i64,
    char_id: i64,
    in_space_ball_alive: Option<bool>,
) -> Option<i64> {
    let sys_id = snap.structures.get(&structure_id).map(|s| s.solar_system_id).unwrap_or(0);
    let is_docked = snap.characters.get(&char_id).is_none_or(|c| c.structure_id == Some(structure_id) || c.station_id.is_some());
    let saved_prev = ext.previous_ship_by_structure.get(&structure_id).copied().unwrap_or(0);
    if saved_prev > 0
        && snap.items.get(&saved_prev).is_some_and(|it| {
            is_valid_pilot_fallback_ship(ctx, snap, structure_id, sys_id, char_id, is_docked, in_space_ball_alive, it)
        })
    {
        return Some(saved_prev);
    }
    if !is_docked {
        return None;
    }
    snap.items
        .values()
        .filter(|it| is_valid_pilot_fallback_ship(ctx, snap, structure_id, sys_id, char_id, true, None, it))
        .min_by_key(|it| (it.type_id == 670, it.item_id))
        .map(|it| it.item_id)
}

fn provision_pilot_capsule_in_snap(snap: &mut WorldStateSnapshot, structure_id: i64, char_id: i64) -> i64 {
    let (sys_id, st_pos) = snap
        .structures
        .get(&structure_id)
        .map(|s| (s.solar_system_id, DVec3::from_array(s.position)))
        .unwrap_or((0, DVec3::ZERO));
    let is_docked = snap.characters.get(&char_id).is_none_or(|c| c.structure_id == Some(structure_id) || c.station_id.is_some());
    if is_docked {
        if let Some(cid) = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == char_id
                    && i.type_id == 670
                    && i.singleton
                    && i.location_id == structure_id
                    && i.flag == eve_db::InventoryFlag::Hangar
                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
            })
            .min_by_key(|i| i.item_id)
            .map(|i| i.item_id)
        {
            return cid;
        }
    }
    snap.next_entity_id += 1;
    let cid = snap.next_entity_id;
    let (location_id, flag) = if is_docked {
        (structure_id, eve_db::InventoryFlag::Hangar)
    } else {
        let pod_pos = st_pos + DVec3::new(150.0, 0.0, 0.0);
        snap.item_positions.insert(cid, pod_pos.to_array());
        (i64::from(sys_id), eve_db::InventoryFlag::None)
    };
    snap.items.insert(
        cid,
        eve_db::InventoryItemRecord {
            item_id: cid, type_id: 670, owner_id: char_id, location_id, flag,
            quantity: 1, singleton: true, version: 1, ..Default::default()
        },
    );
    cid
}

fn ensure_in_space_capsule_ball_spawned(ctx: &ServerContext, sys_id: u32, char_id: i64, ship_id: i64) {
    if sys_id == 0 || ship_id <= 0 {
        return;
    }
    let pod_info = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id)?;
        let it = snap.items.get(&ship_id)?;
        if ch.station_id.is_some() || ch.structure_id.is_some() || it.type_id != 670 || it.location_id != i64::from(sys_id) {
            return None;
        }
        let pos = snap.item_positions.get(&ship_id).copied().map(DVec3::from_array).unwrap_or(DVec3::ZERO);
        let all_id = ch
            .alliance_id
            .or_else(|| snap.corporations.get(&ch.corporation_id).and_then(|co| co.alliance_id))
            .unwrap_or(-1);
        Some((pos, ch.corporation_id, all_id, ch.security_status))
    });
    let Some((pos, corp_id, alliance_id, sec_status)) = pod_info else {
        return;
    };
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, 670);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.get_ball_position_sync(ship_id).is_none() {
        let cap_sde = ctx.sde.types.get(&670);
        sys_handle.try_spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id, type_id: 670, position: pos, velocity: DVec3::ZERO,
                max_velocity: cap_sde.and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(335.0),
                inertia: cap_sde.and_then(|t| t.default_attributes.get(&70).copied()).unwrap_or(0.25),
                mass: cap_sde.map(|t| t.mass.max(1_000.0)).unwrap_or(16_000.0),
                radius: cap_sde.map(|t| t.radius.max(15.0)).unwrap_or(15.0),
                speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            },
            combat_state,
            eve_destiny::SlimItemMeta {
                group_id: cap_sde.map(|t| t.group_id).unwrap_or(29), category_id: 6, owner_id: char_id,
                char_id: Some(char_id), corp_id, alliance_id, security_status: sec_status, is_cloaked: false,
            },
        );
    }
}

fn sync_released_ship_on_session(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    structure_id: i64,
    restored_ship: i64,
) {
    let old_ship = session.ship_id;
    if restored_ship > 0 {
        session.ship_id = Some(restored_ship);
        emit_ship_session_change(ctx, session, char_id, old_ship, Some(restored_ship));
    } else if session.ship_id == Some(structure_id) {
        session.ship_id = None;
        emit_ship_session_change(ctx, session, char_id, old_ship, None);
    }
}

/// Temporarily restores `ch.active_ship_id` to the pilot's docked ship before `undock_character`
/// runs without releasing structure control yet, so a failed undock preserves structure control.
pub fn prepare_ship_for_undock_while_controlling(ctx: &ServerContext, structure_id: i64, char_id: i64) -> Option<i64> {
    ctx.db.with_mut_snapshot(|snap| {
        let ext = load_ext_state(snap);
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return None;
        }
        let fallback_ship = find_valid_pilot_fallback_ship(Some(ctx), snap, &ext, structure_id, char_id, None).unwrap_or(0);
        if fallback_ship > 0 {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                if ch.active_ship_id == structure_id || ch.active_ship_id <= 0 {
                    ch.active_ship_id = fallback_ship;
                }
            }
        }
        Some(fallback_ship)
    })
}

/// Restores `ch.active_ship_id = structure_id` if `undock_character` failed after
/// `prepare_ship_for_undock_while_controlling`.
pub fn rollback_ship_after_failed_undock(ctx: &ServerContext, structure_id: i64, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        if load_ext_state(snap).active_pilot_by_structure.get(&structure_id) == Some(&char_id) {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.active_ship_id = structure_id;
            }
        }
    });
}

fn disengage_structure_control_in_snap(
    ctx: Option<&ServerContext>,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    in_space_ball_alive: Option<bool>,
) -> (u32, i64) {
    let sys_id = snap.structures.get(&structure_id).map(|s| s.solar_system_id).unwrap_or(0);
    let mut ext = load_ext_state(snap);
    let existing_active = snap.characters.get(&char_id).map(|c| c.active_ship_id).filter(|&sid| {
        sid > 0
            && sid != structure_id
            && !snap.structures.contains_key(&sid)
            && snap.items.get(&sid).is_some_and(|i| {
                i.owner_id == char_id
                    && i.singleton
                    && !snap.is_item_locked_in_contract_or_job(sid)
                    && !matches!(i.flag, eve_db::InventoryFlag::Locked | eve_db::InventoryFlag::Deliveries)
            })
    });
    let fallback_ship = existing_active
        .or_else(|| find_valid_pilot_fallback_ship(ctx, snap, &ext, structure_id, char_id, in_space_ball_alive))
        .unwrap_or_else(|| provision_pilot_capsule_in_snap(snap, structure_id, char_id));
    ext.active_pilot_by_structure.remove(&structure_id);
    ext.previous_ship_by_structure.remove(&structure_id);
    save_ext_state(snap, &ext);
    let restored = if let Some(ch) = snap.characters.get_mut(&char_id) {
        if existing_active.is_none() {
            ch.active_ship_id = fallback_ship;
        }
        ch.active_ship_id
    } else {
        fallback_ship
    };
    (sys_id, restored)
}

/// Releases all structure control held by `char_id` (used on logout, disconnect, character switch,
/// or character login reconciliation) and restores `ch.active_ship_id` to the pilot's previous ship.
pub fn release_all_structure_control_for_character(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    char_id: i64,
) -> Vec<(i64, u32, i64)> {
    let controlled_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        load_ext_state(snap).active_pilot_by_structure.iter().filter_map(|(&sid, &pid)| (pid == char_id).then_some(sid)).collect()
    });
    let ball_checks: std::collections::HashMap<i64, Option<bool>> = controlled_ids
        .into_iter()
        .map(|sid| (sid, precheck_saved_in_space_ball(ctx, Some(sid), char_id)))
        .collect();
    let released: Vec<(i64, u32, i64)> = ctx.db.with_mut_snapshot(|snap| {
        let controlled: Vec<i64> = load_ext_state(snap)
            .active_pilot_by_structure
            .iter()
            .filter_map(|(&sid, &pid)| (pid == char_id).then_some(sid))
            .collect();
        controlled
            .into_iter()
            .map(|sid| {
                let (sys_id, restored) =
                    disengage_structure_control_in_snap(Some(ctx), snap, sid, char_id, ball_checks.get(&sid).copied().flatten());
                (sid, sys_id, restored)
            })
            .collect()
    });
    for &(structure_id, sys_id, restored_ship) in &released {
        ensure_in_space_capsule_ball_spawned(ctx, sys_id, char_id, restored_ship);
        if let Some(sess) = session.as_deref_mut() {
            sync_released_ship_on_session(ctx, sess, char_id, structure_id, restored_ship);
        }
    }
    released
}

/// Scans `ext.active_pilot_by_structure` in `snap` and releases any pilot whose corporation
/// membership, corp defense/control role, or structure defense ACL no longer permits control.
pub fn release_unauthorized_structure_controllers_in_snap(snap: &mut WorldStateSnapshot) -> Vec<(i64, i64, u32, i64)> {
    let pairs: Vec<(i64, i64)> = load_ext_state(snap).active_pilot_by_structure.iter().map(|(&sid, &pid)| (sid, pid)).collect();
    let mut released = Vec::new();
    for (structure_id, pilot_id) in pairs {
        let authorized = match snap.structures.get(&structure_id) {
            Some(st) if st.state != "Destroyed" => verify_control_acl_or_role(snap, &load_ext_state(snap), st, pilot_id).is_ok(),
            _ => false,
        };
        if !authorized {
            let (sys_id, restored_ship) = disengage_structure_control_in_snap(None, snap, structure_id, pilot_id, None);
            released.push((structure_id, pilot_id, sys_id, restored_ship));
        }
    }
    released
}

/// Synchronizes live `SessionState` with current DB character spatial/vessel state and
/// any queued involuntary corp/structure-control release updates (`ServerContext::sync_session`).
pub(crate) fn sync_character_session(ctx: &ServerContext, session: &mut SessionState) {
    if let Some(cid) = session.character_id {
        if let Some((new_corp, new_ally, new_role)) =
            crate::services::fleet_social::fleet_social_persistence::with_social_state(ctx, |s| s.forced_session_corp_sync.remove(&cid))
        {
            session.corporation_id = Some(new_corp);
            session.alliance_id = new_ally;
            session.corp_role = new_role;
        }
        apply_forced_session_ship_sync(ctx, session);
        if let Some((act_ship, st_id, str_id, sys_id, const_id, reg_id, stale_struct)) = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&cid).map(|c| {
                (
                    c.active_ship_id, c.station_id, c.structure_id, c.solar_system_id, c.constellation_id, c.region_id,
                    session.ship_id.is_some_and(|sid| snap.structures.contains_key(&sid)),
                )
            })
        }) {
            session.solar_system_id = Some(sys_id);
            if const_id != 0 {
                session.constellation_id = Some(const_id);
            }
            if reg_id != 0 {
                session.region_id = Some(reg_id);
            }
            session.station_id = st_id;
            session.structure_id = str_id;
            if act_ship != 0 {
                session.ship_id = Some(act_ship);
            } else if stale_struct {
                session.ship_id = None;
            }
        }
    }
}

/// Synchronizes live `SessionState.ship_id` when a pilot was involuntarily displaced from
/// controlling a structure (called from `ServerContext::sync_session` and `macho_listener`).
pub(crate) fn apply_forced_session_ship_sync(ctx: &ServerContext, session: &mut SessionState) {
    if let Some(cid) = session.character_id {
        if let Some((old_structure_id, new_ship_opt)) =
            crate::services::fleet_social::fleet_social_persistence::with_social_state(ctx, |s| s.forced_session_ship_sync.remove(&cid))
        {
            if new_ship_opt.is_some() || session.ship_id == Some(old_structure_id) {
                session.ship_id = new_ship_opt;
            }
        }
    }
}

/// Emits `OnSessionChanged` (`shipid`) and `OnStructureTargetRemoved` notifications to pilots
/// whose structure control was revoked asynchronously, and queues `forced_session_ship_sync`.
pub fn notify_released_structure_controllers(ctx: &ServerContext, released: &[(i64, i64, u32, i64)]) {
    for &(structure_id, pilot_id, sys_id, restored_ship) in released {
        ensure_in_space_capsule_ball_spawned(ctx, sys_id, pilot_id, restored_ship);
        let new_ship_opt = (restored_ship > 0).then_some(restored_ship);
        crate::services::fleet_social::fleet_social_persistence::with_social_state(ctx, |s| {
            s.forced_session_ship_sync.insert(pilot_id, (structure_id, new_ship_opt));
        });
        let new_py = new_ship_opt.map(PyRep::Integer).unwrap_or(PyRep::None);
        let sess_rep = PyRep::keyval([("charid", PyRep::Integer(pilot_id)), ("shipid", new_py.clone())]);
        let change_args = vec![
            PyRep::Bool(false),
            sess_rep,
            PyRep::Dict(vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::Integer(structure_id), new_py]))]),
        ];
        ctx.notify(NotificationTarget::Character(pilot_id), "OnSessionChanged", "charid", change_args);
        ctx.notify(
            NotificationTarget::Character(pilot_id),
            "OnStructureTargetRemoved",
            "charid",
            vec![PyRep::Integer(structure_id), PyRep::Integer(0)],
        );
    }
}

/// Releases any structure controllers in `ctx.db` who are no longer authorized and notifies them.
pub fn release_unauthorized_structure_controllers(ctx: &ServerContext) -> Vec<(i64, i64, u32, i64)> {
    let released = ctx.db.with_mut_snapshot(release_unauthorized_structure_controllers_in_snap);
    notify_released_structure_controllers(ctx, &released);
    released
}

/// Releases structure control if `char_id` is currently controlling `structure_id` when undocking.
pub fn release_structure_control_on_undock(ctx: &ServerContext, session: &mut SessionState, structure_id: i64, char_id: i64) {
    let released = ctx.db.with_mut_snapshot(|snap| {
        if load_ext_state(snap).active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return None;
        }
        Some(disengage_structure_control_in_snap(Some(ctx), snap, structure_id, char_id, None))
    });
    if let Some((_, restored_ship)) = released {
        sync_released_ship_on_session(ctx, session, char_id, structure_id, restored_ship);
    }
}

pub async fn handle_take_control(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "TakeControl requires structureID"))?;
    let _ = engage_structure_control(ctx, session, structure_id, char_id, false).await?;
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("pilotID", PyRep::Integer(char_id)),
        ("success", PyRep::Bool(true)),
    ]))
}

pub async fn handle_board_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "BoardStructure requires structureID"))?;
    let _ = engage_structure_control(ctx, session, structure_id, char_id, true).await?;
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("pilotID", PyRep::Integer(char_id)),
        ("boarded", PyRep::Bool(true)),
    ]))
}

pub fn handle_release_control(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let explicit_or_docked_id = arg_i64(args, 0).or(session.structure_id);
    let session_ship = session.ship_id;
    let target_id = arg_i64(args, 1).unwrap_or(0);
    let in_space_ball_alive = precheck_saved_in_space_ball(ctx, explicit_or_docked_id, char_id);
    let (structure_id, sys_id, orig_ship_id) = ctx.db.try_with_mut_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let structure_id = explicit_or_docked_id
            .or_else(|| session_ship.filter(|sid| snap.structures.contains_key(sid)))
            .or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id).filter(|sid| snap.structures.contains_key(sid)))
            .or_else(|| ext.active_pilot_by_structure.iter().find_map(|(&sid, &pid)| (pid == char_id).then_some(sid)))
            .ok_or_else(|| err("MissingStructureId", "ReleaseControl requires structureID"))?;
        if snap.structures.get(&structure_id).is_some_and(|s| s.state == "Destroyed") {
            return Err(err("StructureDestroyed", format!("Structure {structure_id} has been destroyed")));
        }
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err("NotControllingStructure", format!("Character {char_id} does not control structure {structure_id}")));
        }
        crate::services::structure_standup_weapons::ensure_gunner_not_immobilized(snap, char_id, ctx.now_unix())?;
        let (sys_id, orig_ship_id) = disengage_structure_control_in_snap(Some(ctx), snap, structure_id, char_id, in_space_ball_alive);
        Ok((structure_id, sys_id, orig_ship_id))
    })?;
    ensure_in_space_capsule_ball_spawned(ctx, sys_id, char_id, orig_ship_id);
    sync_released_ship_on_session(ctx, session, char_id, structure_id, orig_ship_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStructureTargetRemoved",
        "charid",
        vec![PyRep::Integer(structure_id), PyRep::Integer(target_id)],
    );
    Ok(PyRep::None)
}

pub fn handle_eject_from_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "EjectFromStructure requires structureID"))?;
    let in_space_ball_alive = precheck_saved_in_space_ball(ctx, Some(structure_id), char_id);
    let (sys_id, orig_ship_id) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.structures.get(&structure_id).is_some_and(|s| s.state == "Destroyed") {
            return Err(err("StructureDestroyed", format!("Structure {structure_id} has been destroyed")));
        }
        if load_ext_state(snap).active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err("NotControllingStructure", format!("Character {char_id} is not boarding/controlling structure {structure_id}")));
        }
        crate::services::structure_standup_weapons::ensure_gunner_not_immobilized(snap, char_id, ctx.now_unix())?;
        Ok(disengage_structure_control_in_snap(Some(ctx), snap, structure_id, char_id, in_space_ball_alive))
    })?;
    ensure_in_space_capsule_ball_spawned(ctx, sys_id, char_id, orig_ship_id);
    sync_released_ship_on_session(ctx, session, char_id, structure_id, orig_ship_id);
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("pilotID", PyRep::None),
        ("boarded", PyRep::Bool(false)),
    ]))
}

pub fn handle_get_structure_pilot(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "GetStructurePilot requires structureID"))?;
    let now_unix = ctx.now_unix();
    let has_due_profile = ctx.db.with_snapshot(|snap| {
        load_ext_state(snap)
            .pending_profile_applications
            .get(&structure_id)
            .is_some_and(|&(_, t)| now_unix >= t)
    });
    if has_due_profile {
        let released = ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            apply_pending_profile_applications_with_snap(snap, &mut ext, now_unix)
        });
        notify_released_structure_controllers(ctx, &released);
    }
    ctx.db.with_snapshot(|snap| {
        if snap
            .structures
            .get(&structure_id)
            .is_some_and(|s| s.state == "Destroyed")
        {
            return Err(err(
                "StructureDestroyed",
                format!("Structure {structure_id} has been destroyed"),
            ));
        }
        Ok(load_ext_state(snap)
            .active_pilot_by_structure
            .get(&structure_id)
            .copied()
            .map(PyRep::Integer)
            .unwrap_or(PyRep::None))
    })
}

struct HostileTargetInfo {
    ship_id: i64,
    pilot_id: i64,
    position: DVec3,
    velocity: DVec3,
    sig_radius: f64,
    dist_m: f64,
}

/// Computes exact cycle count and remaining cooldown in `[0, dt)` without iteration loops.
fn compute_weapon_cycles(rem: f64, dt: f64, rof_s: f64) -> (u32, f64) {
    let rem = rem.max(0.0);
    if rem >= dt {
        (0, (rem - dt).max(0.0))
    } else {
        let after_first = dt - rem;
        let q = after_first / rof_s;
        if (q - q.round()).abs() < 1e-9 && q.round() >= 1.0 {
            (q.round() as u32, 0.0)
        } else {
            let extra = q.floor() as u32;
            let elapsed_since_last = after_first - f64::from(extra) * rof_s;
            (1 + extra, (rof_s - elapsed_since_last).max(0.0))
        }
    }
}

/// Evaluates fitted online Standup weapons for one structure during a tick, requiring real
/// compatible charges (`chargeGroup1`), consuming 1 charge per firing cycle, requiring a
/// completed target lock for directed weapons, and applying EVE missile/turret application math.
#[allow(clippy::too_many_arguments)]
fn resolve_structure_weapon_tick(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    struct_pos: DVec3,
    weapons: &[eve_db::InventoryItemRecord],
    hostile_targets: &[HostileTargetInfo],
    primary_ship_id: i64,
    primary_locked: bool,
    dt: f64,
) -> std::collections::HashMap<i64, DamageVector> {
    let mut ext = load_ext_state(snap);
    let mut damage_by_ship: std::collections::HashMap<i64, DamageVector> =
        std::collections::HashMap::new();
    let mut dirty_ext = false;

    for w in weapons {
        let Some(wtype) = ctx.sde.types.get(&w.type_id) else {
            continue;
        };
        if wtype.category_id != 66 {
            continue;
        }
        let is_pdb = wtype.group_id == 1330;
        if is_pdb {
            if !hostile_targets.iter().any(|t| t.dist_m <= STANDUP_PDB_RANGE_M) {
                continue;
            }
        } else if !primary_locked {
            // Directed Standup weapons cannot fire until target lock acquisition completes.
            continue;
        }

        let charge_group = wtype.default_attributes.get(&604).copied().unwrap_or(0.0) as u32;
        let loaded_charge = if charge_group > 0 {
            let found = snap
                .items
                .values()
                .filter(|c| {
                    c.location_id == structure_id
                        && c.flag == w.flag
                        && c.item_id != w.item_id
                        && c.quantity > 0
                        && ctx
                            .sde
                            .types
                            .get(&c.type_id)
                            .is_some_and(|ct| ct.group_id == charge_group)
                })
                .min_by_key(|c| c.item_id)
                .map(|c| (c.item_id, c.type_id, c.quantity));
            let Some(charge_tuple) = found else {
                // Weapons with `chargeGroup1` require a loaded compatible charge stack.
                continue;
            };
            Some(charge_tuple)
        } else {
            None
        };

        let rof_ms = wtype
            .default_attributes
            .get(&51)
            .or_else(|| wtype.default_attributes.get(&73))
            .copied()
            .unwrap_or(3_000.0)
            .max(500.0);
        let rof_s = rof_ms / 1_000.0;
        let prev_rem = ext.weapon_cooldowns.get(&w.item_id).copied().unwrap_or(0.0);
        let (raw_cycles, new_rem) = compute_weapon_cycles(prev_rem, dt, rof_s);

        let cycles = if let Some((charge_item_id, _, avail_qty)) = loaded_charge {
            let capped = raw_cycles.min(avail_qty.max(0) as u32);
            if capped == 0 {
                if (new_rem - prev_rem).abs() > 1e-12 {
                    ext.weapon_cooldowns.insert(w.item_id, new_rem);
                    dirty_ext = true;
                }
                continue;
            }
            if let Some(c) = snap.items.get_mut(&charge_item_id) {
                c.quantity = c.quantity.saturating_sub(capped as i32);
                c.version += 1;
                if c.quantity <= 0 {
                    snap.items.remove(&charge_item_id);
                }
            }
            ext.weapon_cooldowns.insert(w.item_id, new_rem);
            dirty_ext = true;
            capped
        } else {
            ext.weapon_cooldowns.insert(w.item_id, new_rem);
            dirty_ext = true;
            if raw_cycles == 0 {
                continue;
            }
            raw_cycles
        };

        let dmg_mult = wtype.default_attributes.get(&64).copied().unwrap_or(1.0);
        let (base_em, base_expl, base_kin, base_therm, app_attrs) =
            if let Some((_, charge_tid, _)) = loaded_charge {
                let Some(ctype) = ctx.sde.types.get(&charge_tid) else {
                    continue;
                };
                (
                    ctype.default_attributes.get(&114).copied().unwrap_or(0.0),
                    ctype.default_attributes.get(&116).copied().unwrap_or(0.0),
                    ctype.default_attributes.get(&117).copied().unwrap_or(0.0),
                    ctype.default_attributes.get(&118).copied().unwrap_or(0.0),
                    &ctype.default_attributes,
                )
            } else {
                (
                    wtype.default_attributes.get(&114).copied().unwrap_or(0.0),
                    wtype.default_attributes.get(&116).copied().unwrap_or(0.0),
                    wtype.default_attributes.get(&117).copied().unwrap_or(0.0),
                    wtype.default_attributes.get(&118).copied().unwrap_or(0.0),
                    &wtype.default_attributes,
                )
            };

        let aoe_vel = app_attrs.get(&653).copied().unwrap_or(0.0);
        let aoe_cloud = app_attrs.get(&654).copied().unwrap_or(0.0);
        let drf = app_attrs.get(&1353).copied().unwrap_or(0.944);
        let max_vel = app_attrs.get(&37).copied().unwrap_or(0.0);
        let exp_delay_ms = app_attrs.get(&281).copied().unwrap_or(0.0);
        let turret_tracking = wtype
            .default_attributes
            .get(&160)
            .or_else(|| app_attrs.get(&160))
            .copied()
            .unwrap_or(0.0);
        let sig_res = wtype
            .default_attributes
            .get(&620)
            .or_else(|| app_attrs.get(&620))
            .copied()
            .unwrap_or(40_000.0);
        let opt_range = wtype
            .default_attributes
            .get(&54)
            .or_else(|| app_attrs.get(&54))
            .copied()
            .unwrap_or(0.0);
        let falloff = wtype
            .default_attributes
            .get(&158)
            .or_else(|| app_attrs.get(&158))
            .copied()
            .unwrap_or(0.0);

        for target in hostile_targets {
            if is_pdb {
                if target.dist_m > STANDUP_PDB_RANGE_M {
                    continue;
                }
            } else if target.ship_id != primary_ship_id {
                continue;
            }
            if max_vel > 0.0 && exp_delay_ms > 0.0 {
                let max_flight_m = max_vel * (exp_delay_ms / 1_000.0);
                if target.dist_m > max_flight_m {
                    continue;
                }
            }
            let app_factor = if is_pdb {
                1.0
            } else if aoe_cloud > 0.0 {
                eve_destiny::combat::compute_missile_damage_factor(
                    target.sig_radius,
                    target.velocity.length(),
                    aoe_cloud,
                    aoe_vel.max(1.0),
                    drf,
                )
            } else if turret_tracking > 0.0 && opt_range > 0.0 {
                let ang_vel = eve_destiny::combat::compute_angular_velocity(
                    struct_pos,
                    DVec3::ZERO,
                    target.position,
                    target.velocity,
                );
                eve_destiny::combat::compute_turret_chance_to_hit_with_res(
                    ang_vel,
                    turret_tracking,
                    sig_res.max(1.0),
                    target.sig_radius,
                    target.dist_m,
                    opt_range,
                    falloff,
                )
            } else {
                1.0
            };

            let scale = dmg_mult * f64::from(cycles) * app_factor;
            if scale <= 0.0 {
                continue;
            }
            let entry = damage_by_ship.entry(target.ship_id).or_default();
            entry.em += base_em * scale;
            entry.thermal += base_therm * scale;
            entry.kinetic += base_kin * scale;
            entry.explosive += base_expl * scale;
        }
    }

    if dirty_ext {
        save_ext_state(snap, &ext);
    }
    damage_by_ship
}

/// Steps unpiloted Upwell structure automated target acquisition and Standup weapon fire
/// during system simulation ticks (`feat:structures.defense-settings`).
pub async fn step_structure_auto_defense(ctx: &ServerContext, sys_id: u32, dt: f64) {
    if dt <= 0.0 {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Ok(balls) = sys_handle.list_balls().await else {
        return;
    };
    if balls.is_empty() {
        return;
    }

    let now_unix = ctx.now_unix();
    let candidate_structures: Vec<(i64, DVec3, f64, f64, Vec<eve_db::InventoryItemRecord>)> =
        ctx.db.with_snapshot(|snap| {
            let ext = load_ext_state(snap);
            snap.structures
                .values()
                .filter(|st| {
                    st.solar_system_id == sys_id
                        && is_operational(&st.state)
                        && !ext.active_pilot_by_structure.contains_key(&st.structure_id)
                        && crate::services::structure_acl_defense_rules::load_defense_settings(
                            snap,
                            st.structure_id,
                        )
                        .auto_defense_enabled
                })
                .filter_map(|st| {
                    let st_attrs = ctx.sde.types.get(&st.type_id).map(|t| &t.default_attributes);
                    let max_target_range = st_attrs
                        .and_then(|a| a.get(&76).copied())
                        .unwrap_or(350_000.0);
                    let scan_resolution = st_attrs
                        .and_then(|a| a.get(&564).copied())
                        .unwrap_or(40.0);
                    let mut weapons: Vec<eve_db::InventoryItemRecord> = snap
                        .items
                        .values()
                        .filter(|it| {
                            it.location_id == st.structure_id
                                && (27..=34).contains(&(it.flag as u16))
                                && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
                                && ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.category_id == 66)
                        })
                        .cloned()
                        .collect();
                    if weapons.is_empty() {
                        return None;
                    }
                    weapons.sort_by_key(|w| w.item_id);
                    Some((
                        st.structure_id,
                        DVec3::from_array(st.position),
                        max_target_range,
                        scan_resolution,
                        weapons,
                    ))
                })
                .collect()
        });

    for (structure_id, struct_pos, max_target_range, scan_resolution, weapons) in candidate_structures {
        let raw_candidates: Vec<(i64, i64, DVec3, DVec3, f64)> = ctx.db.with_snapshot(|snap| {
            balls
                .iter()
                .filter(|b| {
                    b.id != structure_id
                        && b.id > 0
                        && !snap.structures.contains_key(&b.id)
                        && !matches!(b.mode, eve_destiny::MovementMode::Warp { .. })
                })
                .filter_map(|b| {
                    let pilot_id = snap
                        .characters
                        .values()
                        .find(|c| c.active_ship_id == b.id && c.station_id.is_none() && c.structure_id.is_none())
                        .map(|c| c.character_id)?;
                    let dist = b.position.distance(struct_pos);
                    if dist > max_target_range {
                        return None;
                    }
                    if evaluate_auto_defense_target(ctx, snap, structure_id, pilot_id) {
                        Some((b.id, pilot_id, b.position, b.velocity, dist))
                    } else {
                        None
                    }
                })
                .collect()
        });

        let mut hostile_targets = Vec::with_capacity(raw_candidates.len());
        for (ship_id, pilot_id, position, velocity, dist_m) in raw_candidates {
            let sig_radius = match sys_handle.get_combat_state(ship_id).await {
                Ok(Some(cs)) => {
                    if cs.is_cloaked || cs.is_warp_exit_invulnerable() {
                        continue;
                    }
                    cs.signature_radius.max(1.0)
                }
                _ => 35.0,
            };
            hostile_targets.push(HostileTargetInfo {
                ship_id,
                pilot_id,
                position,
                velocity,
                sig_radius,
                dist_m,
            });
        }
        hostile_targets.sort_by(|a, b| {
            a.dist_m
                .partial_cmp(&b.dist_m)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.ship_id.cmp(&b.ship_id))
        });

        if !balls.iter().any(|b| b.id == structure_id) {
            crate::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(
                ctx,
                sys_id,
                structure_id,
            )
            .await;
        }
        let existing_locks = sys_handle.get_target_locks(structure_id).await.unwrap_or_default();
        for lock in &existing_locks {
            if !hostile_targets.iter().any(|t| t.ship_id == lock.target_id) {
                let _ = sys_handle.remove_target_lock(structure_id, lock.target_id).await;
            }
        }

        let Some(primary_target) = hostile_targets.first() else {
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = load_ext_state(snap);
                let mut changed = false;
                for w in &weapons {
                    if let Some(rem) = ext.weapon_cooldowns.get_mut(&w.item_id) {
                        *rem = (*rem - dt).max(0.0);
                        if *rem <= 0.0 {
                            ext.weapon_cooldowns.remove(&w.item_id);
                        }
                        changed = true;
                    }
                }
                if changed {
                    save_ext_state(snap, &ext);
                }
            });
            continue;
        };

        let primary_ship_id = primary_target.ship_id;
        let primary_locked = match existing_locks.iter().find(|l| l.target_id == primary_ship_id) {
            Some(lock) => lock.locked,
            None => {
                let lock_time_s = eve_destiny::combat::compute_lock_time_seconds(
                    scan_resolution,
                    primary_target.sig_radius,
                );
                let lock_time_ms = lock_time_s * 1_000.0;
                let _ = sys_handle
                    .add_target_lock(
                        structure_id,
                        TargetLock {
                            target_id: primary_ship_id,
                            lock_time_ms,
                            remaining_lock_ms: lock_time_ms,
                            locked: false,
                            max_range_m: Some(max_target_range),
                        },
                    )
                    .await;
                false
            }
        };

        let damage_by_ship = ctx.db.with_mut_snapshot(|snap| {
            resolve_structure_weapon_tick(
                ctx,
                snap,
                structure_id,
                struct_pos,
                &weapons,
                &hostile_targets,
                primary_ship_id,
                primary_locked,
                dt,
            )
        });

        for target in &hostile_targets {
            let Some(&total_dmg) = damage_by_ship.get(&target.ship_id) else {
                continue;
            };
            if total_dmg.total() > 0.0 {
                if let Ok(Some((_, destroyed, _))) = sys_handle
                    .apply_damage_with_attacker(target.ship_id, total_dmg, Some(structure_id))
                    .await
                {
                    if destroyed {
                        let _ = sys_handle.remove_target_lock(structure_id, target.ship_id).await;
                        let _ = crate::combat::death_pipeline::execute_death_pipeline_sync(
                            ctx,
                            sys_id,
                            target.pilot_id,
                            target.ship_id,
                            0,
                            target.position,
                            now_unix,
                        );
                    }
                }
            }
        }
    }
}

/// Returns `true` when `ship_id` refers to an anchored Upwell structure rather than a ship.
pub fn is_structure_vessel(ctx: &ServerContext, ship_id: i64) -> bool {
    ctx.db.with_snapshot(|snap| snap.structures.contains_key(&ship_id))
}

/// Rejects ship navigation, warp, jump, filament, or docking commands while the session is
/// controlling an anchored Upwell structure (`ship_id` is in `snap.structures`).
pub fn reject_if_controlling_structure(
    ctx: &ServerContext,
    ship_id: i64,
    action: &str,
) -> Result<(), BridgeRpcError> {
    if is_structure_vessel(ctx, ship_id) {
        return Err(err(
            "StructureCannotNavigate",
            format!("Cannot {action} while controlling an anchored Upwell structure ({ship_id})"),
        ));
    }
    Ok(())
}

