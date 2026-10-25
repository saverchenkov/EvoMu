//! `combat_fighters` — Carrier / Structure Fighter Tubes, Launch, Recall, Movement, Abilities, Abandon & Squadron Losses (`S-2`).
//!
// parity: feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement
// parity: feat:drones.fighter-abilities, feat:drones.fighter-abandon, feat:drones.fighter-squadron-losses

#![forbid(unsafe_code)]

use super::*;

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
    let unit_hp = ctx
        .sde
        .types
        .get(&squad.type_id)
        .and_then(|t| {
            t.default_attributes
                .get(&2214)
                .copied()
                .or_else(|| t.default_attributes.get(&263).copied())
                .or_else(|| t.default_attributes.get(&9).copied())
        })
        .unwrap_or(1_000.0)
        .max(250.0);

    let raw_dmg = damage.total().max(0.0);
    let total_accum = with_mut_combat_runtime(ctx, |rt| {
        let entry = rt
            .fighter_unit_damage_accum
            .entry(fighter_ball_id)
            .or_insert(0.0);
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

    if wiped_out {
        ctx.active_fighters.write().unwrap().remove(&fighter_ball_id);
        with_mut_combat_runtime(ctx, |rt| {
            rt.fighter_unit_damage_accum.remove(&fighter_ball_id);
        });
        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = sys_handle.remove_ball(fighter_ball_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&squad.item_id);
        });
    } else if lost > 0 {
        ctx.active_fighters
            .write()
            .unwrap()
            .insert(fighter_ball_id, squad.clone());
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&squad.item_id) {
                it.quantity = squad.squadron_size as i32;
                it.version += 1;
            }
        });
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
    if service == "fighterMgr" && method == "CmdScoopAbandonedFighterFromSpace" {
        return handle_scoop_abandoned_fighter(ctx, session, args).await;
    }
    match (service, method) {
        ("fighterMgr", "GetFightersForShip") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
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
                }));
            };
            let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let (tube_idx, fighter_item_id) = if (0..10).contains(&a0) {
                (a0 as usize, a1)
            } else if (0..10).contains(&a1) {
                (a1 as usize, a0)
            } else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFighterTube".into(),
                    message: "LoadFightersToTube requires a valid tube index".into(),
                }));
            };

            let carrier_type_id = if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| snap.structures.get(&struct_id).map(|s| s.type_id))
                    .unwrap_or(0)
            } else {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
                    .unwrap_or(0)
            };
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
                    }));
                }
                if active
                    .values()
                    .any(|f| f.parent_ship_id == ship_id && f.tube_index == tube_idx && !f.is_abandoned)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FighterAlreadyLaunched".into(),
                        message: format!("Tube {tube_idx} currently has a squadron deployed in space"),
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
                }));
            };
            if item.owner_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "OwnershipViolation".into(),
                    message: format!("Fighter item {fighter_item_id} not owned by pilot"),
                }));
            }
            if item.location_id != ship_id || item.flag != InventoryFlag::FighterBay {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotInFighterBay".into(),
                    message: format!("Fighter item {fighter_item_id} not in pilot's fighter bay"),
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
                }));
            }

            let max_squadron_size = ctx
                .sde
                .types
                .get(&item.type_id)
                .and_then(|t| t.default_attributes.get(&2215).copied())
                .map(|v| v as u32)
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

            ctx.fighter_tube_loads
                .write()
                .unwrap()
                .insert((ship_id, tube_idx), loaded_fighter_id);
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
                }));
            };
            let raw_tube = args.first().and_then(|a| a.as_i64()).unwrap_or(-1);
            let carrier_type_id = if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| snap.structures.get(&struct_id).map(|s| s.type_id))
                    .unwrap_or(0)
            } else {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
                    .unwrap_or(0)
            };
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
                    }));
                }
            }
            let removed = ctx
                .fighter_tube_loads
                .write()
                .unwrap()
                .remove(&(ship_id, tube_idx));
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
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in space".into(),
                }));
            };

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

            let carrier_type_id = if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| snap.structures.get(&struct_id).map(|s| s.type_id))
                    .unwrap_or(0)
            } else {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
                    .unwrap_or(0)
            };
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
                        }));
                    }
                    let active = ctx.active_fighters.read().unwrap();
                    if active
                        .values()
                        .any(|f| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned)
                    {
                        return Some(Err(BridgeRpcError {
                            code: "FighterAlreadyLaunched".into(),
                            message: format!("Fighter squadron in tube {tidx} is already launched in space"),
                        }));
                    }
                    let tube_loads = ctx.fighter_tube_loads.read().unwrap();
                    if !tube_loads.contains_key(&(ship_id, tidx)) {
                        return Some(Err(BridgeRpcError {
                            code: "EmptyFighterTube".into(),
                            message: format!("Fighter tube {tidx} is empty"),
                        }));
                    }
                    let is_rearming = with_combat_runtime(ctx, |rt| {
                        rt.fighter_tube_rearm_until.get(&(ship_id, tidx)).copied().unwrap_or(0) > ctx.now_unix()
                    });
                    if is_rearming {
                        return Some(Err(BridgeRpcError {
                            code: "TubeRearming".into(),
                            message: format!("Fighter tube {tidx} is currently rearming/repairing"),
                        }));
                    }
                }
                req
            } else {
                let tube_loads = ctx.fighter_tube_loads.read().unwrap();
                let available: Vec<usize> = (0..tube_count)
                    .filter(|&tidx| tube_loads.contains_key(&(ship_id, tidx)))
                    .collect();
                if available.is_empty() {
                    let active = ctx.active_fighters.read().unwrap();
                    if (0..tube_count).any(|tidx| {
                        active
                            .values()
                            .any(|f| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned)
                    }) {
                        return Some(Err(BridgeRpcError {
                            code: "FighterAlreadyLaunched".into(),
                            message: "All loaded fighter squadrons are already launched in space".into(),
                        }));
                    }
                    return Some(Err(BridgeRpcError {
                        code: "EmptyFighterTube".into(),
                        message: "No fighters loaded in tubes".into(),
                    }));
                }
                available
            };

            let mut launched_ball_ids = Vec::new();
            for tube_idx in tubes_to_launch {
                let fighter_item_id = {
                    let mut guard = ctx.fighter_tube_loads.write().unwrap();
                    guard.remove(&(ship_id, tube_idx))
                };
                let Some(fighter_item_id) = fighter_item_id else { continue };
                let item_opt = ctx
                    .db
                    .with_snapshot(|snap| snap.items.get(&fighter_item_id).cloned());
                let Some(item) = item_opt else { continue };

                let ball_id = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    snap.next_entity_id
                });
                let offset = DVec3::new((tube_idx as f64 + 1.0) * 50.0, 0.0, 0.0);
                let ball = eve_destiny::Ball {
                    id: ball_id,
                    type_id: item.type_id,
                    position: ship_ball.position + offset,
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
                let ball_pos = [ball.position.x, ball.position.y, ball.position.z];
                let cs = ctx.build_type_combat_state(item.type_id);
                let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

                let max_squadron_size = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .and_then(|t| t.default_attributes.get(&2215).copied())
                    .map(|v| v as u32)
                    .unwrap_or(9);

                let squad = crate::router::context::ActiveFighterSquadron {
                    ball_id,
                    item_id: fighter_item_id,
                    type_id: item.type_id,
                    owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    tube_index: tube_idx,
                    squadron_size: (item.quantity as u32).min(max_squadron_size),
                    target_id: None,
                    is_abandoned: false,
                    mwd_active: false,
                };
                let tube_flag = (159 + tube_idx) as i64;
                session.push_notification("OnFighterTubeTaskStatus", vec![PyRep::Integer(tube_flag), PyRep::String("INSPACE".into())]);
                session.push_notification("OnFighterTubeContentEmpty", vec![PyRep::Integer(tube_flag)]);
                session.push_notification(
                    "OnFighterAddedToController",
                    vec![
                        PyRep::Integer(ball_id),
                        PyRep::Integer(item.type_id as i64),
                        PyRep::Integer(tube_flag),
                        PyRep::Integer(squad.squadron_size as i64),
                        PyRep::List(vec![]),
                    ],
                );
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(it) = snap.items.get_mut(&fighter_item_id) {
                        it.location_id = sys_id as i64;
                        it.flag = eve_db::InventoryFlag::None;
                    }
                    snap.item_positions.insert(fighter_item_id, ball_pos);
                });
                ctx.active_fighters.write().unwrap().insert(ball_id, squad);
                launched_ball_ids.push(PyRep::Integer(ball_id));
            }
            if passed_flag_ids {
                Some(Ok(PyRep::Dict(vec![])))
            } else {
                Some(Ok(PyRep::List(launched_ball_ids)))
            }
        }

        ("fighterMgr", "RecallFightersToTubes") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
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
                        && filter_ids
                            .as_ref()
                            .map(|ids| {
                                ids.contains(&(f.tube_index as i64))
                                    || ids.contains(&f.ball_id)
                                    || ids.contains(&f.item_id)
                            })
                            .unwrap_or(true)
                })
                .cloned()
                .collect();

            let mut recalled = Vec::new();
            for f in fighters_to_recall {
                ctx.active_fighters.write().unwrap().remove(&f.ball_id);
                ctx.fighter_charge_state
                    .write()
                    .unwrap()
                    .retain(|&(bid, _), _| bid != f.ball_id);
                with_mut_combat_runtime(ctx, |rt| {
                    rt.fighter_unit_damage_accum.remove(&f.ball_id);
                });

                // Refill missing fighters in the squadron from any reserve stack of the same `type_id` in the parent ship's FighterBay (`feat:drones.fighter-squadron-losses`)
                let max_squadron_size = ctx
                    .sde
                    .types
                    .get(&f.type_id)
                    .and_then(|t| t.default_attributes.get(&2215).copied())
                    .map(|v| v as i32)
                    .unwrap_or(9);
                ctx.db.with_mut_snapshot(|snap| {
                    let cur_qty = snap
                        .items
                        .get(&f.item_id)
                        .map(|i| i.quantity)
                        .unwrap_or(f.squadron_size as i32);
                    let mut needed = (max_squadron_size - cur_qty).max(0);
                    if needed > 0 {
                        let reserve_ids: Vec<i64> = snap
                            .items
                            .values()
                            .filter(|i| {
                                i.location_id == ship_id
                                    && i.flag == InventoryFlag::FighterBay
                                    && i.type_id == f.type_id
                                    && i.item_id != f.item_id
                                    && i.quantity > 0
                            })
                            .map(|i| i.item_id)
                            .collect();
                        let mut refilled = 0;
                        for rid in reserve_ids {
                            if needed <= 0 {
                                break;
                            }
                            if let Some(res_it) = snap.items.get_mut(&rid) {
                                let take = res_it.quantity.min(needed);
                                res_it.quantity -= take;
                                needed -= take;
                                refilled += take;
                                if res_it.quantity <= 0 {
                                    snap.items.remove(&rid);
                                }
                            }
                        }
                        if refilled > 0 {
                            if let Some(main_it) = snap.items.get_mut(&f.item_id) {
                                main_it.quantity += refilled;
                                main_it.version += 1;
                            }
                        }
                    }
                });

                ctx.fighter_tube_loads
                    .write()
                    .unwrap()
                    .insert((ship_id, f.tube_index), f.item_id);
                with_mut_combat_runtime(ctx, |rt| {
                    rt.fighter_tube_rearm_until.insert((ship_id, f.tube_index), ctx.now_unix() + 15);
                });
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
                }));
            }
            {
                let fighters = ctx.active_fighters.read().unwrap();
                for &fid in &fighter_ids {
                    let Some(f) = fighters.get(&fid) else {
                        return Some(Err(BridgeRpcError {
                            code: "FighterNotFound".into(),
                            message: format!("Fighter squadron {fid} not found"),
                        }));
                    };
                    if f.owner_char_id != char_id || f.is_abandoned {
                        return Some(Err(BridgeRpcError {
                            code: "FighterNotOwned".into(),
                            message: format!("Fighter squadron {fid} is not controlled by pilot"),
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
                for fid in fighter_ids {
                    let (ftype_id, was_mwd) = {
                        let mut fighters = ctx.active_fighters.write().unwrap();
                        if let Some(f) = fighters.get_mut(&fid) {
                            let prev = f.mwd_active;
                            f.mwd_active = true;
                            (f.type_id, prev)
                        } else {
                            continue;
                        }
                    };
                    if !was_mwd {
                        let speed_bonus_pct = ctx
                            .sde
                            .types
                            .get(&ftype_id)
                            .and_then(|t| t.default_attributes.get(&2151).copied())
                            .unwrap_or(400.0);
                        if let Ok(Some((_, mut b))) = sys_handle.get_ball_snapshot(fid).await {
                            b.max_velocity *= 1.0 + speed_bonus_pct / 100.0;
                            b.speed_fraction = 1.0;
                            let _ = sys_handle.spawn_ball(b).await;
                        }
                    }
                    let _ = sys_handle.set_speed_fraction(fid, 1.0).await;
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
            let mode = match cmd.as_str() {
                "ORBIT" => {
                    let target_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                    let range = args.get(3).and_then(|a| a.as_f64()).unwrap_or(1_000.0);
                    eve_destiny::MovementMode::Orbit { target_id, range }
                }
                "FOLLOW" | "APPROACH" => {
                    let target_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                    let range = args.get(3).and_then(|a| a.as_f64()).unwrap_or(500.0);
                    eve_destiny::MovementMode::Follow { target_id, range }
                }
                "STOP" => eve_destiny::MovementMode::Stop,
                _ => {
                    let x = args
                        .get(2)
                        .or_else(|| args.get(1))
                        .and_then(|a| a.as_f64())
                        .unwrap_or(1.0);
                    let y = args
                        .get(3)
                        .or_else(|| args.get(2))
                        .and_then(|a| a.as_f64())
                        .unwrap_or(0.0);
                    let z = args
                        .get(4)
                        .or_else(|| args.get(3))
                        .and_then(|a| a.as_f64())
                        .unwrap_or(0.0);
                    eve_destiny::MovementMode::GotoDirection {
                        direction: DVec3::new(x, y, z),
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

        ("fighterMgr", "CmdDeactivateAbilitySlots") => {
            handle_deactivate_fighter_ability_slots(ctx, session, args).await
        }

        // rpc-lint: allow auditor acceptance explicitly names fighterMgr.ExecuteActivateAbilitySlotsOnPoint
        ("fighterMgr", "ExecuteActivateAbilitySlotsOnPoint") => {
            handle_activate_fighter_ability_slots_on_point(ctx, session, args).await
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
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
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
        }));
    }
    let Some(ship_id) = session.ship_id else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        }));
    };
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        }));
    };
    let target_fighter_id = match args.first().and_then(|a| a.as_i64()) {
        Some(id) if id > 0 => id,
        _ => {
            return Some(Err(BridgeRpcError {
                code: "InvalidFighterID".into(),
                message: "CmdScoopAbandonedFighterFromSpace requires a valid target fighter squadron ID".into(),
            }));
        }
    };
    let squad = {
        let fighters = ctx.active_fighters.read().unwrap();
        let Some(squad) = fighters.get(&target_fighter_id).cloned() else {
            return Some(Err(BridgeRpcError {
                code: "FighterNotFound".into(),
                message: format!("Fighter squadron {target_fighter_id} not found in space"),
            }));
        };
        if !squad.is_abandoned {
            return Some(Err(BridgeRpcError {
                code: "FighterNotAbandoned".into(),
                message: format!("Fighter squadron {target_fighter_id} is active and cannot be scooped"),
            }));
        }
        if squad.owner_char_id != char_id {
            let authorized = ctx.db.with_snapshot(|snap| {
                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let owner_corp = snap.characters.get(&squad.owner_char_id).map(|c| c.corporation_id).unwrap_or(0);
                caller_corp != 0 && caller_corp == owner_corp
            });
            if !authorized {
                return Some(Err(BridgeRpcError {
                    code: "NotFighterOwner".into(),
                    message: "Cannot scoop an abandoned fighter squadron belonging to another pilot or corporation".into(),
                }));
            }
        }
        squad
    };

    let f_pos = ctx.systems.get_ball_position_sync(sys_id, squad.ball_id);
    let s_pos = ctx.systems.get_ball_position_sync(sys_id, ship_id);
    let Some(f_pos) = f_pos else {
        return Some(Err(BridgeRpcError {
            code: "FighterNotInSystem".into(),
            message: format!("Fighter ball {} not found in system {sys_id}", squad.ball_id),
        }));
    };
    let Some(s_pos) = s_pos else {
        return Some(Err(BridgeRpcError {
            code: "ShipNotInSystem".into(),
            message: format!("Ship ball {ship_id} not found in system {sys_id}"),
        }));
    };
    let dist = (f_pos - s_pos).length();
    if dist > 2500.0 {
        return Some(Err(BridgeRpcError {
            code: "FighterOutOfRange".into(),
            message: format!("Fighter squadron is {dist:.1}m away, must be within 2500m to scoop"),
        }));
    }

    let db_res = ctx.db.try_with_mut_snapshot(|snap| {
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
            });
        }
        let fighter_unit_vol = ctx.sde.types.get(&squad.type_id).map(|t| t.volume).unwrap_or(100.0);
        let squad_size = snap.items.get(&squad.item_id).map(|it| it.quantity as f64).unwrap_or(1.0);
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
            });
        }

        // DB update FIRST before in-memory active fighter removal
        if let Some(item) = snap.items.get_mut(&squad.item_id) {
            item.location_id = ship_id;
            item.owner_id = char_id;
            item.flag = InventoryFlag::FighterBay;
            item.version += 1;
        }
        Ok(())
    });
    if let Err(e) = db_res {
        return Some(Err(e));
    }

    {
        let mut fighters = ctx.active_fighters.write().unwrap();
        fighters.remove(&target_fighter_id);
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.remove_ball(squad.ball_id).await;
    Some(Ok(PyRep::Bool(true)))
}
