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
    let squad_opt = ctx
        .active_fighters
        .read()
        .unwrap()
        .get(&fighter_ball_id)
        .cloned();
    let Some(mut squad) = squad_opt else {
        return (0, 0, false);
    };
    let unit_hp = ctx.sde.types.get(&squad.type_id)
        .and_then(|t| t.default_attributes.get(&263).copied().or_else(|| t.default_attributes.get(&9).copied()))
        .unwrap_or(1_000.0).max(250.0);
    let raw_dmg = damage.total().max(0.0);
    let total_accum = with_mut_combat_runtime(ctx, |rt| {
        let entry = rt.fighter_unit_damage_accum.entry(fighter_ball_id).or_insert(0.0);
        *entry += raw_dmg;
        *entry
    });
    let units_killed = (total_accum / unit_hp).floor() as u32;
    if units_killed > 0 {
        with_mut_combat_runtime(ctx, |rt| {
            if let Some(entry) = rt.fighter_unit_damage_accum.get_mut(&fighter_ball_id) {
                *entry = (*entry - (units_killed as f64) * unit_hp).max(0.0);
            }
        });
    }
    let before_size = squad.squadron_size;
    let lost = units_killed.min(before_size);
    squad.squadron_size = before_size.saturating_sub(lost);
    let wiped_out = squad.squadron_size == 0;
    let owner_char_id = squad.owner_char_id;
    let tube_flag = (159 + squad.tube_index) as i64;
    if wiped_out {
        ctx.active_fighters.write().unwrap().remove(&fighter_ball_id);
        with_mut_combat_runtime(ctx, |rt| { rt.fighter_unit_damage_accum.remove(&fighter_ball_id); });
        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = sys_handle.remove_ball(fighter_ball_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            let reserve = snap.items.get(&squad.item_id).map(|it| it.quantity - (before_size as i32)).unwrap_or(0);
            if reserve > 0 {
                if let Some(it) = snap.items.get_mut(&squad.item_id) { it.quantity = reserve; it.singleton = false; it.version += 1; }
            } else {
                snap.items.remove(&squad.item_id);
            }
        });
        crate::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(ctx, fighter_ball_id);
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(owner_char_id),
            crate::router::notify_registry::OnFighterRemovedFromController {
                fighter_id: fighter_ball_id,
                tube_flag_id: tube_flag,
            },
        );
    } else if lost > 0 {
        ctx.active_fighters.write().unwrap().insert(fighter_ball_id, squad.clone());
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&squad.item_id) {
                let reserve = (it.quantity - (before_size as i32)).max(0);
                it.quantity = (squad.squadron_size as i32) + reserve;
                it.singleton = false;
                it.version += 1;
            }
        });
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(owner_char_id),
            crate::router::notify_registry::OnInSpaceSquadronSizeChanged {
                fighter_id: fighter_ball_id,
                squadron_size: squad.squadron_size as i64,
            },
        );
    }

    (lost, squad.squadron_size, wiped_out)
}

pub async fn handle_fighters(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let res = if service == "fighterMgr" && method == "CmdScoopAbandonedFighterFromSpace" {
        handle_scoop_abandoned_fighter(ctx, session, args).await
    } else {
        handle_fighters_inner(ctx, session, service, method, args, _kwargs).await
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

        ("fighterMgr", "LoadFightersToTube") => {
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

        ("fighterMgr", "UnloadTubeToFighterBay") => {
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

        ("fighterMgr", "LaunchFightersFromTubes") => {
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
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let entry_db_state = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| (c.station_id, c.structure_id, c.solar_system_id, c.active_ship_id))
            });
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                        client_error: None,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in space".into(),
                    client_error: None,
                }));
            };
            if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
                return Some(Err(BridgeRpcError {
                    code: "CannotOpenFighterTubesWhileWarping".into(),
                    message: "UserError: Cannot launch fighters while ship is in warp".into(),
                    client_error: None,
                }));
            }
            crate::services::space_destiny::space_nav_travel::sync_tether(ctx, char_id, sys_id, ship_id).await;
            let active_cloak_module = ctx.db.with_snapshot(|snap| {
                snap.items.values().any(|it| {
                    it.location_id == ship_id
                        && snap.module_active_states.contains(&it.item_id)
                        && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 330)
                })
            });
            if let Ok(Some(ship_cs)) = sys_handle.get_combat_state(ship_id).await {
                if ship_cs.is_cloaked || active_cloak_module {
                    return Some(Err(BridgeRpcError {
                        code: "CannotOpenFighterTubesWhileCloaked".into(),
                        message: "UserError: Cannot launch fighters while cloaked".into(),
                        client_error: None,
                    }));
                }
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
                let now_ft = ctx.now_blueos_filetime_ticks();
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

        ("fighterMgr", "RecallFightersToTubes") => {
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
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let filter_ids: Option<Vec<i64>> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) if !items.is_empty() => {
                    Some(items.iter().filter_map(|x| x.as_i64()).collect())
                }
                Some(PyRep::Integer(t)) => Some(vec![*t]),
                _ => None,
            };

            let fighters_to_recall: Vec<crate::router::context::ActiveFighterSquadron> = ctx
                .active_fighters
                .read()
                .unwrap()
                .values()
                .filter(|f| {
                    f.parent_ship_id == ship_id
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
                .cloned()
                .collect();

            let mut recalled = Vec::new();
            for f in fighters_to_recall {
                if sys_handle.get_ball_snapshot(f.ball_id).await.ok().flatten().is_none() {
                    continue;
                }
                let f_sde = ctx.sde.types.get(&f.type_id);
                let base_refuel_s = f_sde
                    .and_then(|t| t.default_attributes.get(&2426).copied())
                    .map(|ms| (ms / 1_000.0).clamp(2.0, 10.0))
                    .unwrap_or(3.0);
                let charge_reload_s: f64 = {
                    let used_map = ctx.fighter_charge_state.read().unwrap();
                    ctx.sde
                        .fighter_abilities_by_type
                        .get(&f.type_id)
                        .map(|slots| {
                            slots
                                .iter()
                                .filter_map(|s| {
                                    let ch = s.charges.as_ref()?;
                                    let used = used_map.get(&(f.ball_id, s.slot_index as u32)).copied().unwrap_or(0);
                                    let max_c = ch.charge_count.max(1) as f64;
                                    (used > 0).then_some(((used as f64) / max_c) * (ch.rearm_time_seconds.max(1.0) * 0.45))
                                })
                                .sum()
                        })
                        .unwrap_or(0.0)
                };
                let accum_dmg = with_combat_runtime(ctx, |rt| rt.fighter_unit_damage_accum.get(&f.ball_id).copied().unwrap_or(0.0));
                let missing_cs_hp = sys_handle
                    .get_combat_state(f.ball_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|cs| {
                        (cs.max_shield_hp - cs.shield_hp).max(0.0)
                            + (cs.max_armor_hp - cs.armor_hp).max(0.0)
                            + (cs.max_structure_hp - cs.structure_hp).max(0.0)
                    })
                    .unwrap_or(0.0);
                let unit_hp = f_sde
                    .and_then(|t| t.default_attributes.get(&263).copied().or_else(|| t.default_attributes.get(&9).copied()))
                    .unwrap_or(1_000.0)
                    .max(250.0);
                let repair_s = (((accum_dmg + missing_cs_hp) / unit_hp).clamp(0.0, 4.0)) * 1.5;

                ctx.active_fighters.write().unwrap().remove(&f.ball_id);
                ctx.fighter_charge_state.write().unwrap().retain(|&(bid, _), _| bid != f.ball_id);
                with_mut_combat_runtime(ctx, |rt| { rt.fighter_unit_damage_accum.remove(&f.ball_id); });

                let max_squadron_size = f_sde
                    .map(|t| {
                        t.default_attributes.get(&2215).copied().map(|v| v as i32).unwrap_or(match t.group_id {
                            1537 | 4778 => 3,
                            1653 | 4779 => 6,
                            _ => 9,
                        })
                    })
                    .unwrap_or(9);
                let loaded_tube_ids: std::collections::HashSet<i64> = ctx.fighter_tube_loads.read().unwrap().values().copied().collect();
                let mut refilled_units = 0i32;
                ctx.db.with_mut_snapshot(|snap| {
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
                });

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
                let now_ft = ctx.now_blueos_filetime_ticks();
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

        ("fighterMgr", "ExecuteMovementCommandOnFighters") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
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
                        return Some(Err(BridgeRpcError {
                            code: "FighterNotFound".into(),
                            message: format!("Fighter squadron {fid} not found"),
                            client_error: None,
                        }));
                    };
                    if f.owner_char_id != char_id || f.is_abandoned {
                        return Some(Err(BridgeRpcError {
                            code: "FighterNotOwned".into(),
                            message: format!("Fighter squadron {fid} is not controlled by pilot"),
                            client_error: None,
                        }));
                    }
                }
            }
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
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
                for fid in fighter_ids {
                    let ftype_id = ctx
                        .active_fighters
                        .read()
                        .unwrap()
                        .get(&fid)
                        .map(|f| f.type_id)
                        .unwrap_or(0);
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
                        return Some(Err(BridgeRpcError {
                            code: "FighterLacksMjdAbility".into(),
                            message: format!(
                                "Fighter squadron {fid} (type {ftype_id}) does not possess a Micro Jump Drive ability"
                            ),
                            client_error: None,
                        }));
                    }
                    let slot_idx = mjd_slot_opt.map(|s| s.slot_index as u32).unwrap_or(1);
                    if let Some(&ready_at) = ctx
                        .fighter_cooldown_state
                        .read()
                        .unwrap()
                        .get(&(fid, slot_idx))
                    {
                        if now_unix < ready_at {
                            return Some(Err(BridgeRpcError {
                                code: "AbilityOnCooldown".into(),
                                message: format!(
                                    "Fighter MJD ability on squadron {fid} is on cooldown until {ready_at}"
                                ),
                                client_error: None,
                            }));
                        }
                    }
                    let mjd_dist = mjd_dist_attr.unwrap_or(100_000.0);
                    let cd_secs = mjd_slot_opt
                        .and_then(|s| s.cooldown_seconds)
                        .map(|c| c.ceil() as u64)
                        .unwrap_or(60)
                        .max(1);
                    let _ = sys_handle.micro_jump(fid, mjd_dist).await;
                    ctx.fighter_cooldown_state
                        .write()
                        .unwrap()
                        .insert((fid, slot_idx), now_unix + cd_secs);
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
                let _ = sys_handle.set_movement_mode(fid, mode.clone()).await;
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("fighterMgr", "CmdActivateAbilitySlots") => {
            handle_activate_fighter_ability_slots(ctx, session, args).await
        }

        ("fighterMgr", "ActivateAbilitySlotsAtPoint") => {
            handle_activate_fighter_ability_slots_on_point(ctx, session, args).await
        }

        ("fighterMgr", "CmdDeactivateAbilitySlots") => {
            handle_deactivate_fighter_ability_slots(ctx, session, args).await
        }

        ("fighterMgr", "CmdAbandonFighter") => {
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
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let target_fighter_id = args.first().and_then(|a| a.as_i64());
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let mut abandoned = Vec::new();
            let mut abandoned_item_ids = Vec::new();
            {
                let mut fighters = ctx.active_fighters.write().unwrap();
                for f in fighters.values_mut() {
                    if f.parent_ship_id == ship_id
                        && target_fighter_id.map(|tid| tid == f.ball_id).unwrap_or(true)
                    {
                        f.is_abandoned = true;
                        f.parent_ship_id = 0;
                        f.target_id = None;
                        abandoned.push(f.ball_id);
                        abandoned_item_ids.push(f.item_id);
                    }
                }
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

        _ => None,
    }
}

async fn handle_scoop_abandoned_fighter(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Some(Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot scoop abandoned fighters while docked".into(),
            client_error: None,
        }));
    }
    let Some(ship_id) = session.ship_id else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        }));
    };
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
            client_error: None,
        }));
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
        ctx.active_fighters.write().unwrap().insert(sq.ball_id, sq);
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
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let f_pos = sys_handle
        .get_ball_snapshot(squad.ball_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position);
    let s_pos = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position);
    let Some(f_pos) = f_pos else {
        let ball_id = squad.ball_id;
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "FighterNotInSystem".into(),
            message: format!("Fighter ball {ball_id} not found in system {sys_id}"),
            client_error: None,
        }));
    };
    let Some(s_pos) = s_pos else {
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "ShipNotInSystem".into(),
            message: format!("Ship ball {ship_id} not found in system {sys_id}"),
            client_error: None,
        }));
    };
    let dist = (f_pos - s_pos).length();
    if dist > 2500.0 {
        restore_squad(squad);
        return Some(Err(BridgeRpcError {
            code: "FighterOutOfRange".into(),
            message: format!("Fighter squadron is {dist:.1}m away, must be within 2500m to scoop"),
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
