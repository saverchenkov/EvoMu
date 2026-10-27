#![forbid(unsafe_code)]
use super::*;
use crate::services::inventory_fitting::fitting_containers_ships;

/// Ship maintenance bay flags: `invFlagShipHangar` (90, used by `ship.StoreVessel`) and
/// `SpecializedShipHold` (138).
fn is_ship_bay_flag(flag: u16) -> bool {
    matches!(flag, 90 | 138)
}

/// Maximum distance (m) between the pilot's current ship and the bay host for in-space access —
/// the same range `ship.Board` uses for boarding in space.
const BAY_ACCESS_RANGE_M: f64 = 2_500.0;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// `ship.BoardStoredShip(hostID, storedShipID)`: board an owned, assembled ship held in the ship
/// maintenance bay of a ship (in space or docked) or of the structure the pilot is docked in.
pub(crate) async fn board_stored_ship(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let _supplied_password = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
    let (host_arg, stored_id) = if args.len() >= 2 {
        (args[0].as_i64().unwrap_or(0), args[1].as_i64().unwrap_or(0))
    } else {
        (0, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
    };
    let dock_id = session.station_id.or(session.structure_id);
    let custom_flag = read_container_ship_state(ctx).custom_item_flags.get(&stored_id).copied();
    let (stored, host_type, host_loc, host_owner) = ctx.db.with_snapshot(|snap| {
        let stored = snap.items.get(&stored_id).cloned();
        let host = stored.as_ref().and_then(|s| snap.items.get(&s.location_id).cloned());
        (stored, host.as_ref().map(|h| h.type_id), host.as_ref().map(|h| h.location_id), host.as_ref().map(|h| h.owner_id).unwrap_or(0))
    });
    let stored = stored.ok_or_else(|| err("ShipNotFound", format!("Stored ship {stored_id} not found")))?;
    let is_ship = ctx.sde.types.get(&stored.type_id).is_some_and(|t| t.category_id == 6);
    if !is_ship || !stored.singleton {
        return Err(err("ShipNotBoardable", format!("Item {stored_id} is not an assembled ship")));
    }
    let host_id = if host_arg != 0 { host_arg } else { stored.location_id };
    let is_feb = (stored.flag as u16) == 179 || custom_flag == Some(179);
    let in_bay = is_ship_bay_flag(stored.flag as u16) || custom_flag.is_some_and(is_ship_bay_flag) || is_feb;
    if stored.location_id != host_id || !in_bay {
        return Err(err("ShipNotInMaintenanceBay", format!("Ship {stored_id} is not stored in the ship maintenance bay of {host_id}")));
    }
    let host_is_ship = host_type.and_then(|t| ctx.sde.types.get(&t)).is_some_and(|t| t.category_id == 6);
    let host_is_structure = ctx.db.with_snapshot(|s| s.structures.contains_key(&host_id));
    if !host_is_ship && !host_is_structure {
        return Err(err("ShipNotInMaintenanceBay", format!("{host_id} has no ship maintenance bay")));
    }
    if is_feb {
        let feb_cap = host_type.and_then(|t| ctx.sde.types.get(&t)).and_then(|t| t.default_attributes.get(&3020).copied()).unwrap_or(0.0);
        if feb_cap <= 0.0 {
            return Err(err("NoFrigateEscapeBay", format!("{host_id} has no Frigate Escape Bay")));
        }
        let stored_grp = ctx.sde.types.get(&stored.type_id).map(|t| t.group_id).unwrap_or(0);
        if !matches!(stored_grp, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001) {
            return Err(err("InvalidFrigateGroup", format!("Stored ship {stored_id} in FEB is not a frigate")));
        }
    }
    if host_is_ship {
        let has_bay_access = ctx.db.with_snapshot(|snap| {
            fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                snap, char_id, host_id, host_owner, true, _supplied_password,
            )
        });
        if !has_bay_access {
            return Err(err("SMAAccessDenied", "Ship Maintenance Bay access denied by ship configuration"));
        }
    }
    let corp_id = session.corporation_id.unwrap_or(0);
    let can_board = stored.owner_id == char_id
        || (corp_id != 0 && stored.owner_id == corp_id && (session.corp_role & eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG != 0));
    if !can_board {
        return Err(err("OwnershipViolation", format!("Ship {stored_id} is not owned by character {char_id}")));
    }
    ctx.db.with_snapshot(|snap| {
        ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, stored.type_id)
    })?;
    let prev_ship_id = session.ship_id;
    if prev_ship_id == Some(host_id) {
        return Err(err("CannotBoardFromOwnBay", "Cannot board a ship stored inside the ship you are currently piloting"));
    }
    let mut launch_pos = None;
    match dock_id {
        Some(dock) => {
            if !(host_id == dock || host_loc == Some(dock)) {
                return Err(err("ShipOutOfBoardingRange", format!("Bay host {host_id} is not at your dock {dock}")));
            }
        }
        None => {
            let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no solarSystemID"))?;
            let cur = prev_ship_id.ok_or_else(|| err("NoActiveShip", "No active ship in space"))?;
            if ctx
                .crimewatch
                .read()
                .ok()
                .and_then(|cw| cw.states.get(&char_id).map(|st| st.is_weapon_timer_active(ctx.now_unix())))
                .unwrap_or(false)
            {
                return Err(err("UserError", "WeaponsTimerActive"));
            }
            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Ok(Some((_, cb))) = sys.get_ball_snapshot(cur).await {
                if matches!(cb.mode, eve_destiny::MovementMode::Warp { .. }) {
                    return Err(err("ShipInWarp", "Cannot board stored ship while in warp"));
                }
            }
            let is_scrambled = {
                let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
                bp.combat_states.get(&cur).is_some_and(|cs| cs.warp_scramble_strength > 0)
            };
            if is_scrambled {
                return Err(err("WarpScrambled", "Cannot board stored ship while warp scrambled"));
            }
            let cur_pos = sys.get_ball_snapshot(cur).await.ok().flatten().map(|(_, cb)| cb.position)
                .or_else(|| ctx.db.with_snapshot(|s| s.item_positions.get(&cur).map(|p| DVec3::new(p[0], p[1], p[2]))));
            let host_snap = sys.get_ball_snapshot(host_id).await.ok().flatten().map(|(_, hb)| (hb.position, hb.radius))
                .or_else(|| ctx.db.with_snapshot(|s| {
                    let in_space = s.items.get(&host_id).is_some_and(|i| i.location_id == sys_id as i64)
                        || s.structures.get(&host_id).is_some_and(|st| st.solar_system_id == sys_id);
                    if !in_space { return None; }
                    let rad = s.items.get(&host_id).and_then(|i| ctx.sde.types.get(&i.type_id)).map(|t| t.radius).unwrap_or(50.0);
                    s.item_positions.get(&host_id).map(|p| (DVec3::new(p[0], p[1], p[2]), rad))
                }));
            let (Some(cur_pos), Some((host_pos, host_radius))) = (cur_pos, host_snap) else {
                return Err(err("ShipNotInSpace", format!("Bay host {host_id} is not on grid")));
            };
            let dist = cur_pos.distance(host_pos);
            if dist > BAY_ACCESS_RANGE_M {
                return Err(err("ShipOutOfBoardingRange", format!("Bay host {host_id} is {dist:.0} m away (must be <= {BAY_ACCESS_RANGE_M} m)")));
            }
            let pilot_ship = session.ship_id.or(prev_ship_id).unwrap_or(0);
            check_pos_forcefield_coverage(ctx, sys_id, char_id, pilot_ship, &[cur_pos, host_pos]).await?;
            launch_pos = Some(host_pos + DVec3::new(host_radius + 300.0, 0.0, 0.0));
        }
    }
    let capsule_group = ctx.sde.groups.values().find(|g| g.name == "Capsule").map(|g| g.group_id);
    let (removed_capsule, prev_is_capsule) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&stored_id) {
            s.owner_id = char_id;
            s.location_id = dock_id.or(session.solar_system_id.map(|v| v as i64)).unwrap_or(s.location_id);
            s.flag = if dock_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None };
            s.version += 1;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = stored_id;
        }
        let is_cap = prev_ship_id
            .and_then(|p| snap.items.get(&p))
            .and_then(|i| ctx.sde.types.get(&i.type_id))
            .is_some_and(|t| Some(t.group_id) == capsule_group);
        let rem = if dock_id.is_none() && is_cap {
            prev_ship_id.and_then(|p| snap.items.remove(&p)).map(|i| i.item_id)
        } else {
            None
        };
        (rem, is_cap)
    });
    write_container_ship_state(ctx, |st| {
        st.custom_item_flags.remove(&stored_id);
    });
    if let (Some(pos), Some(sys_id)) = (launch_pos, session.solar_system_id) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(stored_id, [pos.x, pos.y, pos.z]);
        });
        let sys = ctx.systems.get_or_spawn(sys_id);
        if let Some(cap) = removed_capsule {
            let _ = sys.remove_ball(cap).await;
        } else if let Some(old_id) = prev_ship_id {
            if !prev_is_capsule {
                crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, old_id);
                crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, old_id, sys_id);
                let meta_opt = {
                    let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
                    if let Some(meta) = bp.ball_slim_meta.get_mut(&old_id) {
                        meta.char_id = None;
                        Some(meta.clone())
                    } else {
                        None
                    }
                };
                if let Some(meta) = meta_opt {
                    let _ = sys.change_slim_item(old_id, meta).await;
                }
                let slim_ev = eve_destiny::DestinyEvent::OnSlimItemChange { ball_id: old_id };
                let _ = sys.emit_destiny_event(stored_id, slim_ev).await;
            }
        }
        let t = ctx.sde.types.get(&stored.type_id);
        let ball = eve_destiny::Ball {
            id: stored_id,
            type_id: stored.type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: t.map(|t| t.mass).unwrap_or(1.0),
            radius: t.map(|t| t.radius).unwrap_or(50.0),
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let cs = ctx.build_ship_combat_state(char_id, stored_id, stored.type_id);
        let meta = ctx.build_ship_slim_meta(char_id, stored.type_id);
        let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
    }
    let old_ship = prev_ship_id.unwrap_or(0);
    session.ship_id = Some(stored_id);
    ctx.invalidate_ship_fitting(stored_id);
    if old_ship != 0 {
        ctx.invalidate_ship_fitting(old_ship);
    }
    let new_loc_id = dock_id.or(session.solar_system_id.map(|v| v as i64)).unwrap_or(stored.location_id);
    let new_flag = if dock_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None };
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnItemChange",
        "charid",
        vec![
            PyRep::keyval([
                ("itemID", PyRep::Integer(stored_id)),
                ("typeID", PyRep::Integer(stored.type_id as i64)),
                ("locationID", PyRep::Integer(new_loc_id)),
                ("flagID", PyRep::Integer(new_flag as i64)),
                ("quantity", PyRep::Integer(1)),
            ]),
            PyRep::Dict(vec![
                (PyRep::Integer(3), PyRep::Integer(stored.location_id)),
                (PyRep::Integer(4), PyRep::Integer(stored.flag as i64)),
            ]),
        ],
    );
    let changes = vec![(
        PyRep::String("shipid".into()),
        PyRep::Tuple(vec![PyRep::Integer(old_ship), PyRep::Integer(stored_id)]),
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
    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(stored_id)),
        ("typeID", PyRep::Integer(stored.type_id as i64)),
        ("hostID", PyRep::Integer(host_id)),
        ("oldShipID", prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ]))
}

pub(crate) use super::fitting_ship_forcefield::{
    is_deployable_item, is_infrastructure_item, is_mobile_depot,
};

fn is_jettisonable_flag(flag: InventoryFlag) -> bool {
    use InventoryFlag::*;
    matches!(flag, Cargo | DroneBay | FighterBay | SpecializedFuelBay | SpecializedOreHold | SpecializedGasHold
        | SpecializedMineralHold | SpecializedSalvageHold | SpecializedShipHold | SpecializedAmmoHold
        | SpecializedCommandCenterHold | SpecializedPlanetaryCommoditiesHold | SpecializedMaterialBay
        | FleetHangar | SpecializedMobileDepotHold | SpecializedIceHold | SpecializedAsteroidHold | SpecializedInfrastructureHold)
}
pub(crate) async fn ship_jettison(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("CannotJettisonWhileDocked", "Jettison can only be performed in space"));
    }
    let sys_id = session.solar_system_id.ok_or_else(|| err("NotInSpace", "Jettison can only be performed in space"))?;
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Jettison requires an active ship"))?;
    let mut item_ids: Vec<i64> = Vec::new();
    let parse_ids = |rep: &PyRep| match rep {
        PyRep::List(l) | PyRep::Tuple(l) => l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>(),
        _ => rep.as_i64().into_iter().collect(),
    };
    if let Some(first) = args.first() {
        item_ids.extend(parse_ids(first));
    }
    if item_ids.is_empty() {
        if let Some(items) = kwargs.get("itemIDs").or_else(|| kwargs.get("itemID")) {
            item_ids.extend(parse_ids(items));
        }
    }
    if item_ids.is_empty() {
        return Err(err("InvalidArgument", "Jettison requires at least one itemID"));
    }
    let now_unix = ctx.now_unix_secs();
    let note_key = -10_000_018 - char_id;
    let last_jettison = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .get(&note_key)
            .and_then(|n| n.note.parse::<u64>().ok())
    });
    if let Some(last) = last_jettison {
        if now_unix < last.saturating_add(180) {
            return Err(err(
                "JettisonCooldownActive",
                format!(
                    "Jettison is on 180s cooldown ({}s remaining)",
                    last.saturating_add(180).saturating_sub(now_unix)
                ),
            ));
        }
    }
    let jetcan_id = ctx.db.try_with_mut_snapshot(|snap| {
        let mut total_vol = 0.0f64;
        for &iid in &item_ids {
            let Some(it) = snap.items.get(&iid) else {
                return Err(err("ItemNotFound", format!("Item {iid} not found")));
            };
            if it.owner_id != char_id || it.location_id != ship_id {
                return Err(err("ItemNotOnShip", format!("Item {iid} is not in active ship {ship_id}")));
            }
            if matches!(it.flag as u16, 11..=34 | 92..=99 | 125..=132 | 164..=171) {
                return Err(err(
                    "CannotJettisonFittedModule",
                    format!("Item {iid} is fitted and cannot be jettisoned"),
                ));
            }
            if it.flag == InventoryFlag::ShipHangar {
                return Err(err(
                    "CannotJettisonStoredShip",
                    format!("Item {iid} is stored in a ship hangar and cannot be jettisoned"),
                ));
            }
            if snap.item_custom_names.get(&iid).is_some_and(|s| s.starts_with("courier_cargo:")) {
                return Err(err("ItemCannotBeJettisoned", "Courier mission items cannot be jettisoned into space"));
            }
            if !is_jettisonable_flag(it.flag) {
                return Err(err(
                    "CannotJettisonItem",
                    format!("Item {iid} with flag {:?} cannot be jettisoned", it.flag),
                ));
            }
            if let Some(t) = ctx.sde.types.get(&it.type_id) {
                if t.category_id == 6 {
                    return Err(err(
                        "CannotJettisonShip",
                        format!("Ship {} cannot be jettisoned into space", t.name),
                    ));
                }
                if fitting_containers_ships::is_container_group(t.group_id, it.type_id) {
                    return Err(err(
                        "CannotPutContainerInContainer",
                        format!("Container {} cannot be placed inside a jetcan", t.name),
                    ));
                }
                if t.group_id == 448 && t.volume >= 100_000.0 {
                    return Err(err(
                        "CannotJettisonStationContainer",
                        format!("Station Container {} cannot be jettisoned into space", t.name),
                    ));
                }
            }
            let item_vol = fitting_containers_ships::effective_item_unit_volume(&ctx.sde, it) * (it.quantity as f64);
            total_vol += item_vol;
        }
        let jetcan_cap = ctx.sde.types.get(&23).map(|t| t.capacity).unwrap_or(27_500.0);
        if total_vol > jetcan_cap + 1e-4 {
            return Err(err(
                "CapacityExceeded",
                format!("Items total volume ({total_vol:.1} m³) exceeds jetcan capacity ({jetcan_cap:.1} m³)"),
            ));
        }
        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
            snap, jid, now_unix, false,
        );
        snap.items.insert(
            jid,
            eve_db::InventoryItemRecord {
                item_id: jid,
                type_id: 23,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = jid;
                it.flag = InventoryFlag::Cargo;
                it.version += 1;
            }
        }
        snap.owner_notes.insert(
            note_key,
            eve_db::OwnerNoteRecord {
                note_id: note_key,
                character_id: char_id,
                item_id: 0,
                label: "S:JETTISON_COOLDOWN".into(),
                note: now_unix.to_string(),
                timestamp_unix: now_unix,
            },
        );
        Ok(jid)
    })?;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);
    let jetcan_pos = ship_pos + DVec3::new(500.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(jetcan_id, [jetcan_pos.x, jetcan_pos.y, jetcan_pos.z]);
    });
    crate::services::combat_crimewatch::register_container_lifetime(ctx, jetcan_id, sys_id, 23, char_id, now_unix, false);
    let _ = sys.spawn_ball_with_meta(
        eve_destiny::Ball {
            id: jetcan_id, type_id: 23, position: jetcan_pos, velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 500.0, radius: 15.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            group_id: 12, category_id: 2, owner_id: char_id, char_id: None,
            corp_id: session.corporation_id.unwrap_or(-1), alliance_id: session.alliance_id.unwrap_or(-1),
            security_status: 0.0, is_cloaked: false,
        },
    ).await;
    Ok(PyRep::Tuple(vec![
        PyRep::List(vec![PyRep::Integer(jetcan_id)]),
        PyRep::List(item_ids.into_iter().map(PyRep::Integer).collect()),
    ]))
}
pub(crate) async fn ship_drop(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("CannotDropWhileDocked", "Drop can only be performed in space"));
    }
    let sys_id = session.solar_system_id.ok_or_else(|| err("NotInSpace", "Drop can only be performed in space"))?;
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Drop requires an active ship"))?;
    let mut items_to_drop: Vec<(i64, Option<i32>)> = Vec::new();
    let parse_el = |el: &PyRep| -> Option<(i64, Option<i32>)> {
        if let Some(id) = el.as_i64() {
            Some((id, None))
        } else {
            let slice = match el {
                PyRep::Tuple(s) | PyRep::List(s) => s.as_slice(),
                _ => return None,
            };
            let id = slice.first().and_then(|v| v.as_i64())?;
            let qty = slice.get(1).and_then(|v| v.as_i64()).map(|q| i32::try_from(q).unwrap_or(i32::MAX));
            Some((id, qty))
        }
    };
    if let Some(first) = args.first() {
        match first {
            PyRep::List(l) | PyRep::Tuple(l) => items_to_drop.extend(l.iter().filter_map(parse_el)),
            _ => items_to_drop.extend(parse_el(first)),
        }
    }
    if items_to_drop.is_empty() {
        if let Some(items) = kwargs.get("itemIDs").or_else(|| kwargs.get("itemID")) {
            match items {
                PyRep::List(l) | PyRep::Tuple(l) => items_to_drop.extend(l.iter().filter_map(parse_el)),
                _ => items_to_drop.extend(parse_el(items)),
            }
        }
    }
    if items_to_drop.is_empty() {
        return Err(err("InvalidArgument", "Drop requires at least one itemID"));
    }
    let drop_owner = args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id);
    let session_corp = session.corporation_id.unwrap_or(0);
    if drop_owner != char_id && (session_corp == 0 || drop_owner != session_corp) {
        return Err(err("UserError", "Cannot drop item with foreign owner"));
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    let dropped_items: Vec<(i64, i64, u32, DVec3)> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut raw_list = Vec::new();
        for &(iid, qty_opt) in &items_to_drop {
            let Some(it) = snap.items.get(&iid) else {
                return Err(err("ItemNotFound", format!("Item {iid} not found")));
            };
            if it.owner_id != char_id || it.location_id != ship_id {
                return Err(err("ItemNotOnShip", format!("Item {iid} is not in active ship {ship_id}")));
            }
            if matches!(it.flag as u16, 11..=34 | 92..=99 | 125..=132 | 164..=171) {
                return Err(err("CannotDropFittedModule", format!("Item {iid} is fitted in a module slot")));
            }
            if it.flag == InventoryFlag::ShipHangar {
                return Err(err("CannotDropStoredShip", format!("Item {iid} is in a ship hangar and cannot be dropped")));
            }
            if !is_deployable_item(&ctx.sde, it.type_id) {
                return Err(err("UserError", format!("Item {iid} (type {}) is not a deployable item", it.type_id)));
            }
            if let Some(t) = ctx.sde.types.get(&it.type_id) {
                if t.category_id == 6 {
                    return Err(err("UserError", format!("Ship {iid} cannot be dropped as a deployable")));
                }
                let disallow_in_empire = t.default_attributes.get(&1074).copied().unwrap_or(0.0) == 1.0;
                if (t.group_id == 361 || disallow_in_empire) && sys_sec > 0.0 {
                    return Err(err("UserError", format!("Item {} cannot be deployed in empire space (system security {sys_sec:.2} > 0.0)", t.name)));
                }
                if let Some(&max_sec) = t.default_attributes.get(&1032) {
                    if sys_sec > max_sec + 1e-4 {
                        return Err(err("UserError", format!("Item {} cannot be anchored in security {sys_sec:.2} > max {max_sec:.2}", t.name)));
                    }
                }
                if let Some(&min_sec) = t.default_attributes.get(&1946) {
                    if min_sec > -1.0 && sys_sec < min_sec - 1e-4 {
                        return Err(err("UserError", format!("Item {} cannot be anchored in security {sys_sec:.2} < min {min_sec:.2}", t.name)));
                    }
                }
            }
            if let Some(qty) = qty_opt {
                if qty <= 0 { return Err(err("UserError", "Drop quantity must be greater than zero")); }
                if qty > it.quantity { return Err(err("UserError", "Drop quantity cannot exceed item quantity")); }
            }
            raw_list.push((iid, it.type_id, qty_opt));
        }
        for g in ctx.sde.stargates().values() {
            if g.from_solar_system_id == sys_id && ship_pos.distance(g.position) < 5_000.0 {
                return Err(err("UserError", "Cannot deploy structure too close to a stargate (minimum distance is 5,000 m)"));
            }
        }
        let mut dropped_items = Vec::new();
        for (iid, tid, qty_opt) in raw_list {
            let (orig_qty, orig_singleton) = {
                let Some(orig_item) = snap.items.get(&iid) else { continue; };
                (orig_item.quantity, orig_item.singleton)
            };
            let drop_qty = qty_opt.unwrap_or(orig_qty);
            if drop_qty <= 0 { return Err(err("UserError", "Drop quantity must be greater than zero")); }
            if drop_qty > orig_qty { return Err(err("UserError", "Drop quantity cannot exceed item quantity")); }

            let spawn_pos = ship_pos + DVec3::new(100.0 + ((dropped_items.len() as f64) * 50.0), 0.0, 0.0);
            let launched_id = if orig_qty == drop_qty {
                let it = snap.items.get_mut(&iid).unwrap();
                it.location_id = sys_id as i64;
                it.flag = InventoryFlag::None;
                it.singleton = orig_singleton;
                it.owner_id = drop_owner;
                it.version += 1;
                iid
            } else {
                let it = snap.items.get_mut(&iid).unwrap();
                it.quantity -= drop_qty;
                it.version += 1;
                snap.next_entity_id += 1;
                let new_id = snap.next_entity_id;
                snap.items.insert(
                    new_id,
                    eve_db::InventoryItemRecord {
                        item_id: new_id,
                        type_id: tid,
                        owner_id: drop_owner,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::None,
                        quantity: drop_qty,
                        singleton: orig_singleton,
                        version: 1,
                        ..Default::default()
                    },
                );
                new_id
            };
            snap.item_positions.insert(launched_id, [spawn_pos.x, spawn_pos.y, spawn_pos.z]);
            dropped_items.push((iid, launched_id, tid, spawn_pos));
        }
        Ok(dropped_items)
    })?;
    for &(_orig_id, launched_id, tid, spawn_pos) in &dropped_items {
        let t = ctx.sde.types.get(&tid);
        let _ = sys
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: launched_id,
                    type_id: tid,
                    position: spawn_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: t.map(|t| t.mass.max(100.0)).unwrap_or(10_000.0),
                    radius: t.map(|t| t.radius.max(10.0)).unwrap_or(30.0),
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
                eve_destiny::SlimItemMeta {
                    group_id: t.map(|t| t.group_id).unwrap_or(1246),
                    category_id: t.map(|t| t.category_id).unwrap_or(22),
                    owner_id: drop_owner,
                    char_id: None,
                    corp_id: session.corporation_id.unwrap_or(-1),
                    alliance_id: session.alliance_id.unwrap_or(-1),
                    security_status: 0.0,
                    is_cloaked: false,
                },
            )
            .await;
    }
    let mut map: HashMap<i64, Vec<PyRep>> = HashMap::new();
    for &(orig_id, launched_id, _, _) in &dropped_items {
        map.entry(orig_id).or_default().push(PyRep::Integer(launched_id));
    }
    let dict_entries = map.into_iter().map(|(orig_id, ids)| (PyRep::Integer(orig_id), PyRep::List(ids))).collect();
    Ok(PyRep::Dict(dict_entries))
}
pub(crate) async fn ship_scoop_and_store(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let dock_id = session.station_id.or(session.structure_id);
    let supplied_password = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
    if method == "StoreVessel" {
        let (target_ship_id, host_ship_id) = if args.len() >= 2 {
            (args[0].as_i64().unwrap_or(0), args[1].as_i64().unwrap_or(0))
        } else {
            (session.ship_id.unwrap_or(0), args.first().and_then(|a| a.as_i64()).unwrap_or(0))
        };
        if target_ship_id == 0 || host_ship_id == 0 {
            return Err(err("InvalidArgument", "StoreVessel requires valid target and host IDs"));
        }
        if let Some(dock) = dock_id {
            let caller_corp = session.corporation_id.unwrap_or(0);
            return ctx.db.try_with_mut_snapshot(|snap| {
                if target_ship_id == host_ship_id { return Err(err("UserError", "CannotStoreSelf")); }
                if Some(target_ship_id) == session.ship_id || snap.characters.values().any(|c| c.active_ship_id == target_ship_id) {
                    return Err(err("UserError", "CannotStoreActiveShip"));
                }
                let Some(target) = snap.items.get(&target_ship_id) else {
                    return Err(err("ShipNotFound", format!("Target ship {target_ship_id} not found")));
                };
                let Some(host) = snap.items.get(&host_ship_id) else {
                    return Err(err("ShipNotFound", format!("Host ship {host_ship_id} not found")));
                };
                if target.location_id != dock || host.location_id != dock {
                    return Err(err("UserError", "ShipNotInSameHangar"));
                }
                let t_type = ctx.sde.types.get(&target.type_id).ok_or_else(|| err("InvalidType", "Unknown type"))?;
                if t_type.category_id != 6 || target.type_id == 670 {
                    return Err(err("UserError", "OnlyShipsCanBeStoredInSMA"));
                }
                let check_corp_role = || -> Result<(), BridgeRpcError> {
                    let has_role = (session.corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER)) != 0
                        || snap.corporations.get(&caller_corp).map(|c| c.ceo_id == char_id).unwrap_or(false)
                        || snap.corporations.get(&caller_corp).and_then(|c| c.member_roles.get(&char_id)).map(|&r| (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER)) != 0).unwrap_or(false);
                    if !has_role { return Err(err("CorpRoleDenied", "Corporation-owned ship storage requires Director or Station Manager role")); }
                    Ok(())
                };
                let is_corp = caller_corp > 0 && target.owner_id == caller_corp;
                if target.owner_id != char_id && !is_corp {
                    return Err(err("OwnershipViolation", "Cannot store another player's ship"));
                }
                if is_corp { check_corp_role()?; }
                if !target.singleton {
                    return Err(err("OnlyAssembledShipsInSMA", "Only assembled ships can be stored in a Ship Maintenance Bay"));
                }
                let has_non_charge = snap.items.values().any(|child| {
                    if child.location_id != target_ship_id || child.quantity <= 0 { return false; }
                    let cf = child.flag as u16;
                    if matches!(cf, 11..=34 | 87 | 92..=94 | 125..=132 | 158) { return false; }
                    let c_cat = ctx.sde.types.get(&child.type_id).map(|t| t.category_id).unwrap_or(0);
                    c_cat != 8
                });
                if has_non_charge {
                    return Err(err("ShipHasNonChargeCargo", "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo"));
                }
                if !fitting_containers_ships::can_character_access_ship_bay_with_password_snap(snap, char_id, host_ship_id, host.owner_id, true, supplied_password) {
                    return Err(err("SMAAccessDenied", "Ship Maintenance Bay access denied by ship configuration"));
                }
                let Some(host_type) = ctx.sde.types.get(&host.type_id) else {
                    return Err(err("TypeNotFound", "Host ship type not found"));
                };
                let sma_cap = host_type.default_attributes.get(&908).copied().unwrap_or(0.0);
                let feb_cap = host_type.default_attributes.get(&3020).copied().unwrap_or(0.0);
                if sma_cap <= 0.0 && feb_cap <= 0.0 {
                    return Err(err("NoShipMaintenanceBay", "Host does not have a Ship Maintenance Bay or Frigate Escape Bay"));
                }
                let is_frigate = matches!(t_type.group_id, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001);
                let store_in_feb = feb_cap > 0.0 && (sma_cap <= 0.0 || is_frigate);
                let dest_flag = if store_in_feb {
                    if !is_frigate {
                        return Err(err("InvalidFrigateGroup", "Frigate Escape Bay only accepts frigates"));
                    }
                    let existing_feb = snap.items.values().any(|i| {
                        i.location_id == host_ship_id && (i.flag as u16) == 179 && i.item_id != target_ship_id
                    });
                    if existing_feb {
                        return Err(err("CapacityExceeded", "Frigate Escape Bay can hold at most 1 assembled frigate"));
                    }
                    InventoryFlag::FrigateEscapeBay
                } else {
                    let target_vol = fitting_containers_ships::effective_item_unit_volume(&ctx.sde, target) * (target.quantity as f64);
                    let used_sma: f64 = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == host_ship_id && matches!(i.flag as u16, 90 | 138) && i.item_id != target_ship_id)
                        .map(|i| fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                        .sum();
                    if used_sma + target_vol > sma_cap + 1e-4 {
                        return Err(err("CapacityExceeded", "Ship Maintenance Bay capacity exceeded"));
                    }
                    InventoryFlag::ShipHangar
                };
                if let Some(target_mut) = snap.items.get_mut(&target_ship_id) {
                    target_mut.location_id = host_ship_id;
                    target_mut.flag = dest_flag;
                    target_mut.version += 1;
                }
                Ok(PyRep::keyval([
                    ("storedShipID", PyRep::Integer(target_ship_id)),
                    ("hostShipID", PyRep::Integer(host_ship_id)),
                    ("flagID", PyRep::Integer(dest_flag as i64)),
                ]))
            });
        }
        let active_ship_id = session.ship_id.unwrap_or(0);
        if target_ship_id == host_ship_id {
            return Err(err("UserError", "CannotStoreShipInItself"));
        }
        let sys_id = session.solar_system_id.ok_or_else(|| err("NotInSpace", "StoreVessel in space requires solarSystemID"))?;
        let sys = ctx.systems.get_or_spawn(sys_id);
        let caller_pos = sys.get_ball_snapshot(active_ship_id).await.ok().flatten().map(|(_, b)| b.position);
        let host_pos = sys.get_ball_snapshot(host_ship_id).await.ok().flatten().map(|(_, b)| b.position);
        let (Some(cp), Some(hp)) = (caller_pos, host_pos) else {
            return Err(err("ShipNotInSpace", format!("Host {host_ship_id} or active ship {active_ship_id} is not on grid")));
        };
        let caller_host_dist = cp.distance(hp);
        if caller_host_dist > BAY_ACCESS_RANGE_M {
            return Err(err(
                "OutOfRange",
                format!("Bay host {host_ship_id} is {caller_host_dist:.0} m away from caller (must be <= {BAY_ACCESS_RANGE_M} m)"),
            ));
        }
        check_pos_forcefield_coverage(ctx, sys_id, char_id, active_ship_id, &[cp, hp]).await?;
        if target_ship_id == active_ship_id {
            if ctx.crimewatch.read().ok().and_then(|cw| cw.states.get(&char_id).map(|st| st.is_weapon_timer_active(ctx.now_unix()))).unwrap_or(false) {
                return Err(err("UserError", "WeaponsTimerActive"));
            }
            let pod_pos = cp + DVec3::new(100.0, 0.0, 0.0);
            let (capsule_id, res_flag_id) = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(target) = snap.items.get(&target_ship_id) else {
                    return Err(err("ShipNotFound", format!("Active ship {target_ship_id} not found")));
                };
                if target.owner_id != char_id {
                    return Err(err("OwnershipViolation", "Cannot store another player's ship"));
                }
                if target.type_id == 23 { return Err(err("UserError", "Cargo Containers cannot be scooped")); }
                if target.location_id != sys_id as i64 {
                    return Err(err("ObjectNotInSpace", format!("Active ship {target_ship_id} is not in space")));
                }
                if target.type_id == 670 {
                    return Err(err("CannotStoreCapsuleInSMA", "Capsules cannot be stored in a Ship Maintenance Bay"));
                }
                let Some(target_type) = ctx.sde.types.get(&target.type_id) else {
                    return Err(err("TypeNotFound", "Target ship type not found"));
                };
                if target_type.category_id != 6 {
                    return Err(err("CannotStoreCargoInSMA", "Only ships can be stored in a Ship Maintenance Bay"));
                }
                if !target.singleton {
                    return Err(err("OnlyAssembledShipsInSMA", "Only assembled ships can be stored in a Ship Maintenance Bay"));
                }
                let has_non_charge = snap.items.values().any(|child| {
                    if child.location_id != target_ship_id || child.quantity <= 0 {
                        return false;
                    }
                    let cf = child.flag as u16;
                    if matches!(cf, 11..=34 | 87 | 92..=94 | 125..=132 | 158) {
                        return false;
                    }
                    let c_cat = ctx.sde.types.get(&child.type_id).map(|t| t.category_id).unwrap_or(0);
                    c_cat != 8
                });
                if has_non_charge {
                    return Err(err("ShipHasNonChargeCargo", "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo"));
                }
                let Some(host) = snap.items.get(&host_ship_id) else {
                    return Err(err("ShipNotFound", format!("Host ship {host_ship_id} not found")));
                };
                if host.location_id != sys_id as i64 {
                    return Err(err("ObjectNotInSpace", format!("Host ship {host_ship_id} is not in space")));
                }
                let Some(host_type) = ctx.sde.types.get(&host.type_id) else {
                    return Err(err("TypeNotFound", "Host ship type not found"));
                };
                if host_type.category_id != 6 {
                    return Err(err("CannotStoreCargoInSMA", "Host is not a ship"));
                }
                let sma_cap = host_type.default_attributes.get(&908).copied().unwrap_or(0.0);
                let feb_cap = host_type.default_attributes.get(&3020).copied().unwrap_or(0.0);
                if sma_cap <= 0.0 && feb_cap <= 0.0 {
                    return Err(err("NoShipMaintenanceBay", format!("Ship {} does not have a Ship Maintenance Bay or Frigate Escape Bay", host_type.name)));
                }
                if !fitting_containers_ships::can_character_access_ship_bay_with_password_snap(snap, char_id, host_ship_id, host.owner_id, true, supplied_password) {
                    return Err(err("SMAAccessDenied", "Ship Maintenance Bay access denied by ship configuration"));
                }
                let is_frigate = matches!(target_type.group_id, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001);
                let store_in_feb = feb_cap > 0.0 && (sma_cap <= 0.0 || is_frigate);
                let dest_flag = if store_in_feb {
                    if !is_frigate {
                        return Err(err("InvalidFrigateGroup", "Frigate Escape Bay only accepts frigates"));
                    }
                    let existing_feb = snap.items.values().any(|i| {
                        i.location_id == host_ship_id && (i.flag as u16) == 179 && i.item_id != target_ship_id
                    });
                    if existing_feb {
                        return Err(err("CapacityExceeded", "Frigate Escape Bay can hold at most 1 assembled frigate"));
                    }
                    InventoryFlag::FrigateEscapeBay
                } else {
                    let target_vol = fitting_containers_ships::effective_item_unit_volume(&ctx.sde, target) * (target.quantity as f64);
                    let used_sma: f64 = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == host_ship_id && matches!(i.flag as u16, 90 | 138) && i.item_id != target_ship_id)
                        .map(|i| fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                        .sum();
                    if used_sma + target_vol > sma_cap + 1e-4 {
                        return Err(err("CapacityExceeded", format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded")));
                    }
                    InventoryFlag::ShipHangar
                };
                if let Some(target_mut) = snap.items.get_mut(&target_ship_id) {
                    target_mut.location_id = host_ship_id;
                    target_mut.flag = dest_flag;
                    target_mut.version += 1;
                }
                snap.item_positions.remove(&target_ship_id);
                let existing_cap = snap.items.values().find(|it| {
                    it.type_id == 670 && it.owner_id == char_id
                        && it.location_id == sys_id as i64
                        && !snap.characters.values().any(|c| c.active_ship_id == it.item_id)
                }).map(|it| it.item_id);
                let cap_id = if let Some(cid) = existing_cap {
                    if let Some(cap) = snap.items.get_mut(&cid) {
                        cap.location_id = sys_id as i64;
                        cap.flag = InventoryFlag::Pilot;
                        cap.singleton = true;
                        cap.version += 1;
                    }
                    cid
                } else {
                    snap.next_entity_id += 1;
                    let cid = snap.next_entity_id;
                    snap.items.insert(
                        cid,
                        eve_db::InventoryItemRecord {
                            item_id: cid,
                            type_id: 670,
                            owner_id: char_id,
                            location_id: sys_id as i64,
                            flag: InventoryFlag::Pilot,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    cid
                };
                snap.item_positions.insert(cap_id, [pod_pos.x, pod_pos.y, pod_pos.z]);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.active_ship_id = cap_id;
                }
                Ok((cap_id, dest_flag as i64))
            })?;
            let _ = sys.remove_ball(target_ship_id).await;
            let cap_t = ctx.sde.types.get(&670);
            let cap_mass = cap_t.map(|t| t.mass).unwrap_or(16_000.0);
            let cap_radius = cap_t.map(|t| t.radius).unwrap_or(22.0);
            let cap_speed = cap_t.and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(350.0);
            let cap_combat = ctx.build_ship_combat_state(char_id, capsule_id, 670);
            let _ = sys.spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: capsule_id, type_id: 670, position: pod_pos, velocity: DVec3::ZERO,
                    max_velocity: cap_speed, inertia: 1.0, mass: cap_mass, radius: cap_radius,
                    speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
                },
                cap_combat,
                eve_destiny::SlimItemMeta {
                    group_id: 29, category_id: 6, owner_id: char_id, char_id: Some(char_id),
                    corp_id: session.corporation_id.unwrap_or(-1), alliance_id: session.alliance_id.unwrap_or(-1),
                    security_status: 0.0, is_cloaked: false,
                },
            ).await;
            session.ship_id = Some(capsule_id);
            ctx.invalidate_ship_fitting(target_ship_id);
            ctx.invalidate_ship_fitting(capsule_id);
            let changes = vec![(PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::Integer(target_ship_id), PyRep::Integer(capsule_id)]))];
            ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnSessionChanged", "clientID", vec![PyRep::Bool(false), PyRep::Integer(session.session_id as i64), PyRep::Dict(changes)]);
            return Ok(PyRep::keyval([
                ("storedShipID", PyRep::Integer(target_ship_id)),
                ("hostShipID", PyRep::Integer(host_ship_id)),
                ("capsuleID", PyRep::Integer(capsule_id)),
                ("flagID", PyRep::Integer(res_flag_id)),
            ]));
        } else {
            if ctx.crimewatch.read().unwrap().states.get(&char_id).is_some_and(|st| st.is_weapon_timer_active(ctx.now_unix())) {
                return Err(err("UserError", "WeaponsTimerActive"));
            }
            let target_pos = sys.get_ball_snapshot(target_ship_id).await.ok().flatten().map(|(_, b)| b.position);
            let Some(tp) = target_pos else {
                return Err(err("ShipNotInSpace", format!("Target ship {target_ship_id} is not on grid")));
            };
            let th_dist = tp.distance(hp);
            if th_dist > BAY_ACCESS_RANGE_M {
                return Err(err("OutOfRange", format!("Target ship is {th_dist:.0} m away from host (must be <= {BAY_ACCESS_RANGE_M} m)")));
            }
            let ct_dist = cp.distance(tp);
            if ct_dist > BAY_ACCESS_RANGE_M {
                return Err(err("OutOfRange", format!("Target ship is {ct_dist:.0} m away from caller (must be <= {BAY_ACCESS_RANGE_M} m)")));
            }
            check_pos_forcefield_coverage(ctx, sys_id, char_id, active_ship_id, &[cp, hp, tp]).await?;
            let res_flag = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(target) = snap.items.get(&target_ship_id) else {
                    return Err(err("ShipNotFound", format!("Target ship {target_ship_id} not found")));
                };
                if target.owner_id != char_id {
                    return Err(err("OwnershipViolation", "Cannot store another player's ship"));
                }
                if target.type_id == 23 { return Err(err("UserError", "Cargo Containers cannot be scooped")); }
                if target.location_id != sys_id as i64 {
                    return Err(err("ObjectNotInSpace", format!("Target ship {target_ship_id} is not in space")));
                }
                if snap.characters.values().any(|c| c.active_ship_id == target_ship_id) {
                    return Err(err("ShipPiloted", "Cannot store a piloted ship"));
                }
                if target.type_id == 670 {
                    return Err(err("CannotStoreCapsuleInSMA", "Capsules cannot be stored in a Ship Maintenance Bay"));
                }
                let Some(target_type) = ctx.sde.types.get(&target.type_id) else {
                    return Err(err("TypeNotFound", "Target ship type not found"));
                };
                if target_type.category_id != 6 {
                    return Err(err("CannotStoreCargoInSMA", "Only ships can be stored in a Ship Maintenance Bay"));
                }
                if !target.singleton {
                    return Err(err("OnlyAssembledShipsInSMA", "Only assembled ships can be stored in a Ship Maintenance Bay"));
                }
                let has_non_charge = snap.items.values().any(|child| {
                    if child.location_id != target_ship_id || child.quantity <= 0 {
                        return false;
                    }
                    let cf = child.flag as u16;
                    if matches!(cf, 11..=34 | 87 | 92..=94 | 125..=132 | 158) {
                        return false;
                    }
                    let c_cat = ctx.sde.types.get(&child.type_id).map(|t| t.category_id).unwrap_or(0);
                    c_cat != 8
                });
                if has_non_charge {
                    return Err(err("ShipHasNonChargeCargo", "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo"));
                }
                let Some(host) = snap.items.get(&host_ship_id) else {
                    return Err(err("ShipNotFound", format!("Host ship {host_ship_id} not found")));
                };
                if host.location_id != sys_id as i64 {
                    return Err(err("ObjectNotInSpace", format!("Host ship {host_ship_id} is not in space")));
                }
                let Some(host_type) = ctx.sde.types.get(&host.type_id) else {
                    return Err(err("TypeNotFound", "Host ship type not found"));
                };
                if host_type.category_id != 6 {
                    return Err(err("CannotStoreCargoInSMA", "Host is not a ship"));
                }
                let sma_cap = host_type.default_attributes.get(&908).copied().unwrap_or(0.0);
                let feb_cap = host_type.default_attributes.get(&3020).copied().unwrap_or(0.0);
                if sma_cap <= 0.0 && feb_cap <= 0.0 {
                    return Err(err("NoShipMaintenanceBay", format!("Ship {} does not have a Ship Maintenance Bay or Frigate Escape Bay", host_type.name)));
                }
                if !fitting_containers_ships::can_character_access_ship_bay_with_password_snap(snap, char_id, host_ship_id, host.owner_id, true, supplied_password) {
                    return Err(err("SMAAccessDenied", "Ship Maintenance Bay access denied by ship configuration"));
                }
                let is_frigate = matches!(target_type.group_id, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001);
                let store_in_feb = feb_cap > 0.0 && (sma_cap <= 0.0 || is_frigate);
                let dest_flag = if store_in_feb {
                    if !is_frigate {
                        return Err(err("InvalidFrigateGroup", "Frigate Escape Bay only accepts frigates"));
                    }
                    let existing_feb = snap.items.values().any(|i| {
                        i.location_id == host_ship_id && (i.flag as u16) == 179 && i.item_id != target_ship_id
                    });
                    if existing_feb {
                        return Err(err("CapacityExceeded", "Frigate Escape Bay can hold at most 1 assembled frigate"));
                    }
                    InventoryFlag::FrigateEscapeBay
                } else {
                    let target_vol = fitting_containers_ships::effective_item_unit_volume(&ctx.sde, target) * (target.quantity as f64);
                    let used_sma: f64 = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == host_ship_id && matches!(i.flag as u16, 90 | 138) && i.item_id != target_ship_id)
                        .map(|i| fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                        .sum();
                    if used_sma + target_vol > sma_cap + 1e-4 {
                        return Err(err("CapacityExceeded", format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded")));
                    }
                    InventoryFlag::ShipHangar
                };
                if let Some(target_mut) = snap.items.get_mut(&target_ship_id) {
                    target_mut.location_id = host_ship_id;
                    target_mut.flag = dest_flag;
                    target_mut.version += 1;
                }
                snap.item_positions.remove(&target_ship_id);
                Ok(dest_flag as i64)
            })?;
            let _ = sys.remove_ball(target_ship_id).await;
            return Ok(PyRep::keyval([("storedShipID", PyRep::Integer(target_ship_id)), ("hostShipID", PyRep::Integer(host_ship_id)), ("flagID", PyRep::Integer(res_flag))]));
        }
    }
    if dock_id.is_some() {
        return Err(err("MustBeInSpace", format!("{method} requires being in space")));
    }
    let sys_id = session.solar_system_id.ok_or_else(|| err("NotInSpace", format!("{method} requires being in space")))?;
    let host_ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Scoop requires an active ship"))?;
    let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if item_id <= 0 {
        return Err(err("InvalidArgument", "Valid itemID required for Scoop"));
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    let host_pos = sys.get_ball_snapshot(host_ship_id).await.ok().flatten().map(|(_, b)| b.position);
    let item_pos = sys.get_ball_snapshot(item_id).await.ok().flatten().map(|(_, b)| b.position);
    let (Some(hp), Some(ip)) = (host_pos, item_pos) else {
        return Err(err("ObjectNotInSpace", format!("Item {item_id} or active ship {host_ship_id} is not in space")));
    };
    let dist = hp.distance(ip);
    if dist > BAY_ACCESS_RANGE_M {
        return Err(err(
            "OutOfRange",
            format!("Item {item_id} is {dist:.0} m away (must be <= {BAY_ACCESS_RANGE_M} m)"),
        ));
    }
    check_pos_forcefield_coverage(ctx, sys_id, char_id, host_ship_id, &[hp, ip]).await?;
    let (target_flag_u16, target_flag) = match method {
        "ScoopToSMA" => (90u16, InventoryFlag::ShipHangar),
        "ScoopToInfrastructureHold" => (185u16, InventoryFlag::SpecializedInfrastructureHold),
        "ScoopToMobileDepotHold" => (179u16, InventoryFlag::SpecializedMobileDepotHold),
        "ScoopToFleetHangar" => (155u16, InventoryFlag::FleetHangar),
        _ => (5u16, InventoryFlag::Cargo),
    };
    let (_transfer_owner, _orig_singleton) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(it) = snap.items.get(&item_id) else {
            return Err(err("ItemNotFound", format!("Item {item_id} not found")));
        };
        let it_type = ctx.sde.types.get(&it.type_id);
        let it_cat = it_type.map(|t| t.category_id).unwrap_or(0);
        let it_grp = it_type.map(|t| t.group_id).unwrap_or(0);
        if it.type_id == 23 {
            return Err(err("CannotPutContainerInContainer", "Cargo Containers cannot be scooped"));
        }
        if it_cat == 2 && it_grp == 12 && snap.items.values().any(|child| child.location_id == item_id && child.quantity > 0) {
            return Err(err("CannotPutContainerInContainer", "Cannot scoop non-empty container"));
        }
        if it.location_id != sys_id as i64 {
            return Err(err("ObjectNotInSpace", format!("Item {item_id} is no longer in space")));
        }
        let Some(host) = snap.items.get(&host_ship_id) else {
            return Err(err("ShipNotFound", format!("Host ship {host_ship_id} not found")));
        };
        let host_type = ctx.sde.types.get(&host.type_id);
        let it_type = ctx.sde.types.get(&it.type_id);
        let it_cat = it_type.map(|t| t.category_id).unwrap_or(0);
        let it_grp = it_type.map(|t| t.group_id).unwrap_or(0);
        let it_vol = effective_item_unit_volume(&ctx.sde, it) * (it.quantity as f64);
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        if let Some(meta) = snap.sp6_lifecycle.space_object_meta.get(&item_id) {
            if meta.anchor_start_unix.is_some() || meta.anchor_completed {
                return Err(err("CannotScoopAnchored", "Cannot scoop an anchored or anchoring object"));
            }
        }
        if it_cat == 65 {
            return Err(err("CannotScoopStructure", "Structures cannot be scooped into ship holds"));
        }
        let check_corp_role = || -> Result<(), BridgeRpcError> {
            let has_role = (session.corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER)) != 0
                || snap.corporations.get(&caller_corp).map(|c| c.ceo_id == char_id).unwrap_or(false)
                || snap.corporations.get(&caller_corp).and_then(|c| c.member_roles.get(&char_id)).map(|&r| (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER)) != 0).unwrap_or(false);
            if !has_role {
                return Err(err("CorpRoleDenied", "Scooping corporation-owned items requires Director or Station Manager role"));
            }
            Ok(())
        };
        let mut to_transfer = false;
        match method {
            "ScoopToSMA" => {
                let Some(t) = it_type else {
                    return Err(err("ItemNotFound", format!("Type {} not found", it.type_id)));
                };
                if t.category_id != 6 {
                    return Err(err("CannotStoreCargoInSMA", "Only ships can be stored in a Ship Maintenance Bay"));
                }
                if !it.singleton {
                    return Err(err("OnlyAssembledShipsInSMA", "Only assembled ships can be stored in a Ship Maintenance Bay"));
                }
                let is_piloted = snap.characters.values().any(|c| c.active_ship_id == item_id && c.station_id.is_none());
                if is_piloted {
                    return Err(err("ShipPiloted", "Ship is currently piloted"));
                }
                let is_corp = caller_corp > 0 && it.owner_id == caller_corp;
                let owns = it.owner_id == char_id || is_corp;
                if !owns {
                    return Err(err("OwnershipViolation", format!("Ship {item_id} is owned by another character")));
                }
                if is_corp {
                    check_corp_role()?;
                }
                let has_non_charge_cargo = snap.items.values().any(|child| {
                    if child.location_id != item_id || child.quantity <= 0 {
                        return false;
                    }
                    let cf = child.flag as u16;
                    if matches!(cf, 11..=34 | 87 | 92..=94 | 125..=132 | 158) {
                        return false;
                    }
                    let c_cat = ctx.sde.types.get(&child.type_id).map(|t| t.category_id).unwrap_or(0);
                    c_cat != 8
                });
                if has_non_charge_cargo {
                    return Err(err(
                        "ShipHasNonChargeCargo",
                        "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo",
                    ));
                }
                let sma_cap = host_type.and_then(|h| h.default_attributes.get(&908)).copied().unwrap_or(0.0);
                if sma_cap <= 0.0 {
                    return Err(err("NoShipMaintenanceBay", "Active ship has no Ship Maintenance Bay"));
                }
                let used_sma: f64 = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == host_ship_id && matches!(i.flag as u16, 90 | 138) && i.item_id != item_id)
                    .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                    .sum();
                if used_sma + it_vol > sma_cap + 1e-4 {
                    return Err(err("CapacityExceeded", format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded")));
                }
            }
            "ScoopToFleetHangar" => {
                if it_cat == 6 {
                    return Err(err("CannotStoreShipInFleetHangar", "Ships cannot be stored in Fleet Hangar"));
                }
                if it_grp == 186 {
                    return Err(err("CannotScoopWreck", "Wrecks cannot be scooped"));
                }
                let is_corp = caller_corp > 0 && it.owner_id == caller_corp;
                let owns = it.owner_id == char_id || is_corp || it.owner_id == 0;
                if !owns {
                    return Err(err("OwnershipViolation", "Item is owned by another character"));
                }
                if is_corp {
                    check_corp_role()?;
                }
                if it.owner_id == 0 {
                    to_transfer = true;
                }
                let fh_cap = host_type.and_then(|h| h.default_attributes.get(&912)).copied().unwrap_or(0.0);
                if fh_cap <= 0.0 {
                    return Err(err("NoFleetHangar", "Active ship has no Fleet Hangar"));
                }
                let used_fh: f64 = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == host_ship_id && (i.flag as u16) == 155 && i.item_id != item_id)
                    .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                    .sum();
                if used_fh + it_vol > fh_cap + 1e-4 {
                    return Err(err("CapacityExceeded", format!("Fleet Hangar capacity {fh_cap} m³ exceeded")));
                }
            }
            "ScoopToInfrastructureHold" => {
                if !is_infrastructure_item(&ctx.sde, it.type_id) {
                    return Err(err("UserError", "Item is not an infrastructure or deployable item"));
                }
                let is_corp = caller_corp > 0 && it.owner_id == caller_corp;
                let owns = it.owner_id == char_id || is_corp || it.owner_id == 0;
                if !owns {
                    return Err(err("OwnershipViolation", "Item is owned by another character"));
                }
                if is_corp {
                    check_corp_role()?;
                }
                if it.owner_id == 0 {
                    to_transfer = true;
                }
                let infra_cap = host_type.and_then(|h| h.default_attributes.get(&5646)).copied().unwrap_or(0.0);
                if infra_cap <= 0.0 {
                    return Err(err("NoInfrastructureHold", "Active ship has no Infrastructure Hold"));
                }
                let used_infra: f64 = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == host_ship_id && (i.flag as u16) == 185 && i.item_id != item_id)
                    .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                    .sum();
                if used_infra + it_vol > infra_cap + 1e-4 {
                    return Err(err("CapacityExceeded", format!("Infrastructure Hold capacity {infra_cap} m³ exceeded")));
                }
            }
            "ScoopToMobileDepotHold" => {
                if !is_mobile_depot(&ctx.sde, it.type_id) {
                    return Err(err("UserError", "Item is not a Mobile Depot"));
                }
                let is_corp = caller_corp > 0 && it.owner_id == caller_corp;
                let owns = it.owner_id == char_id || is_corp || it.owner_id == 0;
                if !owns {
                    return Err(err("OwnershipViolation", "Mobile Depot is owned by another character"));
                }
                if is_corp {
                    check_corp_role()?;
                }
                if it.owner_id == 0 {
                    to_transfer = true;
                }
                let depot_cap = host_type.and_then(|h| h.default_attributes.get(&5325)).copied().unwrap_or(0.0);
                if depot_cap <= 0.0 {
                    return Err(err("NoMobileDepotHold", "Active ship has no Mobile Depot Hold"));
                }
                let used_depot: f64 = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == host_ship_id && (i.flag as u16) == 179 && i.item_id != item_id)
                    .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                    .sum();
                if used_depot + it_vol > depot_cap + 1e-4 {
                    return Err(err("CapacityExceeded", format!("Mobile Depot Hold capacity {depot_cap} m³ exceeded")));
                }
            }
            "Scoop" => {
                if it_cat == 6 {
                    return Err(err("CannotStoreShipInCargo", "Ships cannot be scooped into cargo hold"));
                }
                if it_grp == 186 {
                    return Err(err("CannotScoopWreck", "Wrecks cannot be scooped into cargo"));
                }
                let is_corp = caller_corp > 0 && it.owner_id == caller_corp;
                let owns = it.owner_id == char_id || is_corp;
                if !owns {
                    if it.owner_id == 0 {
                        to_transfer = true;
                    } else {
                        return Err(err("OwnershipViolation", "Cannot scoop an item belonging to another character unless abandoned"));
                    }
                }
                if is_corp {
                    check_corp_role()?;
                }
                let cargo_cap = host_type.map(|h| h.capacity).unwrap_or(0.0);
                if cargo_cap <= 0.0 {
                    return Err(err("NoCargoHold", "Ship has no cargo capacity"));
                }
                let used_cargo: f64 = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == host_ship_id && i.flag == InventoryFlag::Cargo && i.item_id != item_id)
                    .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                    .sum();
                if used_cargo + it_vol > cargo_cap + 1e-4 {
                    return Err(err("CapacityExceeded", format!("Cargo capacity {cargo_cap} m³ exceeded")));
                }
            }
            _ => {}
        }
        let it_mut = snap.items.get_mut(&item_id).unwrap();
        it_mut.location_id = host_ship_id;
        it_mut.flag = target_flag;
        it_mut.singleton = it_cat == 6;
        it_mut.version += 1;
        if to_transfer {
            it_mut.owner_id = char_id;
        }
        snap.item_positions.remove(&item_id);
        snap.sp6_lifecycle.space_object_meta.remove(&item_id);
        Ok((to_transfer, false))
    })?;
    let _ = sys.remove_ball(item_id).await;
    write_container_ship_state(ctx, |st| {
        st.custom_item_flags.insert(item_id, target_flag_u16);
    });
    Ok(PyRep::keyval([
        ("itemID", PyRep::Integer(item_id)),
        ("hostShipID", PyRep::Integer(host_ship_id)),
        ("flagID", PyRep::Integer(target_flag_u16 as i64)),
    ]))
}

pub use super::fitting_ship_forcefield::{can_enter_forcefield, check_pos_forcefield_coverage};
