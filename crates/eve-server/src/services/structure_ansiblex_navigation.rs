//! `structure_ansiblex_navigation` — Ansiblex Jump Bridge navigation and gate jumping (`SO-4`).
#![forbid(unsafe_code)]

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use crate::router::ServerContext;
use super::structure_upwell_ops::load_ext_state;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub const ANSIBLEX_TYPE_ID: u32 = 35841;
pub const LIQUID_OZONE_TYPE_ID: u32 = 16273;
pub const ANSIBLEX_MAX_MASS_KG: f64 = 1_480_000_000.0;
pub const ANSIBLEX_MAX_RANGE_LY: f64 = 5.0;

pub(crate) fn is_sovereign_nullsec_for_ansiblex(sde: &eve_sde::SdeRegistry, sys_id: u32) -> bool {
    sde.solar_systems.get(&sys_id).is_some_and(|s| {
        s.security_status <= 0.0
            && sys_id >= 30_000_000
            && sys_id < 31_000_000
            && sys_id != 30_100_000
            && sde.regions.get(&s.region_id).is_some_and(|r| r.faction_id.is_none())
            && sde.constellations.get(&s.constellation_id).is_some_and(|c| c.faction_id.is_none())
    })
}

pub(crate) fn resolve_linked_structure_id(
    snap: &eve_db::WorldStateSnapshot,
    ext: &super::structure_upwell_ops::StructureExtendedState,
    structure_id: i64,
) -> Option<i64> {
    if let Some(&linked) = ext.jump_bridge_links.get(&structure_id) {
        if snap.structures.get(&linked).is_some_and(|s| s.state != "Destroyed") {
            return Some(linked);
        }
    }
    None
}

fn require_jump_bridge_recon_access(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    if !snap.structures.contains_key(&structure_id) {
        return Ok(());
    }
    let now = ctx.now_unix();
    let allowed = crate::services::structure_profile_services::can_character_use_jump_bridge(
        snap,
        structure_id,
        char_id,
        now,
    ) || super::structure_upwell_ops::evaluate_character_structure_access_in_snap_at(
        snap,
        structure_id,
        char_id,
        Some(now),
    );
    if !allowed {
        return Err(err(
            "AccessDenied",
            format!("Character {char_id} is denied jump bridge access to structure {structure_id}"),
        ));
    }
    Ok(())
}

pub(crate) fn handle_get_linked_structure(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "GetLinkedStructure requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        require_jump_bridge_recon_access(ctx, snap, structure_id, char_id)?;
        let Some(linked) = resolve_linked_structure_id(snap, &load_ext_state(snap), structure_id) else {
            return Ok(PyRep::None);
        };
        let Some(dest_sys) = snap.structures.get(&linked).map(|s| s.solar_system_id) else {
            return Ok(PyRep::None);
        };
        Ok(PyRep::Tuple(vec![PyRep::Integer(linked), PyRep::Integer(i64::from(dest_sys))]))
    })
}

pub fn handle_get_jb_structure_destination(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "GetJbStructureDestination requires structureID"))?;
    ctx.db.with_snapshot(|snap| {
        require_jump_bridge_recon_access(ctx, snap, structure_id, char_id)?;
        let linked_id = resolve_linked_structure_id(snap, &load_ext_state(snap), structure_id)
            .ok_or_else(|| err("NoLinkedStructure", format!("Structure {structure_id} is not linked to any destination gate")))?;
        let dest_sys = snap
            .structures
            .get(&linked_id)
            .map(|s| s.solar_system_id)
            .ok_or_else(|| err("LinkedStructureNotFound", format!("Destination structure {linked_id} not found")))?;
        Ok(PyRep::Integer(i64::from(dest_sys)))
    })
}

pub(crate) fn handle_get_jump_bridges_with_my_access(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let mut pairs = Vec::new();
        let mut accessible_ids = Vec::new();
        let mut structs: Vec<&eve_db::UpwellStructureRecord> = snap
            .structures
            .values()
            .filter(|st| {
                st.type_id == ANSIBLEX_TYPE_ID
                    && st.state != "Destroyed"
                    && crate::services::structure_profile_services::can_character_use_jump_bridge(
                        snap,
                        st.structure_id,
                        char_id,
                        now,
                    )
            })
            .collect();
        structs.sort_by_key(|st| st.structure_id);
        for st in structs {
            accessible_ids.push(PyRep::Integer(st.structure_id));
            if let Some(linked_id) = resolve_linked_structure_id(snap, &ext, st.structure_id) {
                if let Some(dest_st) = snap.structures.get(&linked_id) {
                    let from_kv = PyRep::keyval([
                        ("structureID", PyRep::Integer(st.structure_id)),
                        ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                        ("typeID", PyRep::Integer(i64::from(st.type_id))),
                    ]);
                    let to_kv = PyRep::keyval([
                        ("structureID", PyRep::Integer(linked_id)),
                        ("solarSystemID", PyRep::Integer(i64::from(dest_st.solar_system_id))),
                        ("typeID", PyRep::Integer(i64::from(dest_st.type_id))),
                    ]);
                    pairs.push(PyRep::Tuple(vec![from_kv, to_kv]));
                }
            }
        }
        // V24 `MapSvc.GetJumpBridgesWithMyAccess` (`mapSvc.pyj:609`, `client_expectations.json:101570-101657`)
        // unpacks a 3-tuple: `(jumpBridgePairs, accessibleStructureIDs, auxiliaryMap)`.
        Ok(PyRep::Tuple(vec![
            PyRep::List(pairs),
            PyRep::List(accessible_ids),
            PyRep::Dict(Vec::new()),
        ]))
    })
}

pub(crate) fn handle_get_nearby_jump_bridges(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let mut results = Vec::new();
        let mut structs: Vec<&eve_db::UpwellStructureRecord> = snap
            .structures
            .values()
            .filter(|st| {
                st.type_id == ANSIBLEX_TYPE_ID
                    && st.state != "Destroyed"
                    && crate::services::structure_profile_services::can_character_use_jump_bridge(
                        snap,
                        st.structure_id,
                        char_id,
                        now,
                    )
            })
            .collect();
        structs.sort_by_key(|st| st.structure_id);
        for st in structs {
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
        Ok(PyRep::List(results))
    })
}

pub(crate) async fn handle_cmd_jump_through_structure_stargate(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("ShipDocked", "Cannot jump through structure stargate while docked"));
    }
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "CmdJumpThroughStructureStargate requires structureID"))?;
    let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no solarSystemID"))?;
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Session has no active ship"))?;
    ctx.validate_ship_movement(
        char_id,
        ship_id,
        crate::services::space_nav_movement_guard::MovementIntent::AnsiblexJump {
            from_sys: sys_id,
            to_sys: sys_id,
        },
    )
    .await?;
    let now_unix = ctx.now_unix();
    if ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|st| st.is_weapon_timer_active(now_unix)) {
        return Err(err("WeaponsTimerActive", "Cannot jump through structure stargate while weapons timer is active"));
    }
    let (st_pos, st_radius, dest_sys_id, dest_st_pos) = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if st.solar_system_id != sys_id
            || matches!(st.state.as_str(), "Destroyed" | "Anchoring" | "AnchorVulnerable" | "NoCore" | "Onlining" | "Abandoned")
            || !st.quantum_core_installed
            || st.fuel_blocks <= 0
        {
            return Err(err("StructureNotOperational", "Ansiblex stargate is offline, unpowered, or not in this solar system"));
        }
        if !crate::services::structure_profile_services::can_character_use_jump_bridge(snap, st.structure_id, char_id, now_unix) {
            return Err(err("AccessDenied", "Structure access list denies jump bridge usage"));
        }
        let ext = load_ext_state(snap);
        let linked = resolve_linked_structure_id(snap, &ext, structure_id).ok_or_else(|| err("NoLinkedStructure", "Ansiblex has no linked destination stargate"))?;
        if st.type_id != ANSIBLEX_TYPE_ID {
            return Err(err("StructureNotOperational", "Structure is not an Ansiblex Jump Gate"));
        }
        let dest_st = snap.structures.get(&linked).cloned().ok_or_else(|| err("DestinationStructureNotFound", "Linked destination structure not found"))?;
        if dest_st.state == "Destroyed" {
            return Err(err("DestinationStructureDestroyed", "Linked destination stargate is destroyed"));
        }
        if dest_st.type_id != ANSIBLEX_TYPE_ID
            || matches!(dest_st.state.as_str(), "Anchoring" | "AnchorVulnerable" | "NoCore" | "Onlining" | "Abandoned")
            || !dest_st.quantum_core_installed
            || dest_st.fuel_blocks <= 0
        {
            return Err(err("StructureNotOperational", "Linked destination Ansiblex stargate is offline or unpowered"));
        }
        if !is_sovereign_nullsec_for_ansiblex(&ctx.sde, sys_id)
            || !is_sovereign_nullsec_for_ansiblex(&ctx.sde, dest_st.solar_system_id)
        {
            return Err(err("JumpBridgeRequiresSovNull", "Ansiblex Jump Gates only operate between sovereign null-security systems"));
        }
        let raw_dist_ly = crate::services::space_destiny::space_nav_travel::distance_ly(ctx, sys_id, dest_st.solar_system_id);
        if raw_dist_ly > ANSIBLEX_MAX_RANGE_LY {
            return Err(err("JumpBridgeOutOfRange", format!("Ansiblex link distance ({raw_dist_ly:.2} LY) exceeds 5.0 LY maximum")));
        }
        if let Some(ship_item) = snap.items.get(&ship_id) {
            let ship_mass = ctx.sde.types.get(&ship_item.type_id).map(|t| t.mass).unwrap_or(0.0);
            let group_id = ctx.sde.types.get(&ship_item.type_id).map(|t| t.group_id).unwrap_or(0);
            if ship_mass > ANSIBLEX_MAX_MASS_KG || matches!(group_id, 30 | 659) {
                return Err(err("ShipNotAllowedThroughAnsiblex", "Ship exceeds Ansiblex mass limit of 1.48 Mt"));
            }
        }
        let rad = ctx.sde.types.get(&st.type_id).map(|t| t.radius).unwrap_or(500.0);
        Ok((glam::DVec3::from_array(st.position), rad, dest_st.solar_system_id, glam::DVec3::from_array(dest_st.position)))
    })?;
    let old_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((cur_tick, ship_ball)) = old_handle.get_ball_snapshot(ship_id).await.map_err(|e| err("DestinyQueryFailed", e))? else {
        return Err(err("ShipNotInSpace", "Ship is not in space"));
    };
    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(err("ShipInWarp", "Cannot jump through structure stargate while in warp"));
    }
    if old_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(err("WarpScrambled", "Cannot jump through structure stargate while warp scrambled"));
    }
    if ship_ball.mass > ANSIBLEX_MAX_MASS_KG {
        return Err(err("ShipNotAllowedThroughAnsiblex", "Ship exceeds Ansiblex mass limit of 1.48 Mt"));
    }
    if ship_ball.position.distance(st_pos) > st_radius + 5_000.0 {
        return Err(err("OutsideJumpBridgeRange", format!("Ship is {:.1} m from gate (must be within {:.1} m)", ship_ball.position.distance(st_pos), st_radius + 5000.0)));
    }
    if let Some(active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, Some((sys_id, cur_tick))) {
        return Err(err("SessionChangeInProgress", format!("Cannot jump through structure stargate while 10s timer from '{}' is active", active.reason)));
    }
    let _dest_sys_info = ctx.sde.solar_systems.get(&dest_sys_id).ok_or_else(|| err("InvalidDestination", format!("Destination system {dest_sys_id} not in SDE")))?;
    let dist_ly = crate::services::space_destiny::space_nav_travel::distance_ly(ctx, sys_id, dest_sys_id).max(0.01);
    let ozone_cost = ((ship_ball.mass * dist_ly * 0.000003 + 50.0).round() as i64).max(50);
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut stack_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == structure_id
                    && matches!(i.flag, InventoryFlag::StructureFuel | InventoryFlag::SpecializedFuelBay)
                    && i.type_id == LIQUID_OZONE_TYPE_ID
                    && i.quantity > 0
            })
            .map(|i| i.item_id)
            .collect();
        let total_ozone: i64 = stack_ids
            .iter()
            .filter_map(|sid| snap.items.get(sid))
            .map(|i| i64::from(i.quantity))
            .sum();
        if total_ozone < ozone_cost {
            return Err(err("InsufficientLiquidOzone", format!("Ansiblex gate requires at least {ozone_cost} units of Liquid Ozone in StructureFuel bay")));
        }
        if !snap.characters.contains_key(&char_id) {
            return Err(err("SystemTransitionFailed", format!("Character {char_id} not found")));
        }
        stack_ids.sort_unstable();
        let mut remaining = ozone_cost;
        for sid in stack_ids {
            if remaining <= 0 {
                break;
            }
            let deplete = if let Some(item) = snap.items.get_mut(&sid) {
                let avail = i64::from(item.quantity);
                let take = avail.min(remaining);
                item.quantity = (avail - take) as i32;
                item.version += 1;
                remaining -= take;
                item.quantity == 0
            } else {
                false
            };
            if deplete {
                snap.items.remove(&sid);
            }
        }
        if remaining > 0 {
            return Err(err("InsufficientLiquidOzone", format!("Ansiblex gate requires at least {ozone_cost} units of Liquid Ozone in StructureFuel bay")));
        }
        Ok(())
    })?;
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let prev_combat_state = old_handle.get_combat_state(ship_id).await.ok().flatten();
    let existing_ball = old_handle.remove_ball(ship_id).await;
    let updated_ch = ctx.db.with_mut_snapshot(|snap| {
        crate::router::apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            char_id,
            crate::router::TransitionLocation::Space { solar_system_id: dest_sys_id },
            Some(ship_id),
            true,
            false,
        )
        .expect("character must exist for ansiblex jump")
    });
    ctx.sync_character_local_chat(char_id, Some(sys_id), dest_sys_id);
    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(ship_ball);
    new_ball.position = dest_st_pos + glam::DVec3::new(1_500.0, 0.0, 0.0);
    new_ball.velocity = glam::DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = eve_destiny::MovementMode::Stop;
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(&mut combat_state, prev_combat_state);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;
    let new_tick = new_handle.spawn_ball_with_meta(new_ball, combat_state, slim_meta).await.unwrap_or(0);
    let _ = new_handle.push_set_state(ship_id).await;
    crate::services::space_destiny::record_session_change_timer(ctx, char_id, now_unix, dest_sys_id, new_tick, "jump_bridge");
    let _ = updated_ch;
    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::TransitionKind::JumpSystem { solar_system_id: dest_sys_id },
            "jump_bridge",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );
    crate::services::map_service::record_solar_system_jump(ctx, char_id, dest_sys_id);
    let new_beyonce = ctx.bind_object(session, "beyonce", dest_sys_id as i64)?;
    Ok(PyRep::keyval([("solarSystemID", PyRep::Integer(dest_sys_id as i64)), ("beyonceBoundID", PyRep::Integer(new_beyonce as i64))]))
}

#[cfg(test)] #[path = "structure_ansiblex_navigation_tests.rs"]
mod structure_ansiblex_navigation_tests;

