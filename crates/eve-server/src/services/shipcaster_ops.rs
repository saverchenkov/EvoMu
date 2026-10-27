//! `shipcaster_ops` — Faction Warfare Interstellar Shipcasters and Rookie Starter Shipcasters.
//!
//! Implements:
//! - `shipCasterLandingPadMgr.GetAllLandingPads`: returns FW landing pad list with factionID, isLinked, solarSystemID.
//! - `shipCasterLandingPadMgr.GetFactionLandingPads`: returns landing pads for a specific faction with canBeLinked, isBuilt, isDisrupted, isLinked, solarSystemID.
//! - `shipCasterLauncherMgr.GetFactionsWithShipcaster`: returns list of faction IDs operating active shipcasters.
//! - `starterShipcasterTravelMgr.CanCharacterJump`: checks if character can jump (verifying crimewatch weapon timer).
//! - `starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster`: navigates character through beginner shipcaster to staging/starter system.
//! - `shipcasterTravelMgr.CmdJumpThroughShipcaster`: launches militia ship through interstellar shipcaster to linked destination landing pad.

use crate::router::ServerContext;
use crate::services::cluster_macho_misc_services::with_misc_state;
use crate::system_actor::SystemHandle;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub const SHIPCASTER_MAX_RANGE_M: f64 = 5_000.0;

fn rpc_err(code: impl Into<String>, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    }
}

/// Enforces fail-closed operational range and ball presence in the active solar system actor.
pub(crate) async fn check_proximity(
    sys: &SystemHandle,
    ship_id: i64,
    target_id: i64,
    max_dist: f64,
    label: &str,
) -> Result<(), BridgeRpcError> {
    if target_id <= 0 {
        return Err(rpc_err(
            "TargetNotFound",
            format!("Invalid target ID {target_id} for {label}"),
        ));
    }
    let ship_ball = sys
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", e))?
        .map(|(_, b)| b)
        .ok_or_else(|| rpc_err("ShipNotInSpace", "Active ship ball is not in space in this solar system"))?;

    let target_ball = sys
        .get_ball_snapshot(target_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", e))?
        .map(|(_, b)| b)
        .ok_or_else(|| {
            rpc_err(
                "TargetNotInSpace",
                format!("{label} entity {target_id} is not present in space in this solar system"),
            )
        })?;

    let dist = ship_ball.position.distance(target_ball.position);
    if dist > max_dist {
        return Err(rpc_err(
            "OutsideOperationalRange",
            format!("Ship is {dist:.1}m from {label} (must be <= {max_dist:.0}m)"),
        ));
    }
    Ok(())
}

/// Executes jump handoff across system actors, moving the ball and emitting complete session changes.
pub(crate) async fn execute_shipcaster_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    origin_sys: u32,
    dest_sys: u32,
    arrival_pos: glam::DVec3,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let old_handle = ctx.systems.get_or_spawn(origin_sys);
    let Some((cur_tick, ship_ball)) = old_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", e))?
    else {
        return Err(rpc_err("ShipNotInSpace", "Active ship ball not in space in origin solar system"));
    };

    if matches!(ship_ball.type_id, 670 | 33328) {
        return Err(rpc_err("CapsuleCannotJumpShipcaster", "Capsules cannot jump through a shipcaster"));
    }

    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        if cw.get_or_create(char_id).is_weapon_timer_active(now_unix) {
            return Err(rpc_err(
                "WeaponsTimerActive",
                "Cannot jump through shipcaster while weapons timer is active",
            ));
        }
    }

    if let Some(active_timer) =
        crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, Some((origin_sys, cur_tick)))
    {
        return Err(rpc_err(
            "SessionChangeInProgress",
            format!("Cannot jump shipcaster while 10s session-change timer ({}) is active", active_timer.reason),
        ));
    }

    if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump") {
        return Err(e);
    }

    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(rpc_err("ShipInWarp", "Cannot jump shipcaster while ship is mid-warp"));
    }

    let is_scrambled = old_handle.is_ball_scrambled(ship_id).await.unwrap_or(false);
    if is_scrambled {
        return Err(rpc_err("WarpScrambled", "Cannot jump shipcaster while warp scrambled or disrupted"));
    }

    if let Ok(Some(cs)) = old_handle.get_combat_state(ship_id).await {
        if cs.warp_scramble_strength > 0 {
            return Err(rpc_err("WarpScrambled", "Cannot jump shipcaster while warp scrambled"));
        }
    }

    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys, &ctx.sde)
        .map_err(|e| rpc_err("SystemTransitionFailed", e.to_string()))?;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = dest_sys as i64;
        }
    });

    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, origin_sys);
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(origin_sys), dest_sys);

    let new_handle = ctx.systems.get_or_spawn(dest_sys);
    let mut new_ball = existing_ball.unwrap_or(ship_ball);
    new_ball.position = arrival_pos;
    new_ball.velocity = glam::DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = eve_destiny::MovementMode::Stop;

    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;

    let new_tick = new_handle
        .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
        .await
        .unwrap_or(0);

    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys,
        new_tick,
        "shipcaster",
    );

    session.solar_system_id = Some(dest_sys);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;
    crate::services::map_service::record_solar_system_visit(ctx, char_id, dest_sys);
    let _ = ctx.bind_object(session, "beyonce", dest_sys as i64);

    let _ = new_handle.push_set_state(ship_id).await;

    Ok(())
}

/// Dispatches Shipcaster and Starter Shipcaster RPC calls.
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // --------------------------------------------------------------------
        // shipCasterLandingPadMgr: Faction Warfare Landing Pad Registry
        // --------------------------------------------------------------------
        ("shipCasterLandingPadMgr", "GetAllLandingPads") => {
            let pads = with_misc_state(ctx, |st| st.landing_pads.clone());
            Some(Ok(PyRep::List(pads.iter().map(|p| p.to_keyval()).collect())))
        }

        ("shipCasterLandingPadMgr", "GetFactionLandingPads") => {
            let faction_id = match args.first().and_then(|a| a.as_i64()) {
                Some(fid) => i32::try_from(fid).unwrap_or(0),
                None => return Some(Err(rpc_err("InvalidArguments", "GetFactionLandingPads requires factionID"))),
            };
            let pads = with_misc_state(ctx, |st| {
                st.landing_pads
                    .iter()
                    .filter(|p| p.faction_id == faction_id)
                    .map(|p| p.to_keyval())
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(pads)))
        }

        // --------------------------------------------------------------------
        // shipCasterLauncherMgr: Interstellar Shipcaster Launchers
        // --------------------------------------------------------------------
        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster") => {
            let factions = with_misc_state(ctx, |st| {
                let mut set = std::collections::BTreeSet::new();
                for l in &st.launchers {
                    set.insert(l.faction_id);
                }
                set.into_iter().map(|f| PyRep::Integer(f as i64)).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(factions)))
        }

        // --------------------------------------------------------------------
        // starterShipcasterTravelMgr: Rookie Starter Shipcaster Travel
        // --------------------------------------------------------------------
        ("starterShipcasterTravelMgr", "CanCharacterJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            let has_timer = {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                cw.get_or_create(char_id).is_weapon_timer_active(now)
            };
            Some(Ok(PyRep::Bool(!has_timer)))
        }

        ("starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(rpc_err("CantDoThatWhileDocked", "Cannot jump shipcaster while docked")));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughStarterShipcaster requires an active ship")));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughStarterShipcaster requires an active solar system")));
            };
            let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
            let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594) {
                return Some(Err(rpc_err("CapitalShipCannotJumpShipcaster", "Capital ships cannot jump through a shipcaster")));
            }
            let target_arg = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => return Some(Err(rpc_err("TargetNotFound", format!("Invalid caster ID {id}")))),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughStarterShipcaster requires caster itemID or destinationSystemID"))),
            };

            let (starter_launcher, dest_sys) = match with_misc_state(ctx, |st| {
                if let Some(l) = st.starter_launchers.iter().find(|l| l.item_id == target_arg).cloned() {
                    let dest = l.staging_system_id;
                    Some(Ok((l, dest)))
                } else if let Some(l) = st.starter_launchers.iter().find(|l| l.solar_system_id == sys_id).cloned() {
                    if (30_000_000..40_000_000).contains(&target_arg) && target_arg as u32 != l.staging_system_id {
                        return Some(Err(rpc_err(
                            "InvalidDestinationSystem",
                            "Destination system does not match starter shipcaster staging system",
                        )));
                    }
                    let dest = l.staging_system_id;
                    Some(Ok((l, dest)))
                } else {
                    None
                }
            }) {
                Some(Ok(pair)) => pair,
                Some(Err(e)) => return Some(Err(e)),
                None => return Some(Err(rpc_err("LauncherNotFound", format!("Starter shipcaster {target_arg} not registered")))),
            };

            if starter_launcher.solar_system_id != sys_id {
                return Some(Err(rpc_err(
                    "InvalidSolarSystem",
                    format!(
                        "Starter shipcaster {} is located in system {}, not current system {sys_id}",
                        starter_launcher.item_id, starter_launcher.solar_system_id
                    ),
                )));
            }

            let now = ctx.now_unix();
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                if cw.get_or_create(char_id).is_weapon_timer_active(now) {
                    return Some(Err(rpc_err("WeaponsTimerActive", "Cannot jump through starter shipcaster while weapons timer is active")));
                }
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = check_proximity(&sys, ship_id, starter_launcher.item_id, SHIPCASTER_MAX_RANGE_M, "starter shipcaster").await {
                return Some(Err(e));
            }

            let pad = with_misc_state(ctx, |st| {
                st.landing_pads.iter().find(|p| p.faction_id == starter_launcher.faction_id && p.solar_system_id == dest_sys).cloned()
            });
            let arrival_pos = pad.map(|p| p.position).unwrap_or(glam::DVec3::ZERO) + glam::DVec3::new(2_500.0, 0.0, 0.0);
            if let Err(e) = execute_shipcaster_jump(ctx, session, char_id, ship_id, sys_id, dest_sys, arrival_pos).await {
                return Some(Err(e));
            }

            Some(Ok(PyRep::None))
        }

        // --------------------------------------------------------------------
        // shipcasterTravelMgr: Interstellar Shipcaster Travel
        // --------------------------------------------------------------------
        ("shipcasterTravelMgr", "CmdJumpThroughShipcaster") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(rpc_err("CantDoThatWhileDocked", "Cannot jump shipcaster while docked")));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughShipcaster requires an active ship")));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughShipcaster requires an active solar system")));
            };

            let a0 = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => return Some(Err(rpc_err("TargetNotFound", format!("Invalid caster ID {id}")))),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughShipcaster requires caster itemID"))),
            };
            let a1 = match args.get(1).and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => return Some(Err(rpc_err("TargetNotFound", format!("Invalid destination system or pad ID {id}")))),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughShipcaster requires to_system_id or landingPadID"))),
            };
            let a2 = match args.get(2).and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => return Some(Err(rpc_err("TargetNotFound", format!("Invalid destination pad or origin beacon ID {id}")))),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughShipcaster requires to_pad_id or originBeaconID"))),
            };

            let (launchers, pads) = with_misc_state(ctx, |st| (st.launchers.clone(), st.landing_pads.clone()));

            // Resolve origin launcher (a0, a2, or a1):
            let launcher = if let Some(l) = launchers.iter().find(|l| l.item_id == a0) {
                l.clone()
            } else if let Some(l) = launchers.iter().find(|l| l.item_id == a2) {
                l.clone()
            } else if let Some(l) = launchers.iter().find(|l| l.item_id == a1) {
                l.clone()
            } else {
                return Some(Err(rpc_err("LauncherNotFound", format!("Shipcaster launcher {a0} not registered"))));
            };

            // Resolve destination landing pad:
            let (to_system_id, pad) = if let Some(p) = pads.iter().find(|p| p.item_id == a2) {
                let sys = if (30_000_000..40_000_000).contains(&a1) { a1 as u32 } else { p.solar_system_id };
                (sys, p.clone())
            } else if let Some(p) = pads.iter().find(|p| p.item_id == a1) {
                let sys = if (30_000_000..40_000_000).contains(&a0) { a0 as u32 } else { p.solar_system_id };
                (sys, p.clone())
            } else if let Some(p) = pads.iter().find(|p| p.item_id == a0) {
                let sys = if (30_000_000..40_000_000).contains(&a1) { a1 as u32 } else { p.solar_system_id };
                (sys, p.clone())
            } else {
                let pad_id_candidate = if a0 == launcher.item_id { a2 } else { a1 };
                return Some(Err(rpc_err("LandingPadNotFound", format!("Landing pad {pad_id_candidate} not registered"))));
            };

            let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
            let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594) {
                return Some(Err(rpc_err("CapitalShipCannotJumpShipcaster", "Capital ships cannot jump through an interstellar shipcaster")));
            }
            let char_war_faction = ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));
            if launcher.faction_id > 0 && char_war_faction != Some(launcher.faction_id as i64) {
                return Some(Err(rpc_err(
                    "FactionShipcasterAccessDenied",
                    "Character militia faction does not match shipcaster faction",
                )));
            }
            if launcher.solar_system_id != sys_id {
                return Some(Err(rpc_err(
                    "InvalidSolarSystem",
                    format!("Shipcaster launcher {} is located in system {}, not current system {sys_id}", launcher.item_id, launcher.solar_system_id),
                )));
            }

            if pad.solar_system_id != to_system_id {
                return Some(Err(rpc_err(
                    "InvalidDestinationSystem",
                    format!("Landing pad {} is located in system {}, not specified destination {to_system_id}", pad.item_id, pad.solar_system_id),
                )));
            }
            if !launcher.linked_pad_ids.contains(&pad.item_id) || pad.link_timestamp <= 0 {
                return Some(Err(rpc_err(
                    "LandingPadNotLinked",
                    format!("Landing pad {} is not currently linked to launcher {}", pad.item_id, launcher.item_id),
                )));
            }
            if !pad.is_built || pad.is_disrupted {
                return Some(Err(rpc_err(
                    "LandingPadUnavailable",
                    format!("Landing pad {} is not built or is disrupted", pad.item_id),
                )));
            }

            let now = ctx.now_unix();
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                if cw.get_or_create(char_id).is_weapon_timer_active(now) {
                    return Some(Err(rpc_err("WeaponsTimerActive", "Cannot jump through shipcaster while weapons timer is active")));
                }
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = check_proximity(&sys, ship_id, launcher.item_id, SHIPCASTER_MAX_RANGE_M, "shipcaster launcher").await {
                return Some(Err(e));
            }

            let arrival_pos = pad.position + glam::DVec3::new(2_500.0, 0.0, 0.0);
            if let Err(e) = execute_shipcaster_jump(ctx, session, char_id, ship_id, sys_id, to_system_id, arrival_pos).await {
                return Some(Err(e));
            }

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
