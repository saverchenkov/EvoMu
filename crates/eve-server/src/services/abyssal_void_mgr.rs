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

pub const VOID_SPACE_SYSTEM_ID: SolarSystemId = 32_000_002;
pub const ABYSSAL_SPACE_SYSTEM_ID: SolarSystemId = 32_000_001;
pub const ABYSSAL_SYSTEM_RANGE: std::ops::RangeInclusive<u32> = 32_000_000..=32_999_999;

fn err_no_solar_system() -> BridgeRpcError {
    BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solar system".into(),
    }
}

fn err_no_active_ship() -> BridgeRpcError {
    BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship".into(),
    }
}

fn err_ship_docked(msg: &str) -> BridgeRpcError {
    BridgeRpcError {
        code: "ShipDocked".into(),
        message: msg.into(),
    }
}

fn validate_allowed_ship_hull(ctx: &ServerContext, ship_id: i64) -> Result<(u32, u32), BridgeRpcError> {
    let (ship_type_id, ship_group_id) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&ship_id);
        let tid = it.map(|i| i.type_id).unwrap_or(0);
        let gid = ctx.sde.types.get(&tid).map(|t| t.group_id).unwrap_or(0);
        (tid, gid)
    });
    // Allowed: Cruisers, Frigates, Destroyers (and T2/T3 variants)
    let is_allowed = matches!(
        ship_group_id,
        25 | 26 | 324 | 358 | 420 | 541 | 830 | 831 | 832 | 833 | 834 | 893 | 894 | 906 | 963 | 1305 | 1527 | 1534 | 1972
    );
    if !is_allowed {
        return Err(BridgeRpcError {
            code: "DisallowedShipType".into(),
            message: format!("Ship type {ship_type_id} (group {ship_group_id}) is not permitted to enter Abyssal or Void Space"),
        });
    }
    Ok((ship_type_id, ship_group_id))
}

fn is_owner_or_same_fleet(ctx: &ServerContext, caller_id: i64, owner_id: i64) -> bool {
    if caller_id == owner_id {
        return true;
    }
    ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .any(|f| f.members.contains(&caller_id) && f.members.contains(&owner_id))
    })
}

fn is_actual_filament_type(ctx: &ServerContext, type_id: u32) -> bool {
    crate::services::pve_special_sites::classify_abyssal_filament(type_id).is_some()
        || ctx
            .sde
            .types
            .get(&type_id)
            .map(|t| {
                t.group_id == 1979
                    || t.group_id == 4041
                    || t.group_id == 4050
                    || t.group_id == 4145
                    || t.group_id == 4743
                    || t.name.to_lowercase().contains("filament")
            })
            .unwrap_or(false)
}

fn is_valid_void_entrance_gate(ctx: &ServerContext, target_id: i64, ball_type_id: u32) -> bool {
    if ball_type_id == 47_956 {
        return true;
    }
    let is_trace = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.abyssal_traces.contains_key(&target_id)
    });
    if is_trace {
        return true;
    }
    if let Some(t) = ctx.sde.types.get(&ball_type_id) {
        if t.group_id == 366 || t.group_id == 1991 || t.group_id == 4081 {
            return true;
        }
        let lower = t.name.to_lowercase();
        if lower.contains("gate")
            || lower.contains("conduit")
            || lower.contains("rift")
            || lower.contains("trace")
        {
            return true;
        }
    }
    false
}

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
            // BF-02: Inspect trace before validation — do NOT remove before all pre-conditions pass!
            let trace_opt = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                reg.abyssal_traces.get(&trace_id).cloned()
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
            let cur_sys = match session.solar_system_id {
                Some(s) => s,
                None => return Some(Err(err_no_solar_system())),
            };
            if !ABYSSAL_SYSTEM_RANGE.contains(&cur_sys) {
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
            let cur_sys = match session.solar_system_id {
                Some(s) => s,
                None => return Some(Err(err_no_solar_system())),
            };
            if !ABYSSAL_SYSTEM_RANGE.contains(&cur_sys) {
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
            let cur_sys = match session.solar_system_id {
                Some(s) => s,
                None => return Some(Err(err_no_solar_system())),
            };
            if !ABYSSAL_SYSTEM_RANGE.contains(&cur_sys) {
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
            // BF-01: Require session.solar_system_id == Some(VOID_SPACE_SYSTEM_ID) (32_000_002)
            let cur_sys = match session.solar_system_id {
                Some(s) => s,
                None => return Some(Err(err_no_solar_system())),
            };
            if cur_sys != VOID_SPACE_SYSTEM_ID {
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
    // 1. Caller is undocked in space
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot activate Abyssal entrance gate while docked"));
    }
    // BF-04: Eliminate fallback IDs
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    // 2. Caller has no active PvP timer
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;

    // 3. trace.origin_solar_system_id == current_sys
    if trace.origin_solar_system_id != cur_sys {
        return Err(BridgeRpcError {
            code: "WrongSolarSystem".into(),
            message: "Abyssal entrance gate is located in another solar system".into(),
        });
    }

    // 5. ctx.now_unix() < trace.expires_at_sec
    let now_u = ctx.now_unix();
    if (trace.expires_at_sec > 0 && now_u >= trace.expires_at_sec)
        || (trace.expires_at_sec == 0 && trace.created_unix > 0 && now_u >= trace.created_unix + 1_200)
    {
        crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            reg.abyssal_traces.remove(&trace.trace_ball_id);
        });
        ctx.systems.get_or_spawn(cur_sys).try_remove_ball(trace.trace_ball_id);
        return Err(BridgeRpcError {
            code: "TraceExpired".into(),
            message: "Abyssal entrance trace has expired and collapsed".into(),
        });
    }

    // 4. Ship distance to trace.position <= activation range (2500.0m)
    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    let ship_pos = if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ball.position
    } else {
        let p = ctx.db.with_snapshot(|snap| {
            snap.item_positions.get(&ship_id).copied().unwrap_or([0.0, 0.0, 0.0])
        });
        DVec3::from_array(p)
    };
    let trace_pos = DVec3::from_array(trace.position);
    let dist = ship_pos.distance(trace_pos);
    if dist > 2500.0 {
        return Err(BridgeRpcError {
            code: "OutOfRange".into(),
            message: format!("Ship is too far from Abyssal entrance gate ({dist:.1}m > 2500.0m)"),
        });
    }

    // 6. Caller is trace owner or in the same fleet as trace owner
    if !is_owner_or_same_fleet(ctx, char_id, trace.char_id) {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Caller is neither the trace owner nor in the owner's fleet".into(),
        });
    }

    // 7. Ship hull is an allowed subcapital (Cruiser, Frigate, Destroyer classes)
    validate_allowed_ship_hull(ctx, ship_id)?;

    // ALL PRE-CONDITIONS PASSED: Now remove the trace!
    crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.abyssal_traces.remove(&trace.trace_ball_id);
    });
    sys_handle.try_remove_ball(trace.trace_ball_id);

    let (tier, _) = crate::services::pve_special_sites::classify_abyssal_filament(trace.filament_type_id)
        .unwrap_or((1, crate::services::pve_special_sites::AbyssalWeatherKind::Electrical));
    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        ABYSSAL_SPACE_SYSTEM_ID,
        cur_sys,
        now_u,
        now_u + 1_200,
    );
    // P1-02: Fetch ship's current live ballpark position from origin system handle and write it to snap.item_positions
    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(ship_id, [ball.position.x, ball.position.y, ball.position.z]);
        });
    }

    let (pocket_id, room1_npcs) = crate::services::pve_special_sites::enter_abyssal_pocket_for_character(
        ctx, session, char_id, trace.filament_type_id, tier, cur_sys,
    )
    .await;

    sys_handle.try_remove_ball(ship_id);
    let abyss_handle = ctx.systems.get_or_spawn(ABYSSAL_SPACE_SYSTEM_ID);
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
    session.solar_system_id = Some(ABYSSAL_SPACE_SYSTEM_ID);

    Ok(PyRep::keyval([
        ("pocketID", PyRep::Integer(pocket_id)),
        ("solarSystemID", PyRep::Integer(ABYSSAL_SPACE_SYSTEM_ID as i64)),
        ("originSolarSystemID", PyRep::Integer(cur_sys as i64)),
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
        return Err(err_ship_docked("Cannot activate Abyssal filament while docked"));
    }
    // BF-04: Eliminate fallback IDs
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    // Ship hull class check
    validate_allowed_ship_hull(ctx, ship_id)?;

    // PvP timer check
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;
    let now_unix = ctx.now_unix();

    // BF-02: verify filament item is in ship cargo (i.location_id == ship_id) and consume it
    let resolved = ctx.db.try_with_mut_snapshot(|snap| {
        let found = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && (i.item_id == filament_arg || i.type_id as i64 == filament_arg)
                    && i.quantity >= 1
            })
            .map(|i| (i.item_id, i.type_id));
        let Some((iid, tid)) = found else {
            return Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Abyssal filament {filament_arg} not found in ship cargo"),
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
            .get(&cur_sys)
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
    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        ABYSSAL_SPACE_SYSTEM_ID,
        cur_sys,
        now_unix,
        now_unix.saturating_add(1_200),
    );
    // P1-02: Fetch ship's current live ballpark position from origin system handle and write it to snap.item_positions
    let origin_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(Some((_, ball))) = origin_handle.get_ball_snapshot(ship_id).await {
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(ship_id, [ball.position.x, ball.position.y, ball.position.z]);
        });
    }

    let (pocket_id, room1_npcs) = crate::services::pve_special_sites::enter_abyssal_pocket_for_character(
        ctx,
        session,
        char_id,
        filament_type_id,
        tier,
        cur_sys,
    )
    .await;

    origin_handle.try_remove_ball(ship_id);
    let abyss_handle = ctx.systems.get_or_spawn(ABYSSAL_SPACE_SYSTEM_ID);
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
    session.solar_system_id = Some(ABYSSAL_SPACE_SYSTEM_ID);

    Ok(PyRep::keyval([
        ("pocketID", PyRep::Integer(pocket_id)),
        ("solarSystemID", PyRep::Integer(ABYSSAL_SPACE_SYSTEM_ID as i64)),
        ("originSolarSystemID", PyRep::Integer(cur_sys as i64)),
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
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    // BF-04: Eliminate fallback IDs
    let _cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    // 1. Inspect pocket: check collapse timer, room number, and retrieve current/next room NPCs
    let (curr_room, next_room, pid, abyss_sys, curr_room_npcs, next_room_npcs, room_already_cleared, (tier, filament, next_cache, difficulty)) = {
        let inspect = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let p = reg.abyssal_pockets.get_mut(&char_id)?;
            if p.completed || p.collapsed_destroyed {
                return None;
            }
            let now = ctx.now_unix();
            // BF-03: check collapse timer
            if now >= p.collapse_at_sec {
                p.collapsed_destroyed = true;
                return Some(Err(BridgeRpcError {
                    code: "PocketCollapsed".into(),
                    message: "Abyssal Deadspace pocket has collapsed".into(),
                }));
            }
            if p.current_room >= 3 {
                return Some(Err(BridgeRpcError {
                    code: "AlreadyInFinalRoom".into(),
                    message: "Character is already in the final room of the Abyssal pocket".into(),
                }));
            }
            let curr = p.current_room;
            let next = curr + 1;
            let curr_idx = (curr - 1) as usize;
            let next_idx = (next - 1) as usize;
            let c_npcs = p.rooms[curr_idx].npc_ball_ids.clone();
            let n_npcs = p.rooms[next_idx].npc_ball_ids.clone();
            let cleared = p.rooms[curr_idx].cleared;
            let loot = (p.tier, p.filament_type_id, p.rooms[next_idx].cache_item_id, p.npc_difficulty_mult);
            Some(Ok((curr, next, p.pocket_id, p.abyssal_solar_system_id, c_npcs, n_npcs, cleared, loot)))
        });

        let Some(res) = inspect else {
            return Err(BridgeRpcError {
                code: "NotInAbyssalSpace".into(),
                message: "No active Abyssal Deadspace pocket for character".into(),
            });
        };
        res?
    };

    // 2. BF-03: Verify that NPCs spawned for the current room have been cleared
    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Validate gate_id against active room's gate
    let expected_gate_id = 1_895_000_000 + pid * 10 + (curr_room as i64);
    let alt_gate_id = 1_895_000_000 + (curr_room as i64);
    if gate_id == 0 || (gate_id != expected_gate_id && gate_id != alt_gate_id) {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: format!("Gate {gate_id} is not the active transfer gate for room {curr_room}"),
        });
    }

    if !room_already_cleared && !curr_room_npcs.is_empty() {
        let mut alive_count = 0;
        for &npc_id in &curr_room_npcs {
            if let Ok(Some(_)) = abyss_handle.get_ball_snapshot(npc_id).await {
                alive_count += 1;
            }
        }
        if alive_count > 0 {
            return Err(BridgeRpcError {
                code: "GateLocked".into(),
                message: format!("Transfer gate locked: {alive_count} hostile entities remaining in room {curr_room}"),
            });
        }
    }

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Enforce standard gate proximity (<= 2500m surface distance)
    let actual_gate_id = if abyss_handle.get_ball_snapshot(expected_gate_id).await.ok().flatten().is_some() {
        expected_gate_id
    } else {
        alt_gate_id
    };
    let ship_ball = abyss_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError { code: "SystemError".into(), message: e })?
        .map(|(_, b)| b);
    let gate_ball = abyss_handle
        .get_ball_snapshot(actual_gate_id)
        .await
        .map_err(|e| BridgeRpcError { code: "SystemError".into(), message: e })?
        .map(|(_, b)| b);
    let (Some(sb), Some(gb)) = (ship_ball, gate_ball) else {
        return Err(BridgeRpcError {
            code: "GateNotFound".into(),
            message: "Gate or ship ball not found in space".into(),
        });
    };
    let surface_dist = (sb.position.distance(gb.position) - sb.radius - gb.radius).max(0.0);
    if surface_dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "GateOutOfRange".into(),
            message: format!("Distance {surface_dist:.1} m exceeds 2,500 m gate activation proximity"),
        });
    }

    // 3. Mark current room cleared and advance current_room in registry
    let advance_res = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        let p = reg.abyssal_pockets.get_mut(&char_id)?;
        let curr_idx = (curr_room - 1) as usize;
        p.rooms[curr_idx].cleared = true;
        p.current_room = next_room;
        Some(())
    });
    if advance_res.is_none() {
        return Err(BridgeRpcError {
            code: "NotInAbyssalSpace".into(),
            message: "Abyssal pocket disappeared during gate activation".into(),
        });
    }

    // 4. Remove previous room's balls, NPC runtime entities, and unlooted cache items
    for &npc_id in &curr_room_npcs {
        abyss_handle.try_remove_ball(npc_id);
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        for &npc_id in &curr_room_npcs {
            rt.npc_entities.remove(&npc_id);
        }
    });
    let old_marker_id = 1_895_000_000 + pid * 10 + (curr_room as i64);
    abyss_handle.try_remove_ball(old_marker_id);
    let old_cache_id = 1_896_000_000 + pid * 10 + (curr_room as i64);
    abyss_handle.try_remove_ball(old_cache_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&old_cache_id);
        snap.item_positions.remove(&old_cache_id);
        snap.items.retain(|_, item| item.location_id != old_cache_id);
    });

    // 5. Spawn the next room: exit conduit, room NPCs with real SDE types + CombatState, and a
    //    Triglavian Bioadaptive Cache with persisted loot (same path as Room 1 on entry).
    let z_pos = (next_room as f64 - 1.0) * 50_000.0;
    crate::services::pve_special_sites_abyssal::spawn_abyssal_room_contents(
        ctx, &abyss_handle, pid, next_room, &next_room_npcs, next_cache, tier, filament, difficulty, z_pos,
    )
    .await;
    let _ = abyss_handle
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
    // BF-04: Eliminate fallback IDs
    let pocket_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot activate Abyssal end gate while docked"));
    }

    // 1. Inspect pocket: check collapse timer, verify p.current_room == 3, check room 3 NPCs
    let (origin_sys, origin_pos, saved_attrs, r3_npcs, r3_cleared, pid) = {
        let inspect = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let p = reg.abyssal_pockets.get_mut(&char_id)?;
            if p.completed || p.collapsed_destroyed {
                return None;
            }
            let now = ctx.now_unix();
            // BF-03: check collapse timer
            if now >= p.collapse_at_sec {
                p.collapsed_destroyed = true;
                return Some(Err(BridgeRpcError {
                    code: "PocketCollapsed".into(),
                    message: "Abyssal Deadspace pocket has collapsed".into(),
                }));
            }
            // BF-03: verify p.current_room == 3
            if p.current_room != 3 {
                return Some(Err(BridgeRpcError {
                    code: "NotInFinalRoom".into(),
                    message: format!("Exit conduit only accessible from final room (currently in room {})", p.current_room),
                }));
            }
            let r3_npcs = p.rooms[2].npc_ball_ids.clone();
            let r3_cleared = p.rooms[2].cleared;
            Some(Ok((
                p.origin_solar_system_id,
                p.origin_position,
                p.saved_ship_attrs.clone(),
                r3_npcs,
                r3_cleared,
                p.pocket_id,
            )))
        });

        let Some(res) = inspect else {
            return Err(BridgeRpcError {
                code: "NotInAbyssalSpace".into(),
                message: "No active Abyssal Deadspace pocket for character".into(),
            });
        };
        res?
    };

    // 2. BF-03: verify Room 3 NPCs are cleared
    let abyss_handle = ctx.systems.get_or_spawn(pocket_sys);

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Validate gate_id against active room 3 exit conduit
    let expected_gate_id = 1_895_000_000 + pid * 10 + 3;
    let alt_gate_id = 1_895_000_000 + 3;
    if gate_id == 0 || (gate_id != expected_gate_id && gate_id != alt_gate_id) {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: format!("Gate {gate_id} is not the active exit conduit for room 3"),
        });
    }

    if !r3_cleared && !r3_npcs.is_empty() {
        let mut alive_count = 0;
        for &npc_id in &r3_npcs {
            if let Ok(Some(_)) = abyss_handle.get_ball_snapshot(npc_id).await {
                alive_count += 1;
            }
        }
        if alive_count > 0 {
            return Err(BridgeRpcError {
                code: "GateLocked".into(),
                message: format!("Exit conduit locked: {alive_count} hostiles remaining in room 3"),
            });
        }
    }

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Enforce standard gate proximity (<= 2500m surface distance)
    let actual_gate_id = if abyss_handle.get_ball_snapshot(expected_gate_id).await.ok().flatten().is_some() {
        expected_gate_id
    } else {
        alt_gate_id
    };
    let ship_ball = abyss_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError { code: "SystemError".into(), message: e })?
        .map(|(_, b)| b);
    let gate_ball = abyss_handle
        .get_ball_snapshot(actual_gate_id)
        .await
        .map_err(|e| BridgeRpcError { code: "SystemError".into(), message: e })?
        .map(|(_, b)| b);
    let (Some(sb), Some(gb)) = (ship_ball, gate_ball) else {
        return Err(BridgeRpcError {
            code: "GateNotFound".into(),
            message: "Gate or ship ball not found in space".into(),
        });
    };
    let surface_dist = (sb.position.distance(gb.position) - sb.radius - gb.radius).max(0.0);
    if surface_dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "GateOutOfRange".into(),
            message: format!("Distance {surface_dist:.1} m exceeds 2,500 m gate activation proximity"),
        });
    }

    // 3. Remove pocket from registry and mark completed
    let removed_pid = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        if let Some(mut p) = reg.abyssal_pockets.remove(&char_id) {
            p.completed = true;
            p.rooms[2].cleared = true;
            Some(p.pocket_id)
        } else {
            None
        }
    });

    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    abyss_handle.try_remove_ball(ship_id);
    for &npc_id in &r3_npcs {
        abyss_handle.try_remove_ball(npc_id);
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        for &npc_id in &r3_npcs {
            rt.npc_entities.remove(&npc_id);
        }
    });
    if let Some(pid) = removed_pid {
        let r3_marker_id = 1_895_000_000 + pid * 10 + 3;
        let r3_cache_id = 1_896_000_000 + pid * 10 + 3;
        abyss_handle.try_remove_ball(r3_marker_id);
        abyss_handle.try_remove_ball(r3_cache_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&r3_cache_id);
            snap.item_positions.remove(&r3_cache_id);
            snap.items.retain(|_, item| item.location_id != r3_cache_id);
        });
    }

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
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
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
        return Err(err_ship_docked("Cannot jump to Void Space while docked"));
    }
    // BF-04: Eliminate fallback IDs
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    // BF-01: Enforce subcapital ship and timer constraints
    validate_allowed_ship_hull(ctx, ship_id)?;
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;

    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if target_id <= 0 {
        return Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: "VoidSpacePlayerJump requires arguments".into(),
        });
    }

    let origin_handle = ctx.systems.get_or_spawn(cur_sys);
    let ship_pos = if let Ok(Some((_, ball))) = origin_handle.get_ball_snapshot(ship_id).await {
        [ball.position.x, ball.position.y, ball.position.z]
    } else {
        ctx.db.with_snapshot(|snap| {
            snap.item_positions.get(&ship_id).copied().unwrap_or([0.0, 0.0, 0.0])
        })
    };

    // P1-01: (1) restrict cargo filament branch to i.flag == InventoryFlag::Cargo and verify i.type_id is an actual filament type before consuming.
    let is_cargo_filament = ctx.db.try_with_mut_snapshot(|snap| {
        let found = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && i.flag == eve_db::InventoryFlag::Cargo
                    && (i.item_id == target_id || i.type_id as i64 == target_id)
                    && i.quantity >= 1
                    && is_actual_filament_type(ctx, i.type_id)
            })
            .map(|i| i.item_id);
        if let Some(iid) = found {
            if let Some(item) = snap.items.get_mut(&iid) {
                if item.quantity > 1 {
                    item.quantity -= 1;
                    item.version += 1;
                } else {
                    snap.items.remove(&iid);
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    })?;

    if !is_cargo_filament {
        // P1-01: (2) in in-space gate branch, reject target_id == ship_id and verify target_b is a valid Void Space entrance gate.
        if target_id == ship_id {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: "Cannot target own ship as Void Space entrance gate".into(),
            });
        }

        // Must be an in-space gate or trace in caller's current system within activation distance
        let target_ball = origin_handle
            .get_ball_snapshot(target_id)
            .await
            .ok()
            .flatten();
        let Some((_, target_b)) = target_ball else {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: format!("Target {target_id} is neither a filament in ship cargo nor a gate in space"),
            });
        };
        if !is_valid_void_entrance_gate(ctx, target_id, target_b.type_id) {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: format!("Target {target_id} (type {}) is not a valid Void Space entrance gate", target_b.type_id),
            });
        }
        let dist = DVec3::from_array(ship_pos).distance(target_b.position);
        if dist > 5_000.0 {
            return Err(BridgeRpcError {
                code: "OutOfRange".into(),
                message: format!("Void Space gate is too far ({dist:.1}m > 5000.0m)"),
            });
        }
    }

    // BF-01: Record caller's origin system and position for return in a distinct Void Space session record
    let now_unix = ctx.now_unix();
    let void_session = crate::services::pve_special_sites::VoidSpaceSessionRecord {
        char_id,
        ship_id,
        origin_solar_system_id: cur_sys,
        origin_position: ship_pos,
        target_id,
        entered_unix: now_unix,
        expires_at_sec: now_unix.saturating_add(1_200),
    };
    crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions.insert(char_id, void_session);
    });

    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        VOID_SPACE_SYSTEM_ID,
        cur_sys,
        now_unix,
        now_unix.saturating_add(1_200),
    );

    // Remove ship ball from origin system destiny engine
    origin_handle.try_remove_ball(ship_id);

    // Relocate character and ship to void space in DB
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = VOID_SPACE_SYSTEM_ID;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = VOID_SPACE_SYSTEM_ID as i64;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        Ok::<(), BridgeRpcError>(())
    });

    // Spawn ship ball in void space destiny engine
    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
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
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(VOID_SPACE_SYSTEM_ID as i64)),
        ("originSolarSystemID", PyRep::Integer(cur_sys as i64)),
        ("voidSpaceJump", PyRep::Bool(true)),
        ("siteID", PyRep::Integer(target_id)),
    ]))
}

async fn handle_void_space_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

    // BF-01: 1. Require session.solar_system_id == Some(VOID_SPACE_SYSTEM_ID) (32_000_002)
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    if cur_sys != VOID_SPACE_SYSTEM_ID {
        return Err(BridgeRpcError::user_error("NotInVoidSpace"));
    }

    // BF-01: 2. Require ship to be undocked in space (station_id is None)
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot activate Void Space end gate while docked"));
    }

    // BF-04: Require active ship from session
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    // BF-01: 3. Require an active Void Space session record (separate from an Abyssal pocket session)
    let void_session = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions.remove(&char_id)
    });
    let Some(vs) = void_session else {
        return Err(BridgeRpcError {
            code: "NotInVoidSpace".into(),
            message: "No active Void Space session record for character".into(),
        });
    };

    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID).try_remove_ball(ship_id);

    // BF-01: 4. Restore the character's recorded entry position rather than hardcoded [145_000.0, -75_000.0, 42_000.0] or fallback to Jita
    let origin_sys = vs.origin_solar_system_id;
    let exit_pos = [
        vs.origin_position[0] + 1_000.0,
        vs.origin_position[1] + 1_000.0,
        vs.origin_position[2] + 1_000.0,
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

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
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
