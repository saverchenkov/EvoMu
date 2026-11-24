//! `combat_fighters` — Carrier / Structure Fighter Tubes, Launch, Recall, Movement, Abilities, Abandon & Squadron Losses (`S-2`).

#![forbid(unsafe_code)]

use super::*;

fn fighter_class_and_slot_attr(sde: &eve_sde::SdeRegistry, fighter_type_id: u32) -> Option<(u16, u16)> {
    let ty = sde.types.get(&fighter_type_id)?;
    let attrs = &ty.default_attributes;
    for (f_attr, s_attr) in [(2212u16, 2217u16), (2213, 2218), (2214, 2219), (2740, 2737), (2741, 2738), (2742, 2739)] {
        if attrs.get(&f_attr).copied().unwrap_or(0.0) > 0.0 {
            return Some((f_attr, s_attr));
        }
    }
    match ty.group_id {
        1652 => Some((2212, 2217)),
        1537 => Some((2213, 2218)),
        1653 => Some((2214, 2219)),
        4777 => Some((2740, 2737)),
        4778 => Some((2741, 2738)),
        4779 => Some((2742, 2739)),
        _ => None,
    }
}

fn check_fighter_class_slot_limit_in_map(
    active_map: &HashMap<i64, crate::router::context::ActiveFighterSquadron>,
    ctx: &ServerContext,
    ship_id: i64,
    carrier_type_id: u32,
    fighter_type_id: u32,
    extra_squadrons: usize,
) -> Result<(), BridgeRpcError> {
    let Some((f_attr, s_attr)) = fighter_class_and_slot_attr(&ctx.sde, fighter_type_id) else {
        return Ok(());
    };
    let Some(raw_max) = ctx.sde.types.get(&carrier_type_id).and_then(|t| t.default_attributes.get(&s_attr).copied()) else {
        return Ok(());
    };
    let max_slots = raw_max.max(0.0) as usize;
    if max_slots == 0 {
        return Err(BridgeRpcError {
            code: "CannotLaunchFighterNotEnoughTubesAvailableForClass".into(),
            message: "UserError: No fighter launch tubes exist on this hull for that fighter class".into(),
            client_error: None,
        });
    }
    let is_same_class = |tid: u32| fighter_class_and_slot_attr(&ctx.sde, tid).is_some_and(|(fa, _)| fa == f_attr);
    let active_in_class = active_map
        .values()
        .filter(|f| f.parent_ship_id == ship_id && !f.is_abandoned && is_same_class(f.type_id))
        .count();
    if active_in_class + extra_squadrons > max_slots {
        return Err(BridgeRpcError {
            code: "CannotLaunchFighterNotEnoughTubesAvailableForClass".into(),
            message: format!("UserError: Fighter class limit ({max_slots}) exceeded for hull {carrier_type_id}"),
            client_error: None,
        });
    }
    Ok(())
}

fn check_fighter_class_slot_limit(
    ctx: &ServerContext, ship_id: i64, carrier_type_id: u32, fighter_type_id: u32,
    count_loaded_tubes: bool, extra_squadrons: usize, replace_tube_idx: Option<usize>,
) -> Result<(), BridgeRpcError> {
    let Some((f_attr, s_attr)) = fighter_class_and_slot_attr(&ctx.sde, fighter_type_id) else { return Ok(()) };
    let Some(raw_max) = ctx.sde.types.get(&carrier_type_id).and_then(|t| t.default_attributes.get(&s_attr).copied()) else {
        return Ok(());
    };
    let max_slots = raw_max.max(0.0) as usize;
    if max_slots == 0 {
        let code = if count_loaded_tubes { "CannotLoadFighterTubeNoTubesExistForClass" } else { "CannotLaunchFighterNotEnoughTubesAvailableForClass" };
        return Err(BridgeRpcError { code: code.into(), message: "UserError: No fighter launch tubes exist on this hull for that fighter class".into(), client_error: None });
    }
    let is_same_class = |tid: u32| fighter_class_and_slot_attr(&ctx.sde, tid).is_some_and(|(fa, _)| fa == f_attr);
    let active_in_class = ctx.active_fighters.read().unwrap().values().filter(|f| f.parent_ship_id == ship_id && !f.is_abandoned && is_same_class(f.type_id)).count();
    let loaded_in_class = if count_loaded_tubes {
        let loads: Vec<i64> = ctx.fighter_tube_loads.read().unwrap().iter().filter(|&(&(sid, tidx), _)| sid == ship_id && Some(tidx) != replace_tube_idx).map(|(_, &fid)| fid).collect();
        ctx.db.with_snapshot(|snap| loads.into_iter().filter(|fid| snap.items.get(fid).is_some_and(|it| is_same_class(it.type_id))).count())
    } else {
        0
    };
    if active_in_class + loaded_in_class + extra_squadrons > max_slots {
        let code = if count_loaded_tubes { "CannotLoadFighterTubeNoTubesAvailableForClass" } else { "CannotLaunchFighterNotEnoughTubesAvailableForClass" };
        return Err(BridgeRpcError { code: code.into(), message: format!("UserError: Fighter class limit ({max_slots}) exceeded for hull {carrier_type_id}"), client_error: None });
    }
    Ok(())
}

/// Applies incoming weapon/NPC damage to a deployed fighter squadron in space (`feat:drones.fighter-squadron-losses`).
/// Each individual fighter unit in the squadron has unit HP (`attr 2214` / `attr 263` / `attr 9`, default `1,000.0`).
/// Whenever cumulative damage reaches one unit's HP, 1 individual fighter in the squadron is destroyed,
/// reducing `squadron_size` (which proportionally reduces the squadron's ability damage output) and
/// decrementing the backing inventory item's `quantity`. If `squadron_size` reaches 0, the entire squadron
/// ball is removed from space and the backing item is deleted.
pub async fn apply_damage_to_fighter_squadron(
    ctx: &ServerContext,
    solar_system_id: u32,
    fighter_ball_id: i64,
    damage: DamageVector,
) -> (u32, u32, bool) {
    let Some(type_id) = ctx
        .active_fighters
        .read()
        .unwrap()
        .get(&fighter_ball_id)
        .map(|s| s.type_id)
    else {
        return (0, 0, false);
    };
    let unit_hp = ctx
        .sde
        .types
        .get(&type_id)
        .and_then(|t| t.default_attributes.get(&263).copied().or_else(|| t.default_attributes.get(&9).copied()))
        .unwrap_or(1_000.0)
        .max(250.0);
    let raw_dmg = damage.total().max(0.0);
    let units_killed = with_mut_combat_runtime(ctx, |rt| {
        let entry = rt.fighter_unit_damage_accum.entry(fighter_ball_id).or_insert(0.0);
        *entry += raw_dmg;
        let killed = (*entry / unit_hp).floor() as u32;
        if killed > 0 {
            *entry = (*entry - (killed as f64) * unit_hp).max(0.0);
        }
        killed
    });

    let atomic_res = {
        let mut fg = ctx.fighters.write().unwrap();
        if let Some(live) = fg.active_fighters.get_mut(&fighter_ball_id) {
            let before_size = live.squadron_size;
            let lost = units_killed.min(before_size);
            let after_size = before_size.saturating_sub(lost);
            let wiped_out = after_size == 0;
            let item_id = live.item_id;
            let owner_char_id = live.owner_char_id;
            let parent_ship_id = live.parent_ship_id;
            let tube_flag = (159 + live.tube_index) as i64;
            if wiped_out {
                fg.active_fighters.remove(&fighter_ball_id);
                fg.charge_state.retain(|&(bid, _), _| bid != fighter_ball_id);
                fg.cooldown_state.retain(|&(bid, _), _| bid != fighter_ball_id);
            } else if lost > 0 {
                live.squadron_size = after_size;
            }
            Some((lost, after_size, wiped_out, item_id, owner_char_id, parent_ship_id, tube_flag))
        } else {
            None
        }
    };
    let Some((lost, after_size, wiped_out, item_id, owner_char_id, parent_ship_id, tube_flag)) = atomic_res else {
        with_mut_combat_runtime(ctx, |rt| {
            rt.fighter_unit_damage_accum.remove(&fighter_ball_id);
        });
        return (0, 0, false);
    };

    if wiped_out {
        with_mut_combat_runtime(ctx, |rt| {
            rt.fighter_unit_damage_accum.remove(&fighter_ball_id);
            rt.record_destroyed_item_id(fighter_ball_id);
            rt.record_destroyed_item_id(item_id);
        });
    }

    if wiped_out || lost > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            let mut remove_item = false;
            if let Some(it) = snap.items.get_mut(&item_id) {
                if !(it.location_id == parent_ship_id && it.flag == InventoryFlag::FighterBay) {
                    let remaining = it.quantity - (lost as i32);
                    if remaining > 0 {
                        it.quantity = remaining;
                        it.singleton = false;
                        it.version += 1;
                    } else {
                        remove_item = true;
                    }
                }
            }
            if remove_item {
                snap.items.remove(&item_id);
            }
        });
    }

    if wiped_out {
        for slot in 0..3 {
            ctx.remove_projected_ewar_by_module(parent_ship_id, -(fighter_ball_id * 10 + slot));
        }
        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = sys_handle.remove_ball(fighter_ball_id).await;
        crate::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(ctx, fighter_ball_id);
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(owner_char_id),
            crate::router::notify_registry::OnFighterRemovedFromController {
                fighter_id: fighter_ball_id,
                tube_flag_id: tube_flag,
            },
        );
    } else if lost > 0 {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(owner_char_id),
            crate::router::notify_registry::OnInSpaceSquadronSizeChanged {
                fighter_id: fighter_ball_id,
                squadron_size: after_size as i64,
            },
        );
    }

    (lost, after_size, wiped_out)
}

fn fighter_rpc_err(code: &'static str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

fn parse_fighter_id_filter(
    args: &[PyRep],
    allow_zero_tube_index: bool,
) -> Result<Option<Vec<i64>>, BridgeRpcError> {
    let min_id = if allow_zero_tube_index { 0 } else { 1 };
    match args.first() {
        None | Some(PyRep::None) => Ok(None),
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            if items.is_empty() {
                return Err(fighter_rpc_err(
                    "InvalidArgument",
                    "Fighter selector list cannot be empty",
                ));
            }
            let ids: Vec<i64> = items
                .iter()
                .filter_map(|x| x.as_i64())
                .filter(|&id| id >= min_id)
                .collect();
            if ids.is_empty() {
                return Err(fighter_rpc_err(
                    "FighterNotFound",
                    "No valid fighter IDs specified",
                ));
            }
            Ok(Some(ids))
        }
        Some(PyRep::Integer(id)) => {
            if *id >= min_id {
                Ok(Some(vec![*id]))
            } else {
                Err(fighter_rpc_err(
                    "FighterNotFound",
                    "Invalid fighter or tube ID",
                ))
            }
        }
        Some(_) => Err(fighter_rpc_err(
            "InvalidArgument",
            "Expected fighter ID, tube index, or list",
        )),
    }
}

/// Resolves an undocked in-space ship session and ball. Unconditionally rejects station-docked
/// sessions/DB characters with `ShipDocked`, and allows `structure_id` only when the pilot is
/// actively controlling that structure (`structure_id == Some(ship_id)`).
async fn resolve_in_space_fighter_ship(
    ctx: &ServerContext,
    session: &SessionState,
    action_label: &str,
) -> Result<
    (
        i64,
        i64,
        u32,
        crate::system_actor::SystemHandle,
        eve_destiny::Ball,
    ),
    BridgeRpcError,
> {
    let (char_id, sys_id, ship_id) = ctx.require_undocked_space_ship(
        session,
        "ShipDocked",
        &format!("Cannot {action_label} while docked"),
        true,
        true,
    )?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = match sys_handle.get_ball_snapshot(ship_id).await {
        Ok(Some((_, b))) => b,
        Ok(None) => {
            return Err(fighter_rpc_err(
                "ShipNotInSpace",
                "Active ship is not spawned in space",
            ));
        }
        Err(e) => return Err(fighter_rpc_err("DestinyCommandFailed", e)),
    };
    Ok((char_id, ship_id, sys_id, sys_handle, ship_ball))
}

/// Validates that the caller's session is undocked with an active ship ball in space and is not actively cloaked.
async fn resolve_uncloaked_fighter_ship(
    ctx: &ServerContext,
    session: &SessionState,
    action_label: &str,
) -> Result<(i64, i64, u32, crate::system_actor::SystemHandle), BridgeRpcError> {
    let (char_id, ship_id, sys_id, sys_handle, _) =
        resolve_in_space_fighter_ship(ctx, session, action_label).await?;
    if crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
        ctx,
        &sys_handle,
        ship_id,
    )
    .await
    {
        return Err(fighter_rpc_err(
            "CannotActivateCloaked",
            format!("Cannot {action_label} while cloaked"),
        ));
    }
    Ok((char_id, ship_id, sys_id, sys_handle))
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_fighters(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("fighterMgr", "GetFightersForShip", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "LoadFightersToTube", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "UnloadTubeToFighterBay", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "LaunchFightersFromTubes", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "RecallFightersToTubes", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "ExecuteMovementCommandOnFighters", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "CmdActivateAbilitySlots", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "ActivateAbilitySlotsAtPoint", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "CmdDeactivateAbilitySlots", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "CmdAbandonFighter", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fighterMgr", "CmdScoopAbandonedFighterFromSpace", RpcDispatchHandler::Async(dispatch_rpc)),
];

pub async fn handle_fighters(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let res = if service == "fighterMgr" && method == "CmdScoopAbandonedFighterFromSpace" {
        Box::pin(handle_scoop_abandoned_fighter(ctx, session, args)).await
    } else {
        Box::pin(handle_fighters_inner(ctx, session, service, method, args, _kwargs)).await
    };
    let is_read_only = matches!((service, method), ("fighterMgr", "GetFightersForShip"));
    if !is_read_only && matches!(res, Some(Ok(_))) {
        ctx.persist_server_runtime_to_db();
    }
    res
}

async fn handle_fighters_inner(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fighterMgr", "GetFightersForShip") => {
            handle_get_fighters_for_ship(ctx, session, args)
        }
        ("fighterMgr", "LoadFightersToTube") => {
            handle_load_fighters_to_tube(ctx, session, args)
        }
        ("fighterMgr", "UnloadTubeToFighterBay") => {
            handle_unload_tube_to_fighter_bay(ctx, session, args)
        }
        ("fighterMgr", "LaunchFightersFromTubes") => {
            Box::pin(handle_launch_fighters_from_tubes(ctx, session, args)).await
        }
        ("fighterMgr", "RecallFightersToTubes") => {
            Box::pin(handle_recall_fighters_to_tubes(ctx, session, args)).await
        }
        ("fighterMgr", "ExecuteMovementCommandOnFighters") => {
            Box::pin(handle_execute_movement_command_on_fighters(ctx, session, args)).await
        }
        ("fighterMgr", "CmdActivateAbilitySlots") => {
            Box::pin(handle_activate_fighter_ability_slots(ctx, session, args)).await
        }
        ("fighterMgr", "ActivateAbilitySlotsAtPoint") => {
            Box::pin(handle_activate_fighter_ability_slots_on_point(ctx, session, args)).await
        }
        ("fighterMgr", "CmdDeactivateAbilitySlots") => {
            Box::pin(handle_deactivate_fighter_ability_slots(ctx, session, args)).await
        }
        ("fighterMgr", "CmdAbandonFighter") => {
            Box::pin(handle_cmd_abandon_fighter(ctx, session, args)).await
        }
        _ => None,
    }
}

fn handle_get_fighters_for_ship(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        }));
    };
    let authorized = session.ship_id == Some(ship_id)
        || ctx.db.with_snapshot(|snap| {
            let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
            snap.items.get(&ship_id).is_some_and(|i| i.owner_id == char_id)
                || snap
                    .structures
                    .get(&ship_id)
                    .is_some_and(|s| s.owner_corp_id == char_corp)
        });
    if !authorized {
        return Some(Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: format!("Character {char_id} does not control ship or structure {ship_id}"),
            client_error: None,
        }));
    }
    let mut tube_pairs: Vec<(usize, i64)> = ctx
        .fighter_tube_loads
        .read()
        .unwrap()
        .iter()
        .filter(|&(&(sid, _), _)| sid == ship_id)
        .map(|(&(_, tidx), &fid)| (tidx, fid))
        .collect();
    tube_pairs.sort_by_key(|&(tidx, _)| tidx);
    let fighters_in_tubes: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        tube_pairs
            .into_iter()
            .filter_map(|(tidx, fid)| {
                let it = snap.items.get(&fid)?;
                Some(PyRep::Tuple(vec![
                    PyRep::Integer(159 + tidx as i64),
                    PyRep::Integer(fid),
                    PyRep::Integer(it.type_id as i64),
                    PyRep::Integer(it.quantity as i64),
                ]))
            })
            .collect()
    });
    let mut in_space_vec: Vec<_> = ctx
        .active_fighters
        .read()
        .unwrap()
        .values()
        .filter(|f| f.parent_ship_id == ship_id && !f.is_abandoned)
        .cloned()
        .collect();
    in_space_vec.sort_by_key(|f| f.tube_index);
    let fighters_in_space: Vec<PyRep> = in_space_vec
        .into_iter()
        .map(|f| {
            PyRep::Tuple(vec![
                PyRep::Integer(159 + f.tube_index as i64),
                PyRep::Integer(f.item_id),
                PyRep::Integer(f.type_id as i64),
                PyRep::Integer(f.squadron_size as i64),
            ])
        })
        .collect();
    // `shipFighterState._UpdateStateForShip`
    // (`eve/client/script/ui/inflight/squadrons/shipFighterState.pyj:215-248`)
    // unpacks `(fightersInTubes, fightersInSpace, abilityStates)`.
    Some(Ok(PyRep::Tuple(vec![
        PyRep::List(fighters_in_tubes),
        PyRep::List(fighters_in_space),
        PyRep::Dict(Vec::new()),
    ])))
}

fn handle_load_fighters_to_tube(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(ship_id) = session.ship_id else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        }));
    };
    let rpc = RpcArgs::positional(args);
    let a0 = match rpc.req_i64(0, "tubeOrFighterID") {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let a1 = match rpc.req_i64(1, "fighterOrTubeID") {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let (tube_idx, fighter_item_id) = if (0..10).contains(&a0) {
        (a0 as usize, a1)
    } else if (0..10).contains(&a1) {
        (a1 as usize, a0)
    } else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterTube".into(),
            message: "LoadFightersToTube requires a valid tube index".into(),
            client_error: None,
        }));
    };

    let carrier_type_id = ctx.db.with_snapshot(|snap| {
        if let Some(struct_id) = session.structure_id {
            if ship_id == struct_id || !snap.items.contains_key(&ship_id) {
                if let Some(st) = snap.structures.get(&struct_id) {
                    return st.type_id;
                }
            }
        }
        snap.items
            .get(&ship_id)
            .map(|s| s.type_id)
            .or_else(|| snap.structures.get(&ship_id).map(|s| s.type_id))
            .unwrap_or(0)
    });
    let tube_count = ctx
        .sde
        .types
        .get(&carrier_type_id)
        .and_then(|t| t.default_attributes.get(&2216).copied())
        .map(|v| v as usize)
        .unwrap_or(0);
    if !(0..tube_count).contains(&tube_idx) {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterTube".into(),
            message: format!(
                "Tube index {tube_idx} is invalid (ship/structure has {tube_count} fighter tubes)"
            ),
            client_error: None,
        }));
    }

    {
        let tube_loads = ctx.fighter_tube_loads.read().unwrap();
        if tube_loads
            .iter()
            .any(|(&(sid, tidx), &fid)| sid == ship_id && fid == fighter_item_id && tidx != tube_idx)
        {
            return Some(Err(BridgeRpcError {
                code: "FighterAlreadyLoaded".into(),
                message: format!("Fighter item {fighter_item_id} is already loaded in another tube"),
                client_error: None,
            }));
        }
    }
    {
        let active = ctx.active_fighters.read().unwrap();
        if active
            .values()
            .any(|f| f.item_id == fighter_item_id && !f.is_abandoned)
        {
            return Some(Err(BridgeRpcError {
                code: "FighterAlreadyLaunched".into(),
                message: format!("Fighter item {fighter_item_id} is already launched in space"),
                client_error: None,
            }));
        }
        if active
            .values()
            .any(|f| f.parent_ship_id == ship_id && f.tube_index == tube_idx && !f.is_abandoned)
        {
            return Some(Err(BridgeRpcError {
                code: "FighterAlreadyLaunched".into(),
                message: format!("Tube {tube_idx} currently has a squadron deployed in space"),
                client_error: None,
            }));
        }
    }

    let item_opt = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&fighter_item_id).cloned());
    let Some(item) = item_opt else {
        return Some(Err(BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Fighter item {fighter_item_id} not found"),
            client_error: None,
        }));
    };
    if item.owner_id != char_id {
        return Some(Err(BridgeRpcError {
            code: "OwnershipViolation".into(),
            message: format!("Fighter item {fighter_item_id} not owned by pilot"),
            client_error: None,
        }));
    }
    if item.location_id != ship_id || item.flag != InventoryFlag::FighterBay {
        return Some(Err(BridgeRpcError {
            code: "ItemNotInFighterBay".into(),
            message: format!("Fighter item {fighter_item_id} not in pilot's fighter bay"),
            client_error: None,
        }));
    }
    if ctx
        .fighter_tube_loads
        .read()
        .unwrap()
        .iter()
        .any(|(&(sid, tidx), &fid)| sid == ship_id && tidx != tube_idx && fid == fighter_item_id)
    {
        return Some(Err(BridgeRpcError {
            code: "FighterAlreadyLoadedInTube".into(),
            message: format!("Fighter item {fighter_item_id} is already loaded in another fighter tube"),
            client_error: None,
        }));
    }
    let is_fighter_category = ctx
        .sde
        .types
        .get(&item.type_id)
        .map(|t| t.category_id == 87)
        .unwrap_or(false);
    if !is_fighter_category {
        return Some(Err(BridgeRpcError {
            code: "ItemNotAFighter".into(),
            message: format!(
                "Item {fighter_item_id} (type {}) is not a fighter squadron (category 87)",
                item.type_id
            ),
            client_error: None,
        }));
    }
    if let Err(e) = check_fighter_class_slot_limit(
        ctx,
        ship_id,
        carrier_type_id,
        item.type_id,
        true,
        1,
        Some(tube_idx),
    ) {
        return Some(Err(e));
    }

    let max_squadron_size = ctx
        .sde
        .types
        .get(&item.type_id)
        .map(|t| {
            t.default_attributes
                .get(&2215)
                .copied()
                .map(|v| v as u32)
                .unwrap_or(match t.group_id {
                    1537 | 4778 => 3,
                    1653 | 4779 => 6,
                    _ => 9,
                })
        })
        .unwrap_or(9);

    let loaded_fighter_id = if item.quantity > max_squadron_size as i32 {
        let excess_qty = item.quantity - max_squadron_size as i32;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&fighter_item_id) {
                it.quantity = max_squadron_size as i32;
                it.version += 1;
            }
            snap.next_entity_id += 1;
            let rem_id = snap.next_entity_id;
            let rem_item = InventoryItemRecord {
                item_id: rem_id,
                type_id: item.type_id,
                owner_id: item.owner_id,
                location_id: item.location_id,
                flag: item.flag,
                quantity: excess_qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            snap.items.insert(rem_id, rem_item);
            fighter_item_id
        })
    } else {
        fighter_item_id
    };

    ctx.fighter_tube_loads.write().unwrap().insert((ship_id, tube_idx), loaded_fighter_id);
    let loaded_sq_size = item.quantity.min(max_squadron_size as i32) as i64;
    crate::router::notify_registry::push_typed_session_notification(
        session,
        crate::router::notify_registry::OnFighterTubeContentUpdate {
            tube_flag_id: (159 + tube_idx) as i64,
            fighter_id: loaded_fighter_id,
            fighter_type_id: item.type_id as i64,
            squadron_size: loaded_sq_size,
        },
    );
    let ability_slots: Vec<PyRep> = ctx
        .sde
        .fighter_ability_slots_with_info(item.type_id)
        .into_iter()
        .filter_map(|(slot, info)| {
            ctx.sde.fighter_abilities.get(&slot.ability_id).map(|fa| {
                PyRep::keyval([
                    ("slotID", PyRep::Integer(i64::from(slot.slot_index))),
                    ("abilityID", PyRep::Integer(i64::from(fa.ability_id))),
                    ("targetMode", PyRep::String(fa.target_mode.clone())),
                    ("displayNameID", info.display_name_id.map(|id| PyRep::Integer(i64::from(id))).unwrap_or(PyRep::None)),
                    ("abilityNameID", info.ability_name_id.map(|id| PyRep::Integer(i64::from(id))).unwrap_or(PyRep::None)),
                    ("tooltipTextID", info.tooltip_text_id.map(|id| PyRep::Integer(i64::from(id))).unwrap_or(PyRep::None)),
                    ("turretGraphicID", info.turret_graphic_id.map(|id| PyRep::Integer(i64::from(id))).unwrap_or(PyRep::None)),
                ])
            })
        })
        .collect();
    Some(Ok(PyRep::keyval([
        ("tubeIndex", PyRep::Integer(tube_idx as i64)),
        ("fighterID", PyRep::Integer(loaded_fighter_id)),
        ("abilitySlots", PyRep::List(ability_slots)),
        ("success", PyRep::Bool(true)),
    ])))
}

fn handle_unload_tube_to_fighter_bay(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, _char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(ship_id) = session.ship_id else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        }));
    };
    let raw_tube = match RpcArgs::positional(args).req_i64(0, "tubeID") {
        Ok(v) => v,
        Err(e) => {
            return Some(Err(BridgeRpcError {
                code: "InvalidFighterTube".into(),
                message: e.message,
                client_error: None,
            }));
        }
    };
    let carrier_type_id = ctx.db.with_snapshot(|snap| {
        if let Some(struct_id) = session.structure_id {
            if ship_id == struct_id || !snap.items.contains_key(&ship_id) {
                if let Some(st) = snap.structures.get(&struct_id) {
                    return st.type_id;
                }
            }
        }
        snap.items
            .get(&ship_id)
            .map(|s| s.type_id)
            .or_else(|| snap.structures.get(&ship_id).map(|s| s.type_id))
            .unwrap_or(0)
    });
    let tube_count = ctx
        .sde
        .types
        .get(&carrier_type_id)
        .and_then(|t| t.default_attributes.get(&2216).copied())
        .map(|v| v as usize)
        .unwrap_or(0);
    if raw_tube < 0 || raw_tube as usize >= tube_count {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterTube".into(),
            message: format!(
                "Tube index {raw_tube} is invalid (ship/structure has {tube_count} tubes)"
            ),
            client_error: None,
        }));
    }
    let tube_idx = raw_tube as usize;
    {
        let active = ctx.active_fighters.read().unwrap();
        if active
            .values()
            .any(|f| f.parent_ship_id == ship_id && f.tube_index == tube_idx && !f.is_abandoned)
        {
            return Some(Err(BridgeRpcError {
                code: "FighterAlreadyLaunched".into(),
                message: format!("Cannot unload tube {tube_idx} while squadron is deployed in space"),
                client_error: None,
            }));
        }
    }
    let removed = ctx.fighter_tube_loads.write().unwrap().remove(&(ship_id, tube_idx));
    if removed.is_some() {
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterTubeContentEmpty {
                tube_flag_id: (159 + tube_idx) as i64,
            },
        );
    }
    Some(Ok(PyRep::Bool(removed.is_some())))
}

async fn handle_launch_fighters_from_tubes(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, ship_id, sys_id, sys_handle, ship_ball) =
        match resolve_in_space_fighter_ship(ctx, session, "launch fighters").await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let entry_db_state = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.station_id, c.structure_id, c.solar_system_id, c.active_ship_id))
    });
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Some(Err(BridgeRpcError {
            code: "CannotOpenFighterTubesWhileWarping".into(),
            message: "UserError: Cannot launch fighters while ship is in warp".into(),
            client_error: None,
        }));
    }
    let pre_ship_cs = sys_handle.get_combat_state(ship_id).await.ok().flatten();
    let is_cloaked = pre_ship_cs
        .as_ref()
        .is_some_and(|cs| cs.is_cloaked || cs.cloak_velocity_multiplier.is_some())
        || crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
            ctx,
            &sys_handle,
            ship_id,
        )
        .await;
    if is_cloaked {
        return Some(Err(BridgeRpcError {
            code: "CannotOpenFighterTubesWhileCloaked".into(),
            message: "UserError: Cannot launch fighters while cloaked".into(),
            client_error: None,
        }));
    }
    crate::services::space_destiny::space_nav_travel::sync_tether(ctx, char_id, sys_id, ship_id).await;
    if let Ok(Some(ship_cs)) = sys_handle.get_combat_state(ship_id).await {
        if ship_cs.is_tethered {
            return Some(Err(BridgeRpcError {
                code: "DeniedTethered".into(),
                message: "UserError: Cannot launch fighters while tethered to a structure".into(),
                client_error: None,
            }));
        }
    }

    let raw_req: Option<Vec<i64>> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) if !items.is_empty() => {
            Some(items.iter().filter_map(|x| x.as_i64()).collect())
        }
        Some(PyRep::Integer(t)) => Some(vec![*t]),
        _ => None,
    };
    let passed_flag_ids = raw_req.as_ref().map(|v| v.iter().any(|&x| (159..=163).contains(&x))).unwrap_or(false);
    let requested_tubes: Option<Vec<usize>> = raw_req.map(|v| {
        v.into_iter().map(|x| if (159..=163).contains(&x) { (x - 159) as usize } else { x as usize }).collect()
    });

    let carrier_type_id = ctx.db.with_snapshot(|snap| {
        if let Some(struct_id) = session.structure_id {
            if ship_id == struct_id || !snap.items.contains_key(&ship_id) {
                if let Some(st) = snap.structures.get(&struct_id) {
                    return st.type_id;
                }
            }
        }
        snap.items
            .get(&ship_id)
            .map(|s| s.type_id)
            .or_else(|| snap.structures.get(&ship_id).map(|s| s.type_id))
            .unwrap_or(0)
    });
    let tube_count = ctx
        .sde
        .types
        .get(&carrier_type_id)
        .and_then(|t| t.default_attributes.get(&2216).copied())
        .map(|v| v as usize)
        .unwrap_or(0);

    let tubes_to_launch: Vec<usize> = if let Some(req) = requested_tubes {
        for &tidx in &req {
            if !(0..tube_count).contains(&tidx) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFighterTube".into(),
                    message: format!("Tube index {tidx} is invalid (ship/structure has {tube_count} tubes)"),
                    client_error: None,
                }));
            }
            {
                let fighters = ctx.fighters.read().unwrap();
                if fighters
                    .active_fighters
                    .values()
                    .any(|f| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FighterAlreadyLaunched".into(),
                        message: format!("Fighter squadron in tube {tidx} is already launched in space"),
                        client_error: None,
                    }));
                }
                if !fighters.tube_loads.contains_key(&(ship_id, tidx)) {
                    return Some(Err(BridgeRpcError {
                        code: "EmptyFighterTube".into(),
                        message: format!("Fighter tube {tidx} is empty"),
                        client_error: None,
                    }));
                }
            }
            let now_unix = ctx.now_unix();
            let is_rearming = with_combat_runtime(ctx, |rt| {
                rt.fighter_tube_rearm_until.get(&(ship_id, tidx)).copied().unwrap_or(0) > now_unix
            });
            if is_rearming {
                return Some(Err(BridgeRpcError {
                    code: "TubeRearming".into(),
                    message: format!("Fighter tube {tidx} is currently rearming/repairing"),
                    client_error: None,
                }));
            }
        }
        req
    } else {
        let fighters = ctx.fighters.read().unwrap();
        let available: Vec<usize> = (0..tube_count)
            .filter(|&tidx| fighters.tube_loads.contains_key(&(ship_id, tidx)))
            .collect();
        if available.is_empty() {
            if (0..tube_count).any(|tidx| {
                fighters
                    .active_fighters
                    .values()
                    .any(|f| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned)
            }) {
                return Some(Err(BridgeRpcError {
                    code: "FighterAlreadyLaunched".into(),
                    message: "All loaded fighter squadrons are already launched in space".into(),
                    client_error: None,
                }));
            }
            return Some(Err(BridgeRpcError {
                code: "EmptyFighterTube".into(),
                message: "No fighters loaded in tubes".into(),
                client_error: None,
            }));
        }
        available
    };

    if let Ok(Some(ship_cs)) = sys_handle.get_combat_state(ship_id).await {
        if ship_cs.is_tethered {
            let _ = sys_handle
                .modify_combat_state(ship_id, |live_cs| {
                    live_cs.is_tethered = false;
                })
                .await;
        }
    }

    static NEXT_FIGHTER_RES_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-300_000);
    let claimed_tubes: Vec<(usize, i64)> = {
        let mut guard = ctx.fighter_tube_loads.write().unwrap();
        tubes_to_launch
            .iter()
            .filter_map(|&tube_idx| guard.remove(&(ship_id, tube_idx)).map(|fid| (tube_idx, fid)))
            .collect()
    };
    let restore_tubes = |claimed: &[(usize, i64)]| {
        let mut guard = ctx.fighter_tube_loads.write().unwrap();
        for &(tidx, fid) in claimed {
            guard.insert((ship_id, tidx), fid);
        }
    };

    let items_meta: Vec<(usize, i64, u32)> = match ctx.db.with_snapshot(|snap| -> Result<Vec<(usize, i64, u32)>, BridgeRpcError> {
        let mut out = Vec::with_capacity(claimed_tubes.len());
        for &(tube_idx, fighter_item_id) in &claimed_tubes {
            let Some(item) = snap.items.get(&fighter_item_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Fighter item {fighter_item_id} in tube {tube_idx} no longer exists"),
                    client_error: None,
                });
            };
            let tube_flag = InventoryFlag::from_u16(159 + tube_idx as u16);
            if item.owner_id != char_id
                || item.location_id != ship_id
                || (item.flag != InventoryFlag::FighterBay && item.flag != tube_flag)
                || item.quantity <= 0
            {
                return Err(BridgeRpcError {
                    code: "ItemNotInFighterBay".into(),
                    message: format!("Fighter item {fighter_item_id} is no longer owned by pilot in ship's fighter bay"),
                    client_error: None,
                });
            }
            out.push((tube_idx, fighter_item_id, item.type_id));
        }
        Ok(out)
    }) {
        Ok(v) => v,
        Err(e) => {
            restore_tubes(&claimed_tubes);
            return Some(Err(e));
        }
    };

    let res_ids: Vec<(usize, i64)> = {
        let mut active = ctx.active_fighters.write().unwrap();
        let mut inserted = Vec::with_capacity(items_meta.len());
        for &(tube_idx, fid, ftype) in &items_meta {
            if let Err(e) = check_fighter_class_slot_limit_in_map(&active, ctx, ship_id, carrier_type_id, ftype, 1) {
                for &(_, rid) in &inserted {
                    active.remove(&rid);
                }
                drop(active);
                restore_tubes(&claimed_tubes);
                return Some(Err(e));
            }
            let rid = NEXT_FIGHTER_RES_ID.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            active.insert(rid, crate::router::context::ActiveFighterSquadron {
                ball_id: rid,
                item_id: fid,
                type_id: ftype,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                tube_index: tube_idx,
                squadron_size: 1,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            });
            inserted.push((tube_idx, rid));
        }
        inserted
    };
    let rollback_launch_claims = || {
        {
            let mut active = ctx.active_fighters.write().unwrap();
            for &(_, rid) in &res_ids {
                active.remove(&rid);
            }
        }
        restore_tubes(&claimed_tubes);
    };

    let db_committed = ctx.db.try_with_mut_snapshot(|snap| -> Result<Vec<(usize, i64, i64, u32, u32, [f64; 3])>, BridgeRpcError> {
        if let Some(ch) = snap.characters.get(&char_id) {
            let cur_db_state = (ch.station_id, ch.structure_id, ch.solar_system_id, ch.active_ship_id);
            if entry_db_state.is_some_and(|e| cur_db_state != e) {
                return Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is no longer undocked in solar system {sys_id}"),
                    client_error: None,
                });
            }
        }
        let mut committed = Vec::with_capacity(claimed_tubes.len());
        for &(tube_idx, fighter_item_id) in &claimed_tubes {
            let item_type_id = snap
                .items
                .get(&fighter_item_id)
                .ok_or_else(|| BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Fighter item {fighter_item_id} in tube {tube_idx} no longer exists"),
                    client_error: None,
                })?
                .type_id;
            let max_squadron_size = ctx
                .sde
                .types
                .get(&item_type_id)
                .map(|t| {
                    t.default_attributes
                        .get(&2215)
                        .copied()
                        .map(|v| v as u32)
                        .unwrap_or(match t.group_id {
                            1537 | 4778 => 3,
                            1653 | 4779 => 6,
                            _ => 9,
                        })
                })
                .unwrap_or(9);
            let cap = max_squadron_size.max(1) as i32;
            let tube_flag = InventoryFlag::from_u16(159 + tube_idx as u16);

            let (launched_units, rem_excess) = {
                let it = snap.items.get_mut(&fighter_item_id).unwrap();
                if it.owner_id != char_id
                    || it.location_id != ship_id
                    || (it.flag != InventoryFlag::FighterBay && it.flag != tube_flag)
                    || it.quantity <= 0
                {
                    return Err(BridgeRpcError {
                        code: "ItemNotInFighterBay".into(),
                        message: format!("Fighter item {fighter_item_id} is no longer owned by pilot in ship's fighter bay"),
                        client_error: None,
                    });
                }
                let rem = if it.quantity > cap {
                    let excess = it.quantity - cap;
                    let excess_rec = InventoryItemRecord {
                        item_id: 0,
                        type_id: it.type_id,
                        owner_id: it.owner_id,
                        location_id: ship_id,
                        flag: InventoryFlag::FighterBay,
                        quantity: excess,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    };
                    it.quantity = cap;
                    Some(excess_rec)
                } else {
                    None
                };
                let units = (it.quantity.max(1) as u32).min(max_squadron_size);
                it.location_id = sys_id as i64;
                it.flag = eve_db::InventoryFlag::None;
                it.version += 1;
                (units, rem)
            };
            if let Some(mut rem_item) = rem_excess {
                snap.next_entity_id += 1;
                rem_item.item_id = snap.next_entity_id;
                snap.items.insert(rem_item.item_id, rem_item);
            }
            snap.next_entity_id += 1;
            let ball_id = snap.next_entity_id;
            let offset = DVec3::new((tube_idx as f64 + 1.0) * 50.0, 0.0, 0.0);
            let ball_pos = [
                ship_ball.position.x + offset.x,
                ship_ball.position.y + offset.y,
                ship_ball.position.z + offset.z,
            ];
            crate::sp6_lifecycle::record_drone_ball_meta_in_snap(snap, fighter_item_id, ball_id);
            snap.item_positions.insert(fighter_item_id, ball_pos);
            snap.item_positions.insert(ball_id, ball_pos);
            committed.push((tube_idx, fighter_item_id, ball_id, item_type_id, launched_units, ball_pos));
        }
        Ok(committed)
    });
    let committed_launches = match db_committed {
        Ok(v) => v,
        Err(e) => {
            rollback_launch_claims();
            return Some(Err(e));
        }
    };

    {
        let mut active = ctx.active_fighters.write().unwrap();
        for &(_, rid) in &res_ids {
            active.remove(&rid);
        }
        for &(tube_idx, fighter_item_id, ball_id, item_type_id, launched_units, _) in &committed_launches {
            active.insert(
                ball_id,
                crate::router::context::ActiveFighterSquadron {
                    ball_id,
                    item_id: fighter_item_id,
                    type_id: item_type_id,
                    owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    tube_index: tube_idx,
                    squadron_size: launched_units,
                    target_id: None,
                    is_abandoned: false,
                    mwd_active: false,
                },
            );
        }
    }

    let mut launched_ball_ids = Vec::new();
    for (tube_idx, _fighter_item_id, ball_id, item_type_id, launched_units, ball_pos) in committed_launches {
        let ball = eve_destiny::Ball {
            id: ball_id,
            type_id: item_type_id,
            position: DVec3::new(ball_pos[0], ball_pos[1], ball_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 2_500.0,
            inertia: 0.5,
            mass: 25_000.0,
            radius: 20.0,
            speed_fraction: 1.0,
            mode: eve_destiny::MovementMode::Orbit {
                target_id: ship_id,
                range: 1_000.0,
            },
        };
        let mut cs = ctx.build_type_combat_state(item_type_id);
        let squad_size = launched_units as f64;
        cs.shield_hp *= squad_size;
        cs.max_shield_hp *= squad_size;
        cs.armor_hp *= squad_size;
        cs.max_armor_hp *= squad_size;
        cs.structure_hp *= squad_size;
        cs.max_structure_hp *= squad_size;
        let (pilot_corp_id, pilot_alliance_id) = ctx
            .db
            .get_character(char_id)
            .map(|ch| (ch.corporation_id, ch.alliance_id.unwrap_or(-1)))
            .unwrap_or((
                session.corporation_id.unwrap_or(-1),
                session.alliance_id.unwrap_or(-1),
            ));
        let f_type = ctx.sde.types.get(&item_type_id);
        let fighter_meta = eve_destiny::SlimItemMeta {
            group_id: f_type.map(|t| t.group_id).unwrap_or(1652),
            category_id: f_type.map(|t| t.category_id).unwrap_or(87),
            owner_id: char_id,
            char_id: None,
            corp_id: pilot_corp_id,
            alliance_id: pilot_alliance_id,
            security_status: 0.0,
            is_cloaked: false,
            name: f_type.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
            ..Default::default()
        };
        let _ = sys_handle.spawn_ball_with_meta(ball, cs, fighter_meta).await;

        let tube_flag = (159 + tube_idx) as i64;
        let now_ft = ctx.now_client_sim_filetime_ticks();
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterTubeTaskStatus {
                tube_flag_id: tube_flag,
                status_id: "INSPACE".into(),
                status_start_time: now_ft,
                status_end_time: now_ft,
                user_error: None,
            },
        );
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterTubeContentEmpty {
                tube_flag_id: tube_flag,
            },
        );
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterAddedToController {
                fighter_id: ball_id,
                fighter_type_id: item_type_id as i64,
                tube_flag_id: tube_flag,
                squadron_size: launched_units as i64,
                ability_slot_states: PyRep::List(vec![]),
            },
        );
        launched_ball_ids.push(PyRep::Integer(ball_id));
    }
    if passed_flag_ids {
        Some(Ok(PyRep::Dict(vec![])))
    } else {
        Some(Ok(PyRep::List(launched_ball_ids)))
    }
}

async fn handle_recall_fighters_to_tubes(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, ship_id, sys_id, sys_handle) =
        match resolve_uncloaked_fighter_ship(ctx, session, "recall fighters").await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let entry_db_state = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.station_id, c.structure_id, c.solar_system_id, c.active_ship_id))
    });
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        let message = "Active ship is not spawned in space".to_string();
        return Some(Err(BridgeRpcError::with_client_error(
            "ShipNotInSpace",
            message.clone(),
            eve_proto::ClientError::CustomInfo(message),
        )));
    };
    let filter_ids = match parse_fighter_id_filter(args, true) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let candidate_ids: Vec<(i64, u32)> = ctx
        .active_fighters
        .read()
        .unwrap()
        .values()
        .filter(|f| {
            f.parent_ship_id == ship_id
                && f.owner_char_id == char_id
                && !f.is_abandoned
                && filter_ids
                    .as_ref()
                    .map(|ids| {
                        ids.contains(&(f.tube_index as i64))
                            || ids.contains(&(159 + f.tube_index as i64))
                            || ids.contains(&f.ball_id)
                            || ids.contains(&f.item_id)
                    })
                    .unwrap_or(true)
        })
        .map(|f| (f.ball_id, f.type_id))
        .collect();
    if candidate_ids.is_empty() {
        let all_already_recalled_own = filter_ids.as_ref().is_some_and(|ids| {
            ctx.db.with_snapshot(|snap| {
                ids.iter().all(|&id| {
                    snap.sp6_lifecycle
                        .space_object_meta
                        .iter()
                        .any(|(item_id, meta)| {
                            meta.drone_ball_id == Some(id)
                                && snap.items.get(item_id).is_some_and(|it| {
                                    it.owner_id == char_id && it.location_id == ship_id
                                })
                        })
                })
            })
        });
        if all_already_recalled_own {
            return Some(Ok(PyRep::List(Vec::new())));
        }
        return Some(Err(fighter_rpc_err(
            if filter_ids.is_some() {
                "FighterNotFound"
            } else {
                "NoActiveFighters"
            },
            "No matching active fighter squadron found to recall",
        )));
    }

    const FIGHTER_RECALL_SURFACE_RANGE_M: f64 = 2_500.0;
    let mut prechecked = Vec::with_capacity(candidate_ids.len());
    for (bid, type_id) in candidate_ids {
        let Some((_, f_ball)) = sys_handle.get_ball_snapshot(bid).await.ok().flatten() else {
            continue;
        };
        let dist = ship_ball.surface_distance(&f_ball);
        if dist > FIGHTER_RECALL_SURFACE_RANGE_M {
            let message = format!(
                "Fighter squadron {bid} is {dist:.1}m from ship hull, must be within 2,500m to recall"
            );
            return Some(Err(BridgeRpcError::with_client_error(
                "FighterOutOfRange",
                message.clone(),
                eve_proto::ClientError::CustomInfo(message),
            )));
        }
        prechecked.push((bid, type_id));
    }

    let mut recalled = Vec::new();
    for (bid, _) in prechecked {
        let Some((_, cur_ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
            let message = "Active ship is not spawned in space".to_string();
            return Some(Err(BridgeRpcError::with_client_error(
                "ShipNotInSpace",
                message.clone(),
                eve_proto::ClientError::CustomInfo(message),
            )));
        };
        let Some((_, cur_f_ball)) = sys_handle.get_ball_snapshot(bid).await.ok().flatten() else {
            continue;
        };
        let cur_dist = cur_ship_ball.surface_distance(&cur_f_ball);
        if cur_dist > FIGHTER_RECALL_SURFACE_RANGE_M {
            let message = format!(
                "Fighter squadron {bid} is {cur_dist:.1}m from ship hull, must be within 2,500m to recall"
            );
            return Some(Err(BridgeRpcError::with_client_error(
                "FighterOutOfRange",
                message.clone(),
                eve_proto::ClientError::CustomInfo(message),
            )));
        }
        let missing_cs_hp = sys_handle
            .get_combat_state(bid)
            .await
            .ok()
            .flatten()
            .map(|cs| {
                (cs.max_shield_hp - cs.shield_hp).max(0.0)
                    + (cs.max_armor_hp - cs.armor_hp).max(0.0)
                    + (cs.max_structure_hp - cs.structure_hp).max(0.0)
            })
            .unwrap_or(0.0);

        let claimed = {
            let mut fg = ctx.fighters.write().unwrap();
            let valid = fg.active_fighters.get(&bid).is_some_and(|cur| {
                cur.parent_ship_id == ship_id
                    && cur.owner_char_id == char_id
                    && !cur.is_abandoned
                    && cur.squadron_size > 0
            });
            if valid {
                let f = fg.active_fighters.remove(&bid).unwrap();
                let charge_reload_s: f64 = ctx
                    .sde
                    .fighter_abilities_by_type
                    .get(&f.type_id)
                    .map(|slots| {
                        slots
                            .iter()
                            .filter_map(|s| {
                                let ch = s.charges.as_ref()?;
                                let used = fg.charge_state.get(&(bid, s.slot_index as u32)).copied().unwrap_or(0);
                                let max_c = ch.charge_count.max(1) as f64;
                                (used > 0).then_some(((used as f64) / max_c) * (ch.rearm_time_seconds.max(1.0) * 0.45))
                            })
                            .sum()
                    })
                    .unwrap_or(0.0);
                let saved_charges: Vec<_> = fg
                    .charge_state
                    .iter()
                    .filter(|(&(id, _), _)| id == bid)
                    .map(|(&k, &v)| (k, v))
                    .collect();
                let saved_cooldowns: Vec<_> = fg
                    .cooldown_state
                    .iter()
                    .filter(|(&(id, _), _)| id == bid)
                    .map(|(&k, &v)| (k, v))
                    .collect();
                fg.charge_state.retain(|&(id, _), _| id != bid);
                fg.cooldown_state.retain(|&(id, _), _| id != bid);
                let loaded_tube_ids: std::collections::HashSet<i64> = fg.tube_loads.values().copied().collect();
                Some((f, charge_reload_s, loaded_tube_ids, saved_charges, saved_cooldowns))
            } else {
                None
            }
        };
        let Some((mut f, charge_reload_s, loaded_tube_ids, saved_charges, saved_cooldowns)) = claimed else {
            continue;
        };

        for slot in 0..3 {
            ctx.remove_projected_ewar_by_module(ship_id, -(bid * 10 + slot));
        }
        let accum_dmg = with_mut_combat_runtime(ctx, |rt| {
            rt.fighter_unit_damage_accum.remove(&bid).unwrap_or(0.0)
        });

        let f_sde = ctx.sde.types.get(&f.type_id);
        let base_refuel_s = f_sde
            .and_then(|t| t.default_attributes.get(&2426).copied())
            .map(|ms| (ms / 1_000.0).clamp(2.0, 10.0))
            .unwrap_or(3.0);
        let unit_hp = f_sde
            .and_then(|t| t.default_attributes.get(&263).copied().or_else(|| t.default_attributes.get(&9).copied()))
            .unwrap_or(1_000.0)
            .max(250.0);
        let repair_s = (((accum_dmg + missing_cs_hp) / unit_hp).clamp(0.0, 4.0)) * 1.5;

        let max_squadron_size = f_sde
            .map(|t| {
                t.default_attributes.get(&2215).copied().map(|v| v as i32).unwrap_or(match t.group_id {
                    1537 | 4778 => 3,
                    1653 | 4779 => 6,
                    _ => 9,
                })
            })
            .unwrap_or(9);
        let db_res = ctx.db.try_with_mut_snapshot(|snap| -> Result<i32, BridgeRpcError> {
            let char_invalid = snap.characters.get(&char_id).is_some_and(|ch| {
                let cur_db_state = (ch.station_id, ch.structure_id, ch.solar_system_id, ch.active_ship_id);
                entry_db_state.is_some_and(|e| cur_db_state != e)
                    || ch.station_id.is_some()
                    || ch.structure_id.is_some()
                    || ch.solar_system_id != sys_id
                    || ch.active_ship_id != ship_id
            });
            let ship_moved = snap.items.get(&ship_id).is_some_and(|sh| {
                sh.location_id != sys_id as i64 || sh.flag != InventoryFlag::None
            });
            if char_invalid || ship_moved {
                let msg = format!("Ship {ship_id} is no longer undocked in solar system {sys_id}");
                return Err(BridgeRpcError::with_client_error(
                    "ShipNotInSpace",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            let mut refilled_units = 0i32;
            if let Some(main_it) = snap.items.get_mut(&f.item_id) {
                main_it.location_id = ship_id;
                main_it.flag = InventoryFlag::FighterBay;
                main_it.quantity = f.squadron_size as i32;
                main_it.version += 1;
            }
            snap.item_positions.remove(&f.item_id);
            let mut needed = (max_squadron_size - f.squadron_size as i32).max(0);
            if needed > 0 {
                let reserve_ids: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.location_id == ship_id && i.flag == InventoryFlag::FighterBay && i.owner_id == char_id
                            && i.type_id == f.type_id && i.item_id != f.item_id && !loaded_tube_ids.contains(&i.item_id)
                            && !snap.is_item_locked_in_contract_or_job(i.item_id) && i.quantity > 0
                    })
                    .map(|i| i.item_id)
                    .collect();
                for rid in reserve_ids {
                    if needed <= 0 { break; }
                    if let Some(res_it) = snap.items.get_mut(&rid) {
                        let take = res_it.quantity.min(needed);
                        res_it.quantity -= take;
                        needed -= take;
                        refilled_units += take;
                        if res_it.quantity <= 0 { snap.items.remove(&rid); }
                    }
                }
                if refilled_units > 0 {
                    if let Some(main_it) = snap.items.get_mut(&f.item_id) {
                        main_it.quantity += refilled_units;
                        main_it.version += 1;
                    }
                }
            }
            Ok(refilled_units)
        });
        let refilled_units = match db_res {
            Ok(units) => units,
            Err(e) => {
                let destroyed = with_mut_combat_runtime(ctx, |rt| {
                    let dead = rt.is_recently_destroyed_item_id(f.ball_id)
                        || rt.is_recently_destroyed_item_id(f.item_id);
                    if !dead && accum_dmg > 0.0 {
                        rt.fighter_unit_damage_accum.insert(bid, accum_dmg);
                    }
                    dead
                });
                if let Ok(mut fg) = ctx.fighters.write() {
                    fg.tube_loads.remove(&(ship_id, f.tube_index));
                    if !destroyed {
                        f.is_abandoned = true;
                        f.parent_ship_id = 0;
                        f.target_id = None;
                        fg.active_fighters.entry(f.ball_id).or_insert(f);
                        for (k, v) in saved_charges {
                            fg.charge_state.insert(k, v);
                        }
                        for (k, v) in saved_cooldowns {
                            fg.cooldown_state.insert(k, v);
                        }
                    }
                }
                sys_handle.try_stop_ball(bid);
                return Some(Err(e));
            }
        };

        let total_rearm_secs = (base_refuel_s + charge_reload_s + repair_s + (refilled_units as f64) * 2.5)
            .ceil()
            .clamp(3.0, 20.0) as u64;
        ctx.fighter_tube_loads.write().unwrap().entry((ship_id, f.tube_index)).or_insert(f.item_id);
        let now_unix = ctx.now_unix();
        with_mut_combat_runtime(ctx, |rt| {
            rt.fighter_tube_rearm_until.insert((ship_id, f.tube_index), now_unix + total_rearm_secs);
        });
        let tube_flag = (159 + f.tube_index) as i64;
        let recalled_qty = ctx.db.with_snapshot(|s| s.items.get(&f.item_id).map(|i| i.quantity).unwrap_or(f.squadron_size as i32)) as i64;
        let now_ft = ctx.now_client_sim_filetime_ticks();
        let end_ft = now_ft + (total_rearm_secs as i64) * 10_000_000;
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterRemovedFromController {
                fighter_id: f.ball_id,
                tube_flag_id: tube_flag,
            },
        );
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterTubeContentUpdate {
                tube_flag_id: tube_flag,
                fighter_id: f.item_id,
                fighter_type_id: f.type_id as i64,
                squadron_size: recalled_qty,
            },
        );
        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnFighterTubeTaskStatus {
                tube_flag_id: tube_flag,
                status_id: "REFUELING".into(),
                status_start_time: now_ft,
                status_end_time: end_ft,
                user_error: None,
            },
        );
        let _ = sys_handle.remove_ball(f.ball_id).await;
        recalled.push(PyRep::Integer(f.ball_id));
    }
    Some(Ok(PyRep::List(recalled)))
}

async fn handle_execute_movement_command_on_fighters(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, _ship_id, _sys_id, sys_handle) =
        match resolve_uncloaked_fighter_ship(ctx, session, "command fighters").await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let fighter_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            items.iter().filter_map(|x| x.as_i64()).collect()
        }
        Some(a) => a.as_i64().into_iter().collect(),
        None => Vec::new(),
    };
    if fighter_ids.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "ExecuteMovementCommandOnFighters requires fighterIDs".into(),
            client_error: None,
        }));
    }
    {
        let fighters = ctx.active_fighters.read().unwrap();
        for &fid in &fighter_ids {
            let Some(f) = fighters.get(&fid) else {
                let message = format!("Fighter squadron {fid} not found");
                return Some(Err(BridgeRpcError::with_client_error(
                    "FighterNotFound",
                    message.clone(),
                    eve_proto::ClientError::CustomInfo(message),
                )));
            };
            if f.owner_char_id != char_id || f.is_abandoned {
                let message = format!("Fighter squadron {fid} is not controlled by pilot");
                return Some(Err(BridgeRpcError::with_client_error(
                    "FighterNotOwned",
                    message.clone(),
                    eve_proto::ClientError::CustomInfo(message),
                )));
            }
        }
    }
    let cmd = args
        .get(1)
        .and_then(|a| a.as_str())
        .unwrap_or("GOTO_POINT")
        .to_ascii_uppercase();
    if cmd == "MWD" {
        if let Err(err) = crate::services::r6_t1_p1_helpers::validate_and_activate_fighter_mwd(
            ctx,
            char_id,
            &sys_handle,
            &fighter_ids,
        )
        .await
        {
            return Some(Err(err));
        }
        return Some(Ok(PyRep::Bool(true)));
    }
    if cmd == "MJD" {
        let now_unix = ctx.now_unix();
        let reserved_batch: Result<Vec<(i64, u32, f64, Option<u64>)>, BridgeRpcError> = (|| {
            let mut fg = ctx.fighters.write().unwrap();
            let mut seen = std::collections::HashSet::with_capacity(fighter_ids.len());
            let mut targets = Vec::with_capacity(fighter_ids.len());
            for &fid in &fighter_ids {
                if !seen.insert(fid) {
                    continue;
                }
                let Some(f) = fg.active_fighters.get(&fid) else {
                    continue;
                };
                if f.owner_char_id != char_id || f.is_abandoned {
                    continue;
                }
                let ftype_id = f.type_id;
                let mjd_slot_opt = ctx
                    .sde
                    .fighter_abilities_by_type
                    .get(&ftype_id)
                    .and_then(|slots| slots.iter().find(|s| s.ability_id == 5));
                let mjd_dist_attr = ctx
                    .sde
                    .types
                    .get(&ftype_id)
                    .and_then(|t| t.default_attributes.get(&2154).copied());
                if mjd_slot_opt.is_none() && mjd_dist_attr.is_none() {
                    let message = format!(
                        "Fighter squadron {fid} (type {ftype_id}) does not possess a Micro Jump Drive ability"
                    );
                    return Err(BridgeRpcError::with_client_error(
                        "FighterLacksMjdAbility",
                        message.clone(),
                        eve_proto::ClientError::CustomInfo(message),
                    ));
                }
                let slot_idx = mjd_slot_opt.map(|s| s.slot_index as u32).unwrap_or(1);
                if let Some(&ready_at) = fg.cooldown_state.get(&(fid, slot_idx)) {
                    if now_unix < ready_at {
                        let message = format!(
                            "Fighter MJD ability on squadron {fid} is on cooldown until {ready_at}"
                        );
                        return Err(BridgeRpcError::with_client_error(
                            "AbilityOnCooldown",
                            message.clone(),
                            eve_proto::ClientError::CustomInfo(message),
                        ));
                    }
                }
                let mjd_dist = mjd_dist_attr.unwrap_or(100_000.0);
                let cd_secs = mjd_slot_opt
                    .and_then(|s| s.cooldown_seconds)
                    .map(|c| c.ceil() as u64)
                    .unwrap_or(60)
                    .max(1);
                targets.push((fid, slot_idx, mjd_dist, cd_secs));
            }
            let mut committed = Vec::with_capacity(targets.len());
            for (fid, slot_idx, mjd_dist, cd_secs) in targets {
                let prev_cd = fg.cooldown_state.insert((fid, slot_idx), now_unix + cd_secs);
                committed.push((fid, slot_idx, mjd_dist, prev_cd));
            }
            Ok(committed)
        })();
        let committed = match reserved_batch {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => {
                let first_id = fighter_ids[0];
                let message = format!("Fighter squadron {first_id} not found");
                return Some(Err(BridgeRpcError::with_client_error(
                    "FighterNotFound",
                    message.clone(),
                    eve_proto::ClientError::CustomInfo(message),
                )));
            }
            Err(e) => return Some(Err(e)),
        };
        let mut first_err: Option<BridgeRpcError> = None;
        let mut jumped_any = false;
        for (fid, slot_idx, mjd_dist, prev_cd) in committed {
            match sys_handle.micro_jump(fid, mjd_dist).await {
                Ok(_) => {
                    jumped_any = true;
                }
                Err(err_msg) => {
                    let mut fg = ctx.fighters.write().unwrap();
                    if fg.active_fighters.contains_key(&fid) {
                        if let Some(old) = prev_cd {
                            fg.cooldown_state.insert((fid, slot_idx), old);
                        } else {
                            fg.cooldown_state.remove(&(fid, slot_idx));
                        }
                    } else {
                        fg.cooldown_state.remove(&(fid, slot_idx));
                    }
                    if first_err.is_none() {
                        let message = format!("Fighter micro jump failed for {fid}: {err_msg}");
                        first_err = Some(BridgeRpcError::with_client_error(
                            "MicroJumpFailed",
                            message.clone(),
                            eve_proto::ClientError::CustomInfo(message),
                        ));
                    }
                }
            }
        }
        if !jumped_any {
            if let Some(err) = first_err {
                return Some(Err(err));
            }
        }
        return Some(Ok(PyRep::Bool(true)));
    }
    let rpc = RpcArgs::positional(args);
    let mode = match cmd.as_str() {
        "ORBIT" => {
            let target_id = match rpc.req_positive_i64(2, "targetID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let range = match rpc.opt_non_negative_f64(3, "range") {
                Ok(v) => v.unwrap_or(1_000.0),
                Err(e) => return Some(Err(e)),
            };
            eve_destiny::MovementMode::Orbit { target_id, range }
        }
        "FOLLOW" | "APPROACH" => {
            let target_id = match rpc.req_positive_i64(2, "targetID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let range = match rpc.opt_non_negative_f64(3, "range") {
                Ok(v) => v.unwrap_or(500.0),
                Err(e) => return Some(Err(e)),
            };
            eve_destiny::MovementMode::Follow { target_id, range }
        }
        "STOP" => eve_destiny::MovementMode::Stop,
        _ => {
            let dir = match rpc
                .opt_finite_vec3(2)
                .transpose()
                .or_else(|| rpc.opt_finite_vec3(1).transpose())
            {
                Some(Ok(v)) => v,
                Some(Err(e)) => return Some(Err(e)),
                None => DVec3::new(1.0, 0.0, 0.0),
            };
            eve_destiny::MovementMode::GotoDirection {
                direction: dir,
            }
        }
    };
    for fid in fighter_ids {
        let still_owned = ctx
            .active_fighters
            .read()
            .unwrap()
            .get(&fid)
            .is_some_and(|f| f.owner_char_id == char_id && !f.is_abandoned);
        if still_owned {
            let _ = sys_handle.set_movement_mode(fid, mode.clone()).await;
        }
    }
    Some(Ok(PyRep::Bool(true)))
}

async fn handle_cmd_abandon_fighter(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, ship_id, sys_id, sys_handle) =
        match resolve_uncloaked_fighter_ship(ctx, session, "abandon fighters").await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let filter_ids = match parse_fighter_id_filter(args, false) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mut abandoned = Vec::new();
    let mut abandoned_item_ids = Vec::new();
    let mut foreign_active_match = false;
    {
        let mut fighters = ctx.active_fighters.write().unwrap();
        for f in fighters.values_mut() {
            let matches_filter = filter_ids
                .as_ref()
                .map(|ids| ids.contains(&f.ball_id) || ids.contains(&f.item_id))
                .unwrap_or(true);
            if !matches_filter {
                continue;
            }
            if f.parent_ship_id == ship_id && f.owner_char_id == char_id && !f.is_abandoned
            {
                f.is_abandoned = true;
                f.parent_ship_id = 0;
                f.target_id = None;
                abandoned.push(f.ball_id);
                abandoned_item_ids.push(f.item_id);
            } else if filter_ids.is_some()
                && !f.is_abandoned
                && f.owner_char_id != char_id
            {
                foreign_active_match = true;
            }
        }
    }
    if abandoned.is_empty() {
        if foreign_active_match {
            return Some(Ok(PyRep::List(Vec::new())));
        }
        return Some(Err(fighter_rpc_err(
            if filter_ids.is_some() {
                "FighterNotFound"
            } else {
                "NoActiveFighters"
            },
            "No matching active fighter squadron found to abandon",
        )));
    }
    if !abandoned_item_ids.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            for iid in &abandoned_item_ids {
                if let Some(item) = snap.items.get_mut(iid) {
                    item.location_id = sys_id as i64;
                    item.flag = InventoryFlag::None;
                    item.version += 1;
                }
            }
        });
    }
    for &bid in &abandoned {
        for slot in 0..3 {
            let _ = sys_handle.remove_module_cycle(bid, -(bid * 10 + slot)).await;
        }
        sys_handle.try_stop_ball(bid);
        let _ = sys_handle
            .set_movement_mode(bid, eve_destiny::MovementMode::Stop)
            .await;
    }
    Some(Ok(PyRep::List(
        abandoned.into_iter().map(PyRep::Integer).collect(),
    )))
}

async fn handle_scoop_abandoned_fighter(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, ship_id, sys_id, sys_handle) =
        match resolve_uncloaked_fighter_ship(ctx, session, "scoop abandoned fighters").await
        {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let entry_db_state = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.station_id, c.structure_id, c.solar_system_id, c.active_ship_id))
    });
    let target_fighter_id = match args.first().and_then(|a| a.as_i64()) {
        Some(id) if id > 0 => id,
        _ => {
            return Some(Err(BridgeRpcError {
                code: "InvalidFighterID".into(),
                message: "CmdScoopAbandonedFighterFromSpace requires a valid target fighter squadron ID".into(),
                client_error: None,
            }));
        }
    };
    let (key, owner_char_id) = {
        let fighters = ctx.active_fighters.read().unwrap();
        let key = if fighters.contains_key(&target_fighter_id) {
            target_fighter_id
        } else if let Some((&k, _)) = fighters.iter().find(|(_, f)| f.item_id == target_fighter_id) {
            k
        } else {
            return Some(Err(BridgeRpcError {
                code: "FighterNotFound".into(),
                message: format!("Fighter squadron {target_fighter_id} not found in space"),
                client_error: None,
            }));
        };
        let squad_ref = &fighters[&key];
        if !squad_ref.is_abandoned {
            return Some(Err(BridgeRpcError {
                code: "FighterNotAbandoned".into(),
                message: format!("Fighter squadron {target_fighter_id} is active and cannot be scooped"),
                client_error: None,
            }));
        }
        (key, squad_ref.owner_char_id)
    };
    if owner_char_id != char_id {
        let authorized = ctx.db.with_snapshot(|snap| {
            let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
            let owner_corp = snap.characters.get(&owner_char_id).map(|c| c.corporation_id).unwrap_or(0);
            caller_corp != 0 && caller_corp == owner_corp
        });
        if !authorized {
            return Some(Err(BridgeRpcError {
                code: "NotFighterOwner".into(),
                message: "Cannot scoop an abandoned fighter squadron belonging to another pilot or corporation".into(),
                client_error: None,
            }));
        }
    }
    let squad = {
        let mut fighters = ctx.active_fighters.write().unwrap();
        let resolved_key = if fighters.contains_key(&key) {
            key
        } else if let Some((&k, _)) = fighters.iter().find(|(_, f)| f.item_id == target_fighter_id) {
            k
        } else {
            return Some(Err(BridgeRpcError {
                code: "FighterNotFound".into(),
                message: format!("Fighter squadron {target_fighter_id} not found in space"),
                client_error: None,
            }));
        };
        if !fighters[&resolved_key].is_abandoned {
            return Some(Err(BridgeRpcError {
                code: "FighterNotAbandoned".into(),
                message: format!("Fighter squadron {target_fighter_id} is active and cannot be scooped"),
                client_error: None,
            }));
        }
        fighters.remove(&resolved_key).unwrap()
    };
    let restore_squad = |sq: crate::router::context::ActiveFighterSquadron| {
        let destroyed = with_combat_runtime(ctx, |rt| {
            rt.is_recently_destroyed_item_id(sq.ball_id)
                || rt.is_recently_destroyed_item_id(sq.item_id)
        });
        if !destroyed {
            ctx.active_fighters.write().unwrap().entry(sq.ball_id).or_insert(sq);
        }
    };

    let in_caller_sys = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&squad.item_id)
            .is_some_and(|it| it.location_id == sys_id as i64)
    });
    if !in_caller_sys {
        let ball_id = squad.ball_id;
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "FighterNotInSystem".into(),
            message: format!("Fighter squadron {ball_id} is not in solar system {sys_id}"),
            client_error: None,
        }));
    }
    let f_ball = sys_handle
        .get_ball_snapshot(squad.ball_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b);
    let s_ball = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b);
    let Some(f_ball) = f_ball else {
        let ball_id = squad.ball_id;
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "FighterNotInSystem".into(),
            message: format!("Fighter ball {ball_id} not found in system {sys_id}"),
            client_error: None,
        }));
    };
    let Some(s_ball) = s_ball else {
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "ShipNotInSystem".into(),
            message: format!("Ship ball {ship_id} not found in system {sys_id}"),
            client_error: None,
        }));
    };
    let dist = s_ball.surface_distance(&f_ball);
    if dist > 2500.0 {
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "FighterOutOfRange".into(),
            message: format!("Fighter squadron is {dist:.1}m from ship hull, must be within 2500m to scoop"),
            client_error: None,
        }));
    }

    let db_res = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get(&char_id) {
            let cur_db_state = (ch.station_id, ch.structure_id, ch.solar_system_id, ch.active_ship_id);
            if entry_db_state.is_some_and(|e| cur_db_state != e) {
                return Err(BridgeRpcError {
                    code: "ShipNotInSystem".into(),
                    message: format!("Ship {ship_id} is no longer undocked in system {sys_id}"),
                    client_error: None,
                });
            }
        }
        let ship_type = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
        let bay_capacity = ctx.sde.types.get(&ship_type).and_then(|t| {
            t.default_attributes.get(&2055).copied().or_else(|| t.default_attributes.get(&2216).copied())
        }).unwrap_or(0.0);
        let has_fighter_bay = bay_capacity > 0.0
            || snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::FighterBay);
        if !has_fighter_bay {
            return Err(BridgeRpcError {
                code: "NoFighterBay".into(),
                message: "Caller ship has no fighter bay capacity".into(),
                client_error: None,
            });
        }
        let Some(existing_item) = snap.items.get(&squad.item_id) else {
            return Err(BridgeRpcError {
                code: "FighterNotFound".into(),
                message: format!("Fighter item {} no longer exists", squad.item_id),
                client_error: None,
            });
        };
        if existing_item.location_id != sys_id as i64 || existing_item.flag != InventoryFlag::None {
            return Err(BridgeRpcError {
                code: "FighterNotInSystem".into(),
                message: format!("Fighter squadron {} is no longer in space", squad.ball_id),
                client_error: None,
            });
        }
        let fighter_unit_vol = ctx.sde.types.get(&squad.type_id).map(|t| t.volume).unwrap_or(100.0);
        let squad_size = existing_item.quantity as f64;
        let total_fighter_vol = fighter_unit_vol * squad_size;
        let current_used_vol: f64 = snap.items.values()
            .filter(|it| it.location_id == ship_id && it.flag == InventoryFlag::FighterBay)
            .map(|it| {
                let unit_v = ctx.sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(100.0);
                unit_v * (it.quantity as f64)
            })
            .sum();
        if bay_capacity > 0.0 && current_used_vol + total_fighter_vol > bay_capacity {
            return Err(BridgeRpcError {
                code: "FighterBayFull".into(),
                message: format!("Fighter bay volume full: requires {total_fighter_vol:.1}m3, available {:.1}m3", bay_capacity - current_used_vol),
                client_error: None,
            });
        }

        if let Some(item) = snap.items.get_mut(&squad.item_id) {
            item.location_id = ship_id;
            item.owner_id = char_id;
            item.flag = InventoryFlag::FighterBay;
            item.version += 1;
        }
        snap.item_positions.remove(&squad.item_id);
        snap.item_positions.remove(&squad.ball_id);
        if let Some(meta) = snap.sp6_lifecycle.space_object_meta.get_mut(&squad.item_id) {
            meta.drone_ball_id = None;
        }
        Ok(())
    });
    if let Err(e) = db_res {
        restore_squad(squad);
        return Some(Err(e));
    }

    let _ = sys_handle.remove_ball(squad.ball_id).await;
    Some(Ok(PyRep::Bool(true)))
}

#[cfg(test)]
#[path = "combat_fighters_tests.rs"]
pub(in crate::services) mod combat_fighters_tests;
#[cfg(test)]
#[path = "combat_fighters_edges2_tests.rs"]
mod combat_fighters_edges2_tests;
