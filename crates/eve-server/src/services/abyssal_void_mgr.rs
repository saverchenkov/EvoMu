//! `abyssal_void_mgr` — Abyssal Deadspace & Void Space manager (`abyssalMgr`, `voidSpaceMgr`).
//!
//! Handles Abyssal Deadspace entrance gates, pocket room acceleration gates,
//! end gates, client ready acknowledgments, and Void Space jumps/gates.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;

pub const RPC_ABYSSAL_ENTRANCE_DEPLOYMENT: &str = "AbyssalEntranceDeployment";
pub const RPC_ABYSSAL_ENTRANCE_GATE_ACTIVATION: &str = "AbyssalEntranceGateActivation";
pub const RPC_ABYSSAL_GATE_ACTIVATION: &str = "AbyssalGateActivation";
pub const RPC_ABYSSAL_END_GATE_ACTIVATION: &str = "AbyssalEndGateActivation";
pub const RPC_ABYSSAL_CLIENT_IS_READY: &str = "ClientIsReady";

pub const RPC_VOID_SPACE_CLIENT_IS_READY: &str = "ClientIsReady";
pub const RPC_VOID_SPACE_PLAYER_JUMP: &str = "VoidSpacePlayerJump";
pub const RPC_VOID_SPACE_END_GATE_ACTIVATION: &str = "VoidSpaceEndGateActivation";

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("abyssalMgr", "AbyssalEntranceDeployment") => {
            if args.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "AbyssalEntranceDeployment requires filament item ID".into(),
                }));
            }
            Some(crate::services::pve_special_sites::handle_abyssal_entrance_deployment(ctx, session, args).await)
        }

        ("abyssalMgr", "AbyssalEntranceGateActivation") => {
            if args.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "AbyssalEntranceGateActivation requires gateID".into(),
                }));
            }
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let trace_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let trace_opt = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                reg.abyssal_traces.remove(&trace_id)
            });
            if let Some(trace) = trace_opt {
                return Some(activate_trace(ctx, session, char_id, trace).await);
            }
            Some(activate_inventory_filament(ctx, session, char_id, trace_id).await)
        }

        ("abyssalMgr", "AbyssalGateActivation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cur_sys = session.solar_system_id.unwrap_or(0);
            if !(32_000_000..=32_999_999).contains(&cur_sys) {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "AbyssalGateActivation only valid inside Abyssal Deadspace".into(),
                }));
            }
            Some(handle_abyssal_gate_activation(ctx, session, char_id, args, kwargs).await)
        }

        ("abyssalMgr", "AbyssalEndGateActivation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cur_sys = session.solar_system_id.unwrap_or(0);
            if !(32_000_000..=32_999_999).contains(&cur_sys) {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "AbyssalEndGateActivation only valid inside Abyssal Deadspace".into(),
                }));
            }
            Some(handle_abyssal_end_gate_activation(ctx, session, char_id, args).await)
        }

        ("abyssalMgr", "ClientIsReady") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cur_sys = session.solar_system_id.unwrap_or(0);
            if !(32_000_000..=32_999_999).contains(&cur_sys) {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "ClientIsReady for abyssalMgr only valid inside Abyssal Deadspace".into(),
                }));
            }
            crate::services::pve_special_sites::ensure_abyssal_room_balls_for_ctx(ctx, char_id).await;
            Some(Ok(PyRep::keyval([("ready", PyRep::Bool(true))])))
        }

        ("voidSpaceMgr", "ClientIsReady") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([("ready", PyRep::Bool(true))])))
        }

        ("voidSpaceMgr", "VoidSpacePlayerJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "VoidSpacePlayerJump requires arguments".into(),
                }));
            }
            Some(handle_void_space_player_jump(ctx, session, char_id, args).await)
        }

        ("voidSpaceMgr", "VoidSpaceEndGateActivation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cur_sys = session.solar_system_id.unwrap_or(0);
            if !(32_000_000..=32_999_999).contains(&cur_sys) {
                return Some(Err(BridgeRpcError::user_error("NotInVoidSpace")));
            }
            Some(handle_void_space_end_gate_activation(ctx, session, char_id, args).await)
        }

        _ => None,
    }
}

async fn activate_trace(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    trace: crate::services::pve_special_sites::AbyssalTraceRecord,
) -> Result<PyRep, BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate Abyssal entrance gate while docked".into(),
        });
    }
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;
    let origin_sys = session.solar_system_id.unwrap_or(trace.origin_solar_system_id);
    let ship_id = session.ship_id.unwrap_or(100_000_002);
    let (tier, _) = crate::services::pve_special_sites::classify_abyssal_filament(trace.filament_type_id)
        .unwrap_or((1, crate::services::pve_special_sites::AbyssalWeatherKind::Electrical));
    let now_u = ctx.now_unix();
    let abyssal_sys: SolarSystemId = 32_000_001;
    crate::sp6_lifecycle::register_abyssal_session(ctx, char_id, ship_id, abyssal_sys, origin_sys, now_u, now_u + 1_200);
    let (pocket_id, room1_npcs) = crate::services::pve_special_sites::enter_abyssal_pocket_for_character(
        ctx, session, char_id, trace.filament_type_id, tier, origin_sys,
    )
    .await;

    ctx.systems.get_or_spawn(origin_sys).try_remove_ball(ship_id);
    let abyss_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: glam::DVec3::new(0.0, 0.0, -10_000.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = abyss_handle.spawn_ball_with_combat(ball, cs).await;
    Ok(PyRep::keyval([
        ("pocketID", PyRep::Integer(pocket_id)),
        ("solarSystemID", PyRep::Integer(abyssal_sys as i64)),
        ("originSolarSystemID", PyRep::Integer(origin_sys as i64)),
        ("filamentTypeID", PyRep::Integer(trace.filament_type_id as i64)),
        ("tier", PyRep::Integer(tier as i64)),
        ("suspectFlagOnExit", PyRep::Bool(tier >= 4)),
        ("currentRoom", PyRep::Integer(1)),
        ("timerSeconds", PyRep::Integer(1_200)),
        ("npcBallIDs", PyRep::List(room1_npcs.iter().map(|&id| PyRep::Integer(id)).collect())),
    ]))
}

async fn activate_inventory_filament(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    filament_arg: i64,
) -> Result<PyRep, BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate Abyssal filament while docked".into(),
        });
    }
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;
    let origin_sys = session.solar_system_id.unwrap_or(30_000_142);
    let ship_id = session.ship_id.unwrap_or(100_000_002);
    let now_unix = ctx.now_unix();

    let resolved = ctx.db.try_with_mut_snapshot(|snap| {
        let found = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && (i.item_id == filament_arg || i.type_id as i64 == filament_arg)
                    && i.quantity >= 1
            })
            .map(|i| (i.item_id, i.type_id));
        let Some((iid, tid)) = found else {
            return Err(BridgeRpcError {
                code: "GateNotFound".into(),
                message: format!("Abyssal entrance gate or filament {filament_arg} not found"),
            });
        };
        let Some((tier, _)) = crate::services::pve_special_sites::classify_abyssal_filament(tid) else {
            return Err(BridgeRpcError {
                code: "InvalidAbyssalFilament".into(),
                message: format!("Type {tid} is not a valid Abyssal filament"),
            });
        };
        let origin_sec = ctx
            .sde
            .solar_systems
            .get(&origin_sys)
            .map(|s| s.security_status)
            .unwrap_or(0.9);
        if tier >= 4 && origin_sec >= 0.75 {
            return Err(BridgeRpcError {
                code: "FilamentDisallowedInHighSec".into(),
                message: "Activating high-tier (T4-T6) Abyssal filaments is blocked in 0.8 - 1.0 high-security systems".into(),
            });
        }
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > 1 {
                item.quantity -= 1;
                item.version += 1;
            } else {
                snap.items.remove(&iid);
            }
        }
        Ok((tid, tier, origin_sec))
    });

    let (filament_type_id, tier, origin_sec) = resolved?;
    if tier >= 4 && (0.45..0.75).contains(&origin_sec) {
        ctx.crimewatch
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .trigger_suspect_flag(char_id, now_unix);
    }
    let abyssal_sys: SolarSystemId = 32_000_001;
    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        abyssal_sys,
        origin_sys,
        now_unix,
        now_unix.saturating_add(1_200),
    );
    let (pocket_id, room1_npcs) = crate::services::pve_special_sites::enter_abyssal_pocket_for_character(
        ctx,
        session,
        char_id,
        filament_type_id,
        tier,
        origin_sys,
    )
    .await;

    ctx.systems.get_or_spawn(origin_sys).try_remove_ball(ship_id);
    let abyss_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: glam::DVec3::new(0.0, 0.0, -10_000.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = abyss_handle.spawn_ball_with_combat(ball, cs).await;
    Ok(PyRep::keyval([
        ("pocketID", PyRep::Integer(pocket_id)),
        ("solarSystemID", PyRep::Integer(abyssal_sys as i64)),
        ("originSolarSystemID", PyRep::Integer(origin_sys as i64)),
        ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
        ("tier", PyRep::Integer(tier as i64)),
        ("suspectFlagOnExit", PyRep::Bool(tier >= 4)),
        ("currentRoom", PyRep::Integer(1)),
        ("timerSeconds", PyRep::Integer(1_200)),
        ("npcBallIDs", PyRep::List(room1_npcs.iter().map(|&id| PyRep::Integer(id)).collect())),
    ]))
}

async fn handle_abyssal_gate_activation(
    ctx: &ServerContext,
    _session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let outcome = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get_mut(&char_id)?;
        if p.completed || p.collapsed_destroyed {
            return None;
        }
        let now = ctx.now_unix();
        if now >= p.collapse_at_sec {
            p.collapsed_destroyed = true;
            return Some(Err(BridgeRpcError {
                code: "PocketCollapsed".into(),
                message: "Abyssal Deadspace pocket has collapsed".into(),
            }));
        }
        if p.current_room == 1 {
            p.current_room = 2;
            p.rooms[0].cleared = true;
            let pid = p.pocket_id;
            let abyss_sys = p.abyssal_solar_system_id;
            let r2_npcs = p.rooms[1].npc_ball_ids.clone();
            let ship_id = p.ship_id;
            Some(Ok((2, pid, abyss_sys, r2_npcs, ship_id)))
        } else if p.current_room == 2 {
            p.current_room = 3;
            p.rooms[1].cleared = true;
            let pid = p.pocket_id;
            let abyss_sys = p.abyssal_solar_system_id;
            let r3_npcs = p.rooms[2].npc_ball_ids.clone();
            let ship_id = p.ship_id;
            Some(Ok((3, pid, abyss_sys, r3_npcs, ship_id)))
        } else {
            Some(Err(BridgeRpcError {
                code: "AlreadyInFinalRoom".into(),
                message: "Character is already in the final room of the Abyssal pocket".into(),
            }))
        }
    });

    let Some(res) = outcome else {
        return Err(BridgeRpcError {
            code: "NotInAbyssalSpace".into(),
            message: "No active Abyssal Deadspace pocket for character".into(),
        });
    };
    let (next_room, pid, abyss_sys, npcs, ship_id) = res?;
    let sys_handle = ctx.systems.get_or_spawn(abyss_sys);
    let marker_id = 1_895_000_000 + pid * 10 + (next_room as i64);
    let z_pos = (next_room as f64 - 1.0) * 50_000.0;
    let _ = sys_handle
        .spawn_ball(crate::services::pve_incursion_sites::make_site_npc_ball(
            marker_id,
            47_956,
            DVec3::new(0.0, 0.0, z_pos),
        ))
        .await;
    for (idx, &bid) in npcs.iter().enumerate() {
        let _ = sys_handle
            .spawn_ball(crate::services::pve_incursion_sites::make_site_npc_ball(
                bid,
                47_956,
                DVec3::new((idx as f64) * 3_000.0, 5_000.0, z_pos),
            ))
            .await;
    }
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(0.0, 0.0, z_pos - 10_000.0))
        .await;

    Ok(PyRep::keyval([
        ("room", PyRep::Integer(next_room as i64)),
        ("solarSystemID", PyRep::Integer(abyss_sys as i64)),
        ("pocketID", PyRep::Integer(pid)),
        ("gateID", PyRep::Integer(gate_id)),
        ("gateActivated", PyRep::Bool(true)),
    ]))
}

async fn handle_abyssal_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let pocket_sys = session.solar_system_id.unwrap_or(32_000_001);
    let (origin_sys, origin_pos, ship_id, saved_attrs) =
        crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            if let Some(mut p) = reg.abyssal_pockets.remove(&char_id) {
                p.completed = true;
                (p.origin_solar_system_id, p.origin_position, p.ship_id, p.saved_ship_attrs)
            } else {
                let (orig, sid) = crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
                    st.abyssal_sessions
                        .get(&char_id)
                        .map(|s| (s.origin_system_id, s.ship_id))
                })
                .unwrap_or((30_000_142, session.ship_id.unwrap_or(100_000_002)));
                (orig, [145_000.0, -75_000.0, 42_000.0], sid, HashMap::new())
            }
        });

    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    ctx.systems.get_or_spawn(pocket_sys).try_remove_ball(ship_id);
    if !saved_attrs.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                for (&attr_id, &val) in &saved_attrs {
                    ship.dynamic_attributes.insert(attr_id, val);
                }
            }
        });
    }
    let exit_pos = [
        origin_pos[0] + 1_000.0,
        origin_pos[1] + 1_000.0,
        origin_pos[2] + 1_000.0,
    ];
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = origin_sys as i64;
        }
        snap.item_positions.insert(ship_id, exit_pos);
        Ok::<(), BridgeRpcError>(())
    });

    let origin_sys_handle = ctx.systems.get_or_spawn(origin_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(601));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: glam::DVec3::from_array(exit_pos),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = origin_sys_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(origin_sys);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(origin_sys as i64)),
        ("exitedAbyssalSpace", PyRep::Bool(true)),
        ("gateID", PyRep::Integer(gate_id)),
    ]))
}

async fn handle_void_space_player_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump to Void Space while docked".into(),
        });
    }
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;
    let origin_sys = session.solar_system_id.unwrap_or(30_000_142);
    let ship_id = session.ship_id.unwrap_or(100_000_002);
    let void_sys: SolarSystemId = 32_000_002;
    let now_unix = ctx.now_unix();
    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        void_sys,
        origin_sys,
        now_unix,
        now_unix.saturating_add(1_200),
    );

    // Remove ship ball from origin system destiny engine
    ctx.systems.get_or_spawn(origin_sys).try_remove_ball(ship_id);

    // Relocate character and ship to void space in DB
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = void_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = void_sys as i64;
        }
        Ok::<(), BridgeRpcError>(())
    });

    // Spawn ship ball in void space destiny engine
    let void_handle = ctx.systems.get_or_spawn(void_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(601));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: glam::DVec3::new(0.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = void_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(void_sys);

    let gate_or_site_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(void_sys as i64)),
        ("originSolarSystemID", PyRep::Integer(origin_sys as i64)),
        ("voidSpaceJump", PyRep::Bool(true)),
        ("siteID", PyRep::Integer(gate_or_site_id)),
    ]))
}

async fn handle_void_space_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let (origin_sys, ship_id) = crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.abyssal_sessions
            .get(&char_id)
            .map(|s| (s.origin_system_id, s.ship_id))
    })
    .unwrap_or((30_000_142, session.ship_id.unwrap_or(100_000_002)));

    let cur_sys = session.solar_system_id.unwrap_or(32_000_002);
    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    ctx.systems.get_or_spawn(cur_sys).try_remove_ball(ship_id);

    let exit_pos = [145_000.0, -75_000.0, 42_000.0];
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = origin_sys as i64;
        }
        snap.item_positions.insert(ship_id, exit_pos);
        Ok::<(), BridgeRpcError>(())
    });

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(601));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: glam::DVec3::from_array(exit_pos),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = origin_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(origin_sys);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(origin_sys as i64)),
        ("exitedVoidSpace", PyRep::Bool(true)),
        ("gateID", PyRep::Integer(gate_id)),
    ]))
}
