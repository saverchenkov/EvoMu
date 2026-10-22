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
            let now_unix = ctx.now_unix_secs();
            if method == "Eject" && (session.station_id.is_some() || session.structure_id.is_some()) {
                return Some(Err(BridgeRpcError {
                    code: "CannotEjectWhileDocked".into(),
                    message: "Eject is only possible in space; use LeaveShip while docked".into(),
                }));
            }
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
                snap.next_entity_id += 1;
                let capsule_id = snap.next_entity_id;
                snap.items.insert(
                    capsule_id,
                    eve_db::InventoryItemRecord {
                        item_id: capsule_id,
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
                    let pkg_bytes = eve_proto::marshal::encode(&eve_destiny::wire::encode_destiny_updates(
                        1,
                        &[slim_ev.clone(), inter_ev.clone()],
                        None,
                    ));
                    let _ = sys.emit_destiny_event(capsule_id, slim_ev).await;
                    let _ = sys.emit_destiny_event(capsule_id, inter_ev).await;
                    let _ = sys
                        .emit_destiny_event(
                            capsule_id,
                            eve_destiny::DestinyEvent::PackagedAction { actions_bytes: pkg_bytes },
                        )
                        .await;
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

            Some(Ok(PyRep::keyval([
                ("capsuleID", PyRep::Integer(capsule_id)),
                ("shipID", PyRep::Integer(capsule_id)),
                ("abandonedShipID", PyRep::Integer(old_ship_id)),
                ("typeID", PyRep::Integer(670)),
            ])))
        }

        ("ship", "BoardStoredShip") => Some(board_stored_ship(ctx, session, args).await),

        ("ship", "StoreVessel") | ("ship", "ScoopToSMA") | ("ship", "ScoopToFleetHangar") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_sma = method != "ScoopToFleetHangar";
            let target_flag_u16: u16 = if is_sma { 90 } else { 155 };
            let target_flag = if is_sma {
                InventoryFlag::SpecializedShipHold
            } else {
                InventoryFlag::Cargo
            };
            let (item_id, host_ship_id) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(1)
                        .and_then(|a| a.as_i64())
                        .or(session.ship_id)
                        .unwrap_or(0),
                )
            } else {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                    session.ship_id.unwrap_or(0),
                )
            };
            if let Err(e) = validate_container_and_bay_move(
                ctx,
                char_id,
                item_id,
                host_ship_id,
                target_flag_u16,
                kwargs,
            ) {
                return Some(Err(e));
            }
            let store_auth = ctx.db.with_snapshot(|snap| {
                let Some(it) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                let Some(host) = snap.items.get(&host_ship_id) else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Host ship {host_ship_id} not found"),
                    });
                };
                if snap
                    .characters
                    .values()
                    .any(|c| c.character_id != char_id && c.active_ship_id == item_id && c.station_id.is_none())
                {
                    return Err(BridgeRpcError {
                        code: "ShipPiloted".into(),
                        message: format!("Ship {item_id} is currently piloted by another character"),
                    });
                }
                let ch_loc = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.station_id.or(c.structure_id).or(Some(c.solar_system_id as i64)))
                    .unwrap_or(0);
                if it.owner_id != char_id && host.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!("Character {char_id} does not own item {item_id} or host ship {host_ship_id}"),
                    });
                }
                if it.location_id != ch_loc && it.location_id != host_ship_id && it.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "InvalidLocation".into(),
                        message: format!("Item {item_id} is not in range or at the same location"),
                    });
                }
                Ok(())
            });
            if let Err(e) = store_auth {
                return Some(Err(e));
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.get_mut(&item_id) {
                    it.location_id = host_ship_id;
                    it.flag = target_flag;
                    it.version += 1;
                }
            });
            container_ship_state()
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .custom_item_flags
                .insert(item_id, target_flag_u16);
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_ball(item_id).await;
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(item_id)),
                ("hostShipID", PyRep::Integer(host_ship_id)),
                ("flagID", PyRep::Integer(target_flag_u16 as i64)),
            ])))
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
            let item_id = if args.len() >= 2 {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (type_id, parent_id, item_owner) = {
                    let Some(it) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    (it.type_id, it.location_id, it.owner_id)
                };
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let parent_ok = if let Some(parent) = snap.items.get(&parent_id) {
                    parent.location_id == sys_id as i64
                        && (parent.owner_id == char_id
                            || (corp_id > 0 && parent.owner_id == corp_id)
                            || item_owner == char_id)
                } else {
                    false
                };
                if !parent_ok {
                    return Err(BridgeRpcError {
                        code: "LaunchAccessDenied".into(),
                        message: format!("Item {item_id} is not inside an accessible container or ship bay in system {sys_id}"),
                    });
                }
                let it = snap.items.get_mut(&item_id).unwrap();
                it.location_id = sys_id as i64;
                it.flag = InventoryFlag::None;
                it.singleton = true;
                it.version += 1;
                Ok((type_id, parent_id))
            });
            let (type_id, parent_id) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys = ctx.systems.get_or_spawn(sys_id);
            let parent_pos = sys
                .get_ball_snapshot(parent_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let launch_pos = DVec3::new(parent_pos.x + 300.0, parent_pos.y, parent_pos.z);
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
                        owner_id: char_id,
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
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(0);
            if method == "SetShipKillCounter" {
                let kills = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(it) = snap.items.get_mut(&ship_id) {
                        it.dynamic_attributes.insert(9998, kills as f64);
                    } else {
                        snap.item_custom_names.insert(-8_900_000 - ship_id, kills.to_string());
                    }
                });
                return Some(Ok(PyRep::Integer(kills)));
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Fitting from saved fitting requires being docked".into(),
                }));
            };
            let mut target_ship_ids: Vec<i64> = Vec::new();
            if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
                if list.iter().all(|el| el.as_i64().is_some()) {
                    target_ship_ids.extend(list.iter().filter_map(|el| el.as_i64()));
                }
            }
            if target_ship_ids.is_empty() {
                if let Some(sid) = kwargs
                    .get("shipID")
                    .and_then(|v| v.as_i64())
                    .or(session.ship_id)
                {
                    target_ship_ids.push(sid);
                }
            }
            let fitting_id_opt = if args.len() >= 2 && !target_ship_ids.is_empty() {
                args.get(1).and_then(|a| a.as_i64())
            } else {
                args.first()
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("fittingID").and_then(|v| v.as_i64()))
            };
            let mut desired_items: Vec<(TypeId, InventoryFlag, i32)> = Vec::new();
            if let Some(fid) = fitting_id_opt {
                if let Some(saved) = ctx.db.with_snapshot(|s| s.saved_fittings.get(&fid).cloned()) {
                    desired_items = saved.items;
                }
            }
            if desired_items.is_empty() {
                let fit_data_arg = kwargs
                    .get("fitData")
                    .or_else(|| args.get(1))
                    .or_else(|| args.first());
                if let Some(PyRep::List(list) | PyRep::Tuple(list)) = fit_data_arg {
                    for el in list {
                        if let PyRep::Tuple(seq) | PyRep::List(seq) = el {
                            if let (Some(tid), Some(flg)) = (
                                seq.first().and_then(|v| v.as_i64()),
                                seq.get(1).and_then(|v| v.as_i64()),
                            ) {
                                let qty = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1) as i32;
                                desired_items.push((
                                    tid as TypeId,
                                    InventoryFlag::from_u16(flg as u16),
                                    qty,
                                ));
                            }
                        }
                    }
                }
            }

            let ship_check = ctx.db.with_snapshot(|snap| {
                for &ship_id in &target_ship_ids {
                    let Some(sh) = snap.items.get(&ship_id) else {
                        return Err(BridgeRpcError {
                            code: "ShipNotFound".into(),
                            message: format!("Target ship {ship_id} not found"),
                        });
                    };
                    let cat = ctx.sde.types.get(&sh.type_id).map(|t| t.category_id).unwrap_or(0);
                    if sh.owner_id != char_id || sh.location_id != station_id || cat != 6 {
                        return Err(BridgeRpcError {
                            code: "InvalidFittingTarget".into(),
                            message: format!("Target {ship_id} must be an owned ship at station {station_id}"),
                        });
                    }
                }
                for &(tid, flag, req_qty) in &desired_items {
                    if req_qty <= 0 {
                        return Err(BridgeRpcError {
                            code: "InvalidQuantity".into(),
                            message: "Fitting item quantity must be positive".into(),
                        });
                    }
                    let flag_u16 = flag as u16;
                    if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                        let cat = ctx.sde.types.get(&tid).map(|t| t.category_id).unwrap_or(0);
                        if !matches!(cat, 7 | 8 | 32) || (cat != 8 && req_qty != 1) {
                            return Err(BridgeRpcError {
                                code: "ItemNotFittableModule".into(),
                                message: format!("Type {tid} (qty {req_qty}) cannot be fitted to slot {flag_u16}"),
                            });
                        }
                    }
                }
                Ok(())
            });
            if let Err(e) = ship_check {
                return Some(Err(e));
            }

            let mut fitted_count = 0_i64;
            let mut missing: HashMap<TypeId, i64> = HashMap::new();
            ctx.db.with_mut_snapshot(|snap| {
                for &ship_id in &target_ship_ids {
                    if let Some(sh) = snap.items.get_mut(&ship_id) {
                        sh.singleton = true;
                        sh.version += 1;
                    }
                    let mut ship_fitted = false;
                    for &(tid, flag, req_qty) in &desired_items {
                        let cand_opt = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == char_id
                                    && i.location_id == station_id
                                    && i.flag == InventoryFlag::Hangar
                                    && i.type_id == tid
                                    && i.quantity > 0
                            })
                            .map(|i| (i.item_id, i.quantity));
                        match cand_opt {
                            Some((cid, avail)) if avail >= req_qty => {
                                if avail > req_qty {
                                    if let Some(orig) = snap.items.get_mut(&cid) {
                                        orig.quantity -= req_qty;
                                        orig.version += 1;
                                    }
                                    snap.next_entity_id += 1;
                                    let split_id = snap.next_entity_id;
                                    snap.items.insert(
                                        split_id,
                                        eve_db::InventoryItemRecord {
                                            item_id: split_id,
                                            type_id: tid,
                                            owner_id: char_id,
                                            location_id: ship_id,
                                            flag,
                                            quantity: req_qty,
                                            singleton: req_qty == 1,
                                            version: 1,
                                            ..Default::default()
                                        },
                                    );
                                    snap.module_online_states.insert(split_id, true);
                                } else if let Some(it) = snap.items.get_mut(&cid) {
                                    it.location_id = ship_id;
                                    it.flag = flag;
                                    it.singleton = req_qty == 1;
                                    it.version += 1;
                                    snap.module_online_states.insert(cid, true);
                                }
                                ship_fitted = true;
                            }
                            Some((_, avail)) => {
                                *missing.entry(tid).or_insert(0) += (req_qty - avail) as i64;
                            }
                            None => {
                                *missing.entry(tid).or_insert(0) += req_qty as i64;
                            }
                        }
                    }
                    if ship_fitted {
                        fitted_count += 1;
                    }
                }
            });
            for &ship_id in &target_ship_ids {
                ctx.invalidate_ship_fitting(ship_id);
            }
            let first_ship = target_ship_ids.first().copied().unwrap_or(0);
            if fitted_count > 0 {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnMultipleItemChange",
                    "charid",
                    vec![PyRep::List(vec![PyRep::Integer(first_ship)])],
                );
            }
            let missing_entries: Vec<(PyRep, PyRep)> = missing
                .into_iter()
                .map(|(t, q)| (PyRep::Integer(t as i64), PyRep::Integer(q)))
                .collect();
            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(first_ship)),
                ("fittedCount", PyRep::Integer(fitted_count)),
                ("missingItems", PyRep::Dict(missing_entries)),
            ])))
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
            let alsc_locked = container_ship_state()
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .alsc_locked_items
                .clone();

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

        _ => None,
    }
}
