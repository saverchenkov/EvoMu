//! `fitting_ships_mastery` — Ship Eject/LeaveShip, BoardStoredShip, SMA, Fleet Hangar,
//! Ship Config, LaunchFromContainer, Newbie Ship, Ship Tree/Masteries, Killmarks, FitShips & Simulation.

#![forbid(unsafe_code)]

use super::*;

pub(crate) async fn handle_ships_mastery_and_fitting_ext(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("ship", "Eject") | ("ship", "LeaveShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(old_ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship to leave or eject from".into(),
                }));
            };
            if method == "LeaveShip" && session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "CannotLeaveShipInSpace".into(),
                    message: "LeaveShip is only allowed while docked; use Eject in space".into(),
                }));
            }
            if method == "Eject" && (session.station_id.is_some() || session.structure_id.is_some()) {
                return Some(Err(BridgeRpcError {
                    code: "CannotEjectWhileDocked".into(),
                    message: "Eject is only possible in space; use LeaveShip while docked".into(),
                }));
            }
            let is_capsule = ctx.db.with_snapshot(|snap| {
                snap.items.get(&old_ship_id).is_some_and(|it| it.type_id == 670)
            });
            if is_capsule {
                return Some(Err(BridgeRpcError {
                    code: "CannotEjectFromCapsule".into(),
                    message: "Cannot eject from a Capsule".into(),
                }));
            }
            let now_unix = ctx.now_unix_secs();
            if method == "Eject" {
                let has_weapon_timer = ctx
                    .crimewatch
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .states
                    .get(&char_id)
                    .is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                if has_weapon_timer {
                    return Some(Err(BridgeRpcError {
                        code: "CannotEjectWithWeaponTimer".into(),
                        message: "Cannot eject from ship while a weapons timer is active".into(),
                    }));
                }
            }
            if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
                return Some(Err(BridgeRpcError {
                    code: "SessionChangeTimerActive".into(),
                    message: "Session change timer is active".into(),
                }));
            }
            let dock_opt = session.station_id.or(session.structure_id);
            let sys_opt = session.solar_system_id;
            let loc_id = dock_opt
                .or_else(|| sys_opt.map(|s| s as i64))
                .unwrap_or(60_003_760);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(old_ship) = snap.items.get(&old_ship_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Active ship {old_ship_id} not found"),
                    });
                };
                if old_ship.type_id == 670 {
                    return Err(BridgeRpcError {
                        code: "CannotEjectFromCapsule".into(),
                        message: "Cannot eject from a Capsule".into(),
                    });
                }
                if let Some(os_mut) = snap.items.get_mut(&old_ship_id) {
                    os_mut.location_id = loc_id;
                    os_mut.flag = if dock_opt.is_some() {
                        InventoryFlag::Hangar
                    } else {
                        InventoryFlag::None
                    };
                }
                let existing = snap.items.values().find(|it| {
                    it.type_id == 670 && it.owner_id == char_id && it.location_id == loc_id
                    && it.flag == if dock_opt.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None }
                    && !snap.characters.values().any(|c| c.active_ship_id == it.item_id)
                }).map(|it| it.item_id);
                
                let capsule_id = if let Some(cid) = existing {
                    cid
                } else {
                    snap.next_entity_id += 1;
                    let cid = snap.next_entity_id;
                    snap.items.insert(
                        cid,
                        eve_db::InventoryItemRecord {
                            item_id: cid,
                            type_id: 670, // Capsule
                            owner_id: char_id,
                            location_id: loc_id,
                            flag: if dock_opt.is_some() {
                                InventoryFlag::Hangar
                            } else {
                                InventoryFlag::None
                            },
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    cid
                };
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.active_ship_id = capsule_id;
                }
                Ok(capsule_id)
            });

            let capsule_id = match res {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };

            if dock_opt.is_none() {
                if let Some(sys_id) = sys_opt {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let old_pos = sys
                        .get_ball_snapshot(old_ship_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|(_, b)| b.position)
                        .unwrap_or(DVec3::ZERO);
                    let pod_pos = DVec3::new(old_pos.x + 150.0, old_pos.y, old_pos.z);
                    let _ = sys
                        .spawn_ball_with_meta(
                            eve_destiny::Ball {
                                id: capsule_id,
                                type_id: 670,
                                position: pod_pos,
                                velocity: DVec3::ZERO,
                                max_velocity: 350.0,
                                inertia: 1.0,
                                mass: 16_000.0,
                                radius: 22.0,
                                speed_fraction: 0.0,
                                mode: eve_destiny::MovementMode::Stop,
                            },
                            eve_destiny::CombatState::default(),
                            eve_destiny::SlimItemMeta {
                                group_id: 29,
                                category_id: 6,
                                owner_id: char_id,
                                char_id: Some(char_id),
                                corp_id: session.corporation_id.unwrap_or(-1),
                                alliance_id: session.alliance_id.unwrap_or(-1),
                                security_status: 0.0,
                                is_cloaked: false,
                            },
                        )
                        .await;
                    let slim_ev = eve_destiny::DestinyEvent::OnSlimItemChange { ball_id: old_ship_id };
                    let inter_ev = eve_destiny::DestinyEvent::SetBallInteractive {
                        ball_id: old_ship_id,
                        is_interactive: false,
                    };
                    let _ = sys.emit_destiny_event(capsule_id, slim_ev.clone()).await;
                    let _ = sys.emit_destiny_event(capsule_id, inter_ev.clone()).await;
                    let _ = sys.emit_packaged_actions(capsule_id, vec![slim_ev, inter_ev]).await;
                }
            }

            session.ship_id = Some(capsule_id);
            ctx.invalidate_ship_fitting(old_ship_id);
            ctx.invalidate_ship_fitting(capsule_id);
            let changes = vec![(
                PyRep::String("shipid".to_string()),
                PyRep::Tuple(vec![PyRep::Integer(old_ship_id), PyRep::Integer(capsule_id)]),
            )];
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSessionChanged",
                "clientID",
                vec![
                    PyRep::Bool(false),
                    PyRep::Integer(session.session_id as i64),
                    PyRep::Dict(changes),
                ],
            );
            // `ProcessActiveShipChanged` is derived by the V24 client (`DogmaLocation.MakeShipActive`)
            // from the `shipid` session change; the server never sends it.

            let sys_id = session.solar_system_id.or_else(|| {
                ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            }).unwrap_or(0);
            crate::services::space_destiny::record_session_change_timer(
                ctx, char_id, now_unix, sys_id, 0, "board_or_eject",
            );

            if method == "LeaveShip" {
                Some(Ok(PyRep::Integer(capsule_id)))
            } else {
                Some(Ok(PyRep::keyval([
                    ("capsuleID", PyRep::Integer(capsule_id)),
                    ("shipID", PyRep::Integer(capsule_id)),
                    ("abandonedShipID", PyRep::Integer(old_ship_id)),
                    ("typeID", PyRep::Integer(670)),
                ])))
            }
        }

        ("ship", "BoardStoredShip") => Some(board_stored_ship(ctx, session, args, kwargs).await),

        ("ship", "Board") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: "Board requires target shipID argument or active ship in session"
                        .into(),
                }));
            };
            let now_unix = ctx.now_unix_secs();
            if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
                return Some(Err(BridgeRpcError {
                    code: "SessionChangeTimerActive".into(),
                    message: "Session change timer is active".into(),
                }));
            }

            if session.station_id.is_none() && session.structure_id.is_none() {
                let Some(sys_id) = session.solar_system_id else {
                    return Some(Err(BridgeRpcError {
                        code: "NoSolarSystem".into(),
                        message: "Session has no active solar system".into(),
                    }));
                };
                let Some(cur_ship_id) = session.ship_id else {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "Cannot board in space without an active vessel/capsule".into(),
                    }));
                };
                if cur_ship_id != target_ship_id {
                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                    let cur_ball = sys_handle.get_ball_snapshot(cur_ship_id).await.ok().flatten();
                    let tgt_ball = sys_handle
                        .get_ball_snapshot(target_ship_id)
                        .await
                        .ok()
                        .flatten();
                    let (Some((_, cb)), Some((_, tb))) = (cur_ball, tgt_ball) else {
                        return Some(Err(BridgeRpcError {
                            code: "ShipNotInSpace".into(),
                            message: format!(
                                "Target ship {target_ship_id} is not spawned on grid in system {sys_id}"
                            ),
                        }));
                    };
                    let dist = cb.position.distance(tb.position);
                    if dist > 2_500.0 {
                        return Some(Err(BridgeRpcError {
                            code: "ShipOutOfBoardingRange".into(),
                            message: format!(
                                "Target ship {target_ship_id} is {:.1} m away (must be <= 2500 m)",
                                dist
                            ),
                        }));
                    }
                    if let Err(e) = fitting_ship_bays::check_pos_forcefield_coverage(
                        ctx,
                        sys_id,
                        char_id,
                        target_ship_id,
                        &[cb.position, tb.position],
                    )
                    .await
                    {
                        return Some(Err(e));
                    }
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ship) = snap.items.get(&target_ship_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Ship {target_ship_id} not found"),
                    });
                };
                if ship.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!(
                            "Ship {target_ship_id} is not owned by character {char_id}"
                        ),
                    });
                };
                let reachable = if let Some(dock_id) = session.station_id.or(session.structure_id) {
                    ship.location_id == dock_id && ship.flag == InventoryFlag::Hangar
                } else {
                    Some(ship.location_id) == session.solar_system_id.map(|s| s as i64)
                };
                if !reachable
                    || !ship.singleton
                    || ship.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(target_ship_id)
                {
                    return Err(BridgeRpcError {
                        code: "ShipNotBoardable".into(),
                        message: format!(
                            "Ship {target_ship_id} must be assembled, unlocked, and at your current location"
                        ),
                    });
                }
                let is_ship = ctx
                    .sde
                    .types
                    .get(&ship.type_id)
                    .map(|t| t.category_id == 6)
                    .unwrap_or(false);
                if !is_ship {
                    return Err(BridgeRpcError {
                        code: "NotAShip".into(),
                        message: format!("Item {target_ship_id} is not a ship"),
                    });
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.active_ship_id = target_ship_id;
                }
                Ok(target_ship_id)
            });
            match res {
                Ok(sid) => {
                    let prev_ship_id = session.ship_id;
                    if let Some(prev) = prev_ship_id {
                        ctx.invalidate_ship_fitting(prev);
                        if prev != sid && session.station_id.is_none() && session.structure_id.is_none() {
                            let was_capsule = ctx.db.with_mut_snapshot(|snap| {
                                if snap.items.get(&prev).is_some_and(|it| it.type_id == 670) {
                                    snap.items.remove(&prev);
                                    true
                                } else {
                                    false
                                }
                            });
                            if was_capsule {
                                if let Some(sys_id) = session.solar_system_id {
                                    let sys = ctx.systems.get_or_spawn(sys_id);
                                    let _ = sys.remove_ball(prev).await;
                                    let slim_ev = eve_destiny::DestinyEvent::OnSlimItemChange { ball_id: sid };
                                    let inter_ev = eve_destiny::DestinyEvent::SetBallInteractive {
                                        ball_id: sid,
                                        is_interactive: true,
                                    };
                                    let _ = sys.emit_destiny_event(sid, slim_ev.clone()).await;
                                    let _ = sys.emit_destiny_event(sid, inter_ev.clone()).await;
                                    let _ = sys.emit_packaged_actions(sid, vec![slim_ev, inter_ev]).await;
                                }
                            }
                        }
                    }
                    ctx.invalidate_ship_fitting(sid);
                    session.ship_id = Some(sid);
                    let sys_id = session.solar_system_id.or_else(|| {
                ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            }).unwrap_or(0);
                    crate::services::space_destiny::record_session_change_timer(
                        ctx, char_id, now_unix, sys_id, 0, "board_or_eject",
                    );
                    if prev_ship_id != Some(sid) {
                        let changes = vec![(
                            PyRep::String("shipid".to_string()),
                            PyRep::Tuple(vec![
                                prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                PyRep::Integer(sid),
                            ]),
                        )];
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnSessionChanged",
                            "clientID",
                            vec![
                                PyRep::Bool(false),
                                PyRep::Integer(session.session_id as i64),
                                PyRep::Dict(changes),
                            ],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(char_id)),
                        ("shipID", PyRep::Integer(sid)),
                        (
                            "oldShipID",
                            prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("ship", "StoreVessel")
        | ("ship", "ScoopToSMA")
        | ("ship", "ScoopToFleetHangar")
        | ("ship", "Scoop")
        | ("ship", "ScoopToInfrastructureHold")
        | ("ship", "ScoopToMobileDepotHold") => {
            Some(fitting_ship_bays::ship_scoop_and_store(ctx, session, method, args, kwargs).await)
        }

        ("ship", "GetShipConfiguration") | ("ship", "ConfigureShip") | ("ship", "SetShipPassword") => {
            Some(ship_configuration(ctx, session, method, args))
        }

        ("eject", "LaunchShipOrContainerFromWreckOrContainer")
        | ("ship", "LaunchFromContainer") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Launching a ship from a container or bay requires being in space".into(),
                }));
            };
            let active_ship_id = match session.ship_id {
                Some(sid) => sid,
                None => return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "LaunchFromContainer requires an active ship".into(),
                })),
            };
            let item_id = if args.len() >= 2 {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };
            let supplied_password = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
            let sys = ctx.systems.get_or_spawn(sys_id);
            let active_pos = match sys.get_ball_snapshot(active_ship_id).await {
                Ok(Some((_, b))) => b.position,
                _ => return Some(Err(BridgeRpcError { code: "ObjectNotInSpace".into(), message: "Active ship not in space".into() })),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (type_id, parent_id, item_owner) = {
                    let Some(it) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    let it_type = ctx.sde.types.get(&it.type_id);
                    let cat_id = it_type.map(|t| t.category_id).unwrap_or(0);
                    let grp_id = it_type.map(|t| t.group_id).unwrap_or(0);
                    let is_assembled_ship = cat_id == 6 && it.singleton;
                    let is_valid_container = matches!(grp_id, 12 | 340 | 448 | 649 | 952) || cat_id == 2;
                    if !is_assembled_ship && !is_valid_container {
                        return Err(BridgeRpcError {
                            code: "InvalidLaunchItem".into(),
                            message: "Only assembled ships and valid containers can be launched".into(),
                        });
                    }
                    (it.type_id, it.location_id, it.owner_id)
                };
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let parent_owner = snap.items.get(&parent_id).map(|i| i.owner_id).unwrap_or(0);
                let is_sma = snap.items.get(&parent_id).and_then(|p| ctx.sde.types.get(&p.type_id)).and_then(|t| t.default_attributes.get(&908)).copied().unwrap_or(0.0) > 0.0;
                let parent_ok = if let Some(parent) = snap.items.get(&parent_id) {
                    parent.location_id == sys_id as i64
                        && (parent.owner_id == char_id
                            || (corp_id > 0 && parent.owner_id == corp_id)
                            || item_owner == char_id)
                        && super::fitting_containers_ships::can_character_access_ship_bay_with_password_snap(snap, char_id, parent_id, parent_owner, is_sma, supplied_password)
                } else {
                    false
                };
                if !parent_ok {
                    return Err(BridgeRpcError {
                        code: "LaunchAccessDenied".into(),
                        message: format!("Item {item_id} is not inside an accessible container or ship bay in system {sys_id}"),
                    });
                }
                Ok((type_id, parent_id, item_owner))
            });
            let (type_id, parent_id, item_owner) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let parent_pos = match sys.get_ball_snapshot(parent_id).await {
                Ok(Some((_, b))) => b.position,
                _ => {
                    let snap_pos = ctx.db.with_snapshot(|snap| {
                        let in_space = snap.items.get(&parent_id).is_some_and(|i| i.location_id == sys_id as i64);
                        if !in_space {
                            return None;
                        }
                        snap.item_positions.get(&parent_id).map(|p| DVec3::new(p[0], p[1], p[2]))
                            .or_else(|| snap.structures.get(&parent_id).map(|s| DVec3::new(s.position[0], s.position[1], s.position[2])))
                    });
                    match snap_pos {
                        Some(pos) => pos,
                        None => {
                            return Some(Err(BridgeRpcError {
                                code: "ObjectNotInSpace".into(),
                                message: format!("Parent {parent_id} is not in space in system {sys_id}"),
                            }));
                        }
                    }
                }
            };
            if active_pos.distance(parent_pos) > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutOfRange".into(),
                    message: "Parent ship/container is out of range (>2500m)".into(),
                }));
            }
            let launch_pos = DVec3::new(parent_pos.x + 300.0, parent_pos.y, parent_pos.z);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let parent_in_space = snap.items.get(&parent_id).is_some_and(|p| p.location_id == sys_id as i64)
                    || snap.structures.get(&parent_id).is_some_and(|s| s.solar_system_id == sys_id);
                if !parent_in_space {
                    return Err(BridgeRpcError {
                        code: "ObjectNotInSpace".into(),
                        message: format!("Parent {parent_id} is not in space in system {sys_id}"),
                    });
                }
                let it = snap.items.get_mut(&item_id).ok_or_else(|| BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                })?;
                if it.location_id != parent_id {
                    return Err(BridgeRpcError {
                        code: "LaunchAccessDenied".into(),
                        message: format!("Item {item_id} is no longer in parent {parent_id}"),
                    });
                }
                let old_loc = it.location_id;
                let old_flag = it.flag as i64;
                it.location_id = sys_id as i64;
                it.flag = InventoryFlag::None;
                it.singleton = true;
                it.version += 1;
                snap.item_positions.insert(item_id, [launch_pos.x, launch_pos.y, launch_pos.z]);
                Ok((old_loc, old_flag))
            });
            let (old_loc, old_flag) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let t_sde = ctx.sde.types.get(&type_id);
            let _ = sys
                .spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: item_id,
                        type_id,
                        position: launch_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 200.0,
                        inertia: 1.0,
                        mass: t_sde.map(|t| t.mass.max(1_000.0)).unwrap_or(1_000_000.0),
                        radius: t_sde.map(|t| t.radius.max(20.0)).unwrap_or(50.0),
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    eve_destiny::CombatState::default(),
                    eve_destiny::SlimItemMeta {
                        group_id: t_sde.map(|t| t.group_id).unwrap_or(25),
                        category_id: t_sde.map(|t| t.category_id).unwrap_or(6),
                        owner_id: item_owner,
                        char_id: None,
                        corp_id: session.corporation_id.unwrap_or(-1),
                        alliance_id: session.alliance_id.unwrap_or(-1),
                        security_status: 0.0,
                        is_cloaked: false,
                    },
                )
                .await;
            ctx.notify(
                crate::router::NotificationTarget::SolarSystem(sys_id),
                "DoBallsAdded",
                "solarsystemid",
                vec![PyRep::Integer(item_id), PyRep::Integer(type_id as i64)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnItemChange",
                "charid",
                vec![
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("typeID", PyRep::Integer(type_id as i64)),
                        ("locationID", PyRep::Integer(sys_id as i64)),
                        ("flagID", PyRep::Integer(InventoryFlag::None as i64)),
                        ("quantity", PyRep::Integer(1)),
                    ]),
                    PyRep::Dict(vec![
                        (PyRep::Integer(3), PyRep::Integer(old_loc)),
                        (PyRep::Integer(4), PyRep::Integer(old_flag)),
                    ]),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("launchedItemID", PyRep::Integer(item_id)),
                ("typeID", PyRep::Integer(type_id as i64)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
            ])))
        }

        ("dogmaIM", "CreateNewbieShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "CreateNewbieShip requires being docked at a station or structure".into(),
                }));
            };
            let now_unix = ctx.now_unix_secs();
            let claim_key = -8_800_000 - char_id;
            let on_cooldown = ctx.db.with_snapshot(|snap| {
                snap.item_custom_names
                    .get(&claim_key)
                    .and_then(|s| s.parse::<u64>().ok())
                    .is_some_and(|last_ts| now_unix < last_ts.saturating_add(60))
            });
            if on_cooldown {
                return Some(Err(BridgeRpcError {
                    code: "NewbieShipCooldown".into(),
                    message: "Corvette claim cooldown is still active".into(),
                }));
            }
            let capsule_group = ctx.sde.groups.values().find(|g| g.name == "Capsule").map(|g| g.group_id);
            let in_capsule = ctx.db.with_snapshot(|snap| {
                let active = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0);
                snap.items
                    .get(&active)
                    .and_then(|i| ctx.sde.types.get(&i.type_id))
                    .is_some_and(|t| Some(t.group_id) == capsule_group)
            });
            if !in_capsule {
                return Some(Err(BridgeRpcError {
                    code: "NotInCapsule".into(),
                    message: "A corvette can only be issued to a pilot docked in a capsule".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let has_non_pod_ship = snap.items.values().any(|i| {
                    i.owner_id == char_id
                        && i.location_id == station_id
                        && i.singleton
                        && i.type_id != 670
                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 6)
                });
                if has_non_pod_ship {
                    return Err(BridgeRpcError {
                        code: "AlreadyHaveShipInStation".into(),
                        message: "Character already has an assembled ship in this station".into(),
                    });
                }
                let race_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.race_id)
                    .unwrap_or(2);
                // DERIVED selection: the lowest-ID published SDE type in group "Corvette" whose raceID
                // matches the character's race (Reaper/Impairor/Ibis/Velator in the V24 SDE).
                let corvette_group = ctx.sde.groups.values().find(|g| g.name == "Corvette").map(|g| g.group_id);
                let Some(corvette_type_id) = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| Some(t.group_id) == corvette_group && t.published && t.race_id == Some(race_id as _))
                    .map(|t| t.type_id)
                    .min()
                else {
                    return Err(BridgeRpcError {
                        code: "NoRacialCorvette".into(),
                        message: format!("No published SDE corvette for race {race_id}"),
                    });
                };
                snap.next_entity_id += 1;
                let corvette_id = snap.next_entity_id;
                snap.items.insert(
                    corvette_id,
                    eve_db::InventoryItemRecord {
                        item_id: corvette_id,
                        type_id: corvette_type_id,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.active_ship_id = corvette_id;
                }
                Ok((corvette_id, corvette_type_id, race_id))
            });
            match res {
                Ok((corvette_id, corvette_tid, race_id)) => {
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.item_custom_names.insert(claim_key, now_unix.to_string());
                    });
                    session.ship_id = Some(corvette_id);
                    // `ProcessActiveShipChanged` is derived by the V24 client (`DogmaLocation.MakeShipActive`)
                    // from the `shipid` session change; the server never sends it.
                    Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(corvette_id)),
                        ("typeID", PyRep::Integer(corvette_tid as i64)),
                        ("raceID", PyRep::Integer(race_id as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("dogmaIM", "GetRequiredSkillLevels") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_id = args.first().and_then(|a| a.as_i64()).unwrap_or(587);
            let type_id = ctx
                .db
                .with_snapshot(|s| s.items.get(&raw_id).map(|i| i.type_id))
                .unwrap_or(raw_id as TypeId);
            let Some(t) = ctx.sde.types.get(&type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "TypeNotFound".into(),
                    message: format!("Type {type_id} not found"),
                }));
            };
            let mut req_skills = Vec::new();
            for &(sk_attr, lvl_attr) in &[
                (182u16, 277u16),
                (183, 278),
                (184, 279),
                (1285, 1286),
                (1289, 1287),
                (1290, 1288),
            ] {
                if let Some(&sk_tid) = t.default_attributes.get(&sk_attr) {
                    let lvl = t.default_attributes.get(&lvl_attr).copied().unwrap_or(1.0) as i64;
                    if sk_tid > 0.0 {
                        req_skills.push((PyRep::Integer(sk_tid as i64), PyRep::Integer(lvl)));
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("typeID", PyRep::Integer(type_id as i64)),
                ("typeName", PyRep::String(t.name.clone())),
                ("groupID", PyRep::Integer(t.group_id as i64)),
                ("raceID", t.race_id.map(|r| PyRep::Integer(r as i64)).unwrap_or(PyRep::None)),
                ("factionID", t.faction_id.map(|f| PyRep::Integer(f as i64)).unwrap_or(PyRep::None)),
                ("requiredSkills", PyRep::Dict(req_skills)),
            ])))
        }

        ("dogmaIM", "GetMyCertificates")
        | ("dogmaIM", "GetCertificatesForShipByMasteryLevel")
        | ("dogmaIM", "GetCurrCharMasteryLevel")
        | ("dogmaIM", "GetSkillsRequiredForMasteryLevel")
        | ("ship", "GetCurrCharMasteryLevel")
        | ("ship", "GetSkillsRequiredForMasteryLevel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_type_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as TypeId)
                .unwrap_or(587);
            let req_mastery_level = args
                .get(1)
                .and_then(|a| a.as_i64())
                .unwrap_or(1)
                .clamp(1, 5) as u8;
            let mut hull_skills: Vec<u32> = vec![3392, 3426, 3449]; // Mechanics, CPU Management, Power Grid Management
            if let Some(t) = ctx.sde.types.get(&ship_type_id) {
                for &sk_attr in &[182u16, 183, 184] {
                    if let Some(&sk_tid) = t.default_attributes.get(&sk_attr) {
                        if sk_tid > 0.0 && !hull_skills.contains(&(sk_tid as u32)) {
                            hull_skills.push(sk_tid as u32);
                        }
                    }
                }
            }
            let trained_levels = ctx.character_skill_levels(char_id);
            let min_trained = hull_skills
                .iter()
                .map(|&sk| trained_levels.get(&sk).copied().unwrap_or(0))
                .min()
                .unwrap_or(0)
                .min(5);
            let skills_for_tier: Vec<(PyRep, PyRep)> = hull_skills
                .iter()
                .map(|&sk| (PyRep::Integer(sk as i64), PyRep::Integer(req_mastery_level as i64)))
                .collect();
            Some(Ok(PyRep::keyval([
                ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                ("masteryLevel", PyRep::Integer(min_trained as i64)),
                ("requestedMasteryLevel", PyRep::Integer(req_mastery_level as i64)),
                ("requiredSkills", PyRep::Dict(skills_for_tier)),
            ])))
        }

        ("shipKillCounter", "GetItemKillCountPlayer")
        | ("shipKillCounter", "SetShipKillCounter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(0);
            if method == "SetShipKillCounter" {
                let kills = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(0);
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(it) = snap.items.get_mut(&ship_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Ship {ship_id} not found"),
                        });
                    };
                    if it.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "ShipOwnershipViolation".into(),
                            message: format!("Ship {ship_id} is not owned by character {char_id}"),
                        });
                    }
                    it.dynamic_attributes.insert(9998, kills as f64);
                    Ok(PyRep::Integer(kills))
                });
                return Some(res);
            }
            let kills = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .and_then(|i| i.dynamic_attributes.get(&9998).copied())
                    .map(|v| v as i64)
                    .or_else(|| {
                        snap.item_custom_names
                            .get(&(-8_900_000 - ship_id))
                            .and_then(|s| s.parse::<i64>().ok())
                    })
                    .unwrap_or(0)
            });
            Some(Ok(PyRep::Integer(kills)))
        }

        ("repairSvc", "GetDamagedModulesAndRepairCost") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(0);
            let fit_dmg: f64 = ctx
                .get_or_rebuild_ship_fitting(char_id, ship_id)
                .map(|f| f.modules.values().map(|m| m.damage_hp).sum())
                .unwrap_or(0.0);
            let dyn_dmg: f64 = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.owner_id == char_id && (i.item_id == ship_id || i.location_id == ship_id))
                    .map(|i| {
                        i.dynamic_attributes.get(&attr_ids::DAMAGE).copied().unwrap_or(0.0)
                            + i.dynamic_attributes.get(&attr_ids::ARMOR_DAMAGE).copied().unwrap_or(0.0)
                    })
                    .sum()
            });
            let total_hp = fit_dmg + dyn_dmg;
            let cost_isk = (total_hp * 500.0).round();
            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(ship_id)),
                ("totalDamagedHP", PyRep::Real(total_hp)),
                ("repairCostISK", PyRep::Real(cost_isk)),
            ])))
        }

        ("ship", "FitShips") => {
            crate::services::fitting_saved_apply::handle_fit_ships(ctx, session, args, kwargs).await
        }

        ("ship", "GetShipFittingInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_type_id = kwargs
                .get("shipTypeID")
                .and_then(|v| v.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or(587) as TypeId;
            let skills = ctx.character_skill_levels(char_id);
            let mut sim = eve_dogma::ShipFittingEngine::from_sde(0, ship_type_id, &ctx.sde)
                .with_character_skills(skills);
            if let Some(PyRep::List(list) | PyRep::Tuple(list)) =
                kwargs.get("fitData").or_else(|| args.get(1))
            {
                for (idx, el) in list.iter().enumerate() {
                    if let PyRep::Tuple(seq) | PyRep::List(seq) = el {
                        if let (Some(tid), Some(flg)) = (
                            seq.first().and_then(|v| v.as_i64()),
                            seq.get(1).and_then(|v| v.as_i64()),
                        ) {
                            let _ = sim.fit_and_online_module(
                                (idx as i64) + 1,
                                tid as TypeId,
                                flg as u16,
                                &ctx.sde,
                            );
                        }
                    }
                }
            }
            let eval_ctx = sim.evaluated_context(&ctx.sde);
            let cpu_load = eval_ctx.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde);
            let cpu_out = eval_ctx.ship.get_attribute(attr_ids::CPU_OUTPUT, &ctx.sde);
            let pwg_load = eval_ctx.ship.get_attribute(attr_ids::POWER_LOAD, &ctx.sde);
            let pwg_out = eval_ctx.ship.get_attribute(attr_ids::POWER_OUTPUT, &ctx.sde);
            let max_vel = eval_ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            let shield_cap = eval_ctx.ship.get_attribute(attr_ids::SHIELD_CAPACITY, &ctx.sde);
            let armor_hp = eval_ctx.ship.get_attribute(attr_ids::ARMOR_HP, &ctx.sde);
            let hull_hp = eval_ctx.ship.get_attribute(9, &ctx.sde);
            Some(Ok(PyRep::keyval([
                ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                ("simulated", PyRep::Bool(true)),
                ("cpuLoad", PyRep::Real(cpu_load)),
                ("cpuOutput", PyRep::Real(cpu_out)),
                ("powerLoad", PyRep::Real(pwg_load)),
                ("powerOutput", PyRep::Real(pwg_out)),
                ("maxVelocity", PyRep::Real(max_vel)),
                ("shieldCapacity", PyRep::Real(shield_cap)),
                ("armorHP", PyRep::Real(armor_hp)),
                ("hullHP", PyRep::Real(hull_hp)),
            ])))
        }

        ("invbroker", "TrashItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "DeleteFailed".into(),
                    message: "TrashItems requires at least one itemID".into(),
                }));
            }
            let expected_station = args.get(1).and_then(|a| a.as_i64()).filter(|&st| st > 0);
            let current_dock = session
                .station_id
                .or(session.structure_id)
                .or_else(|| _bound_entry.map(|b| b.object_id));
            let first_id = item_ids[0];
            let alsc_locked = read_container_ship_state(ctx)
                .alsc_locked_items;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                let mut trashed: Vec<(i64, i64)> = Vec::with_capacity(item_ids.len());
                for &item_id in &item_ids {
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!("Item {item_id} is not owned by character {char_id}"),
                        });
                    }
                    if item_id == active_ship_id {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!("Cannot trash active ship {item_id}"),
                        });
                    }
                    let is_fitted = matches!(
                        item.flag as u16,
                        11..=34 | 87 | 92..=94 | 125..=132 | 158 | 164..=171
                    );
                    if is_fitted {
                        return Err(BridgeRpcError {
                            code: "CannotTrashFittedItem".into(),
                            message: format!("Cannot trash fitted item {item_id}"),
                        });
                    }
                    if item.flag == InventoryFlag::Locked
                        || alsc_locked.contains(&item_id)
                        || snap.is_item_locked_in_contract_or_job(item_id)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked by an active contract, industry job, or container"
                            ),
                        });
                    }
                    if let Some(st_id) = expected_station {
                        if item.location_id != st_id
                            || current_dock.is_some_and(|dock_id| dock_id != st_id)
                        {
                            return Err(BridgeRpcError {
                                code: "DeleteFailed".into(),
                                message: format!(
                                    "Item {item_id} is located at {}, not accessible station {st_id}",
                                    item.location_id
                                ),
                            });
                        }
                    } else if let Some(dock_id) = current_dock {
                        let in_accessible_hangar = item.location_id == dock_id
                            || item.location_id == active_ship_id
                            || snap.items.get(&item.location_id).is_some_and(|parent| {
                                parent.location_id == dock_id || parent.location_id == active_ship_id
                            });
                        if !in_accessible_hangar {
                            return Err(BridgeRpcError {
                                code: "DeleteFailed".into(),
                                message: format!(
                                    "Item {item_id} at {} is not in an accessible hangar at {dock_id}",
                                    item.location_id
                                ),
                            });
                        }
                    }
                    if snap.items.values().any(|child| child.location_id == item_id) {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!(
                                "Cannot trash non-empty container or fitted ship {item_id}"
                            ),
                        });
                    }
                    trashed.push((item_id, item.location_id));
                }
                for &(item_id, _) in &trashed {
                    snap.items.remove(&item_id);
                    snap.item_custom_names.remove(&item_id);
                    snap.item_positions.remove(&item_id);
                }
                Ok(trashed)
            });

            let trashed = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            for &(item_id, old_location_id) in &trashed {
                ctx.invalidate_ship_fitting(item_id);
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnItemChange",
                    "charid",
                    vec![
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(item_id)),
                            ("locationID", PyRep::Integer(6)),
                        ]),
                        PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(old_location_id))]),
                    ],
                );
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(first_id)),
                ("deleted", PyRep::Bool(true)),
                ("count", PyRep::Integer(trashed.len() as i64)),
            ])))
        }

        ("ship", "GetDirtTimestamp") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(ship_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "ship.GetDirtTimestamp expects (itemID,)".into(),
                }));
            };
            let ts = ctx.db.with_snapshot(|snap| {
                snap.items.get(&ship_id).and_then(|item| {
                    item.dynamic_attributes
                        .get(&1969)
                        .copied()
                        .map(|v| v as i64)
                })
            });
            match ts {
                Some(t) => Some(Ok(PyRep::Integer(t))),
                None => {
                    let exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&ship_id));
                    if exists {
                        Some(Ok(PyRep::Integer(ctx.now_blueos_filetime_ticks())))
                    } else {
                        Some(Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("ship {ship_id} not found"),
                        }))
                    }
                }
            }
        }

        ("ship", "ResetDirtTimestamp") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "ship.ResetDirtTimestamp expects (itemID,)".into(),
                }));
            };
            let now = ctx.now_blueos_filetime_ticks();
            let ts_i64 = (now as f64) as i64;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get_mut(&ship_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("ship {ship_id} not found"),
                    });
                };
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by character {char_id}"),
                    });
                }
                item.dynamic_attributes.insert(1969, ts_i64 as f64);
                item.version += 1;
                Ok(PyRep::Integer(ts_i64))
            });
            Some(res)
        }

        // parity: feat:character.safe-logoff, plan:SP-6, rpc:ship.SafeLogoff
        ("ship", "SafeLogoff") => {
            Some(crate::sp6_lifecycle::handle_safe_logoff(ctx, session).await)
        }

        // parity: feat:character.safe-logoff, plan:SP-6, rpc:ship.AbortSafeLogoff
        ("ship", "AbortSafeLogoff") => Some(
            crate::sp6_lifecycle::handle_abort_safe_logoff(
                ctx,
                session,
                "SAFE_LOGOFF_ABORT_GENERIC",
            ),
        ),

        ("ship", "SetSessionCheck") | ("ship", "clear_memoized") | ("dogmaIM", "SetSessionCheck") => {
            // const-ok: session check confirmation
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }

        _ => None,
    }
}
