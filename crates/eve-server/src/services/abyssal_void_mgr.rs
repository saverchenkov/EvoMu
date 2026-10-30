//! `abyssal_void_mgr` — Abyssal Deadspace & Void Space manager (`abyssalMgr`, `voidSpaceMgr`).
//!
//! Handles RPC dispatch, Abyssal entrance trace/filament activation, and delegates room/end gate
//! progression and Void Space transitions to `abyssal_void_gates`.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use crate::services::abyssal_void_gates::{
    handle_abyssal_end_gate_activation, handle_abyssal_gate_activation,
    handle_void_space_end_gate_activation, handle_void_space_player_jump,
};
use crate::services::pve_incursion_sites::make_site_npc_ball;
use crate::services::pve_special_sites::{next_spec_seq, with_pve_special_reg};
use crate::services::pve_special_sites_abyssal::{
    pocket_grid_offset, validate_abyssal_hull_and_capacity, validate_and_consume_abyssal_filaments,
    AbyssalTraceRecord, ABYSSAL_TRACE_TYPE_ID,
};
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

pub(crate) fn err_no_solar_system() -> BridgeRpcError {
    BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solar system".into(),
    }
}

pub(crate) fn err_no_active_ship() -> BridgeRpcError {
    BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship".into(),
    }
}

pub(crate) fn err_ship_docked(msg: &str) -> BridgeRpcError {
    BridgeRpcError {
        code: "ShipDocked".into(),
        message: msg.into(),
    }
}

pub(crate) fn validate_allowed_ship_hull(
    ctx: &ServerContext,
    ship_id: i64,
) -> Result<(u32, u32), BridgeRpcError> {
    let (tid, gid, _) = validate_abyssal_hull_and_capacity(ctx, ship_id)?;
    Ok((tid, gid))
}

fn is_in_trace_fleet(ctx: &ServerContext, caller_id: i64, owner_id: i64) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .any(|f| f.members.contains(&caller_id) && f.members.contains(&owner_id))
    })
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(restricted) =
        crate::services::pve_special_sites_abyssal::check_abyssal_in_pocket_rpc_restrictions(
            session, service, method, args,
        )
    {
        return Some(restricted);
    }
    match (service, method) {
        ("abyssalMgr", "AbyssalEntranceDeployment") => {
            if args.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "MissingRequiredArgument".into(),
                    message: "AbyssalEntranceDeployment requires filament item ID".into(),
                }));
            }
            Some(crate::services::pve_special_sites::handle_abyssal_entrance_deployment(ctx, session, args).await)
        }

        ("abyssalMgr", "AbyssalEntranceGateActivation") => {
            if args.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "MissingRequiredArgument".into(),
                    message: "AbyssalEntranceGateActivation requires gateID or filament itemID".into(),
                }));
            }
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let trace_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let trace_opt = with_pve_special_reg(ctx, |reg| reg.abyssal_traces.get(&trace_id).cloned());
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
                    code: "NotInAbyssalSpace".into(),
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
                    code: "NotInAbyssalSpace".into(),
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
                    code: "NotInAbyssalSpace".into(),
                    message: "ClientIsReady for abyssalMgr only valid inside Abyssal Deadspace".into(),
                }));
            }
            crate::services::pve_special_sites::ensure_abyssal_room_balls_for_ctx(ctx, char_id).await;
            if let Some(ship_id) = session.ship_id {
                let _ = crate::services::pve_special_sites_abyssal::evaluate_abyssal_spatial_clouds_for_ship(
                    ctx, cur_sys, ship_id,
                )
                .await;
            }
            Some(Ok(PyRep::keyval([("ready", PyRep::Bool(true))])))
        }

        ("voidSpaceMgr", "ClientIsReady") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.solar_system_id == Some(VOID_SPACE_SYSTEM_ID) {
                let vs_opt = with_pve_special_reg(ctx, |reg| reg.void_space_sessions.get(&char_id).cloned());
                if let Some(vs) = vs_opt {
                    let persisted_cid = ctx.db.with_snapshot(|snap| {
                        snap.service_state_blobs
                            .get(&format!("void_space_content:{char_id}"))
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or(vs.target_id)
                    });
                    let rem_sec = vs.expires_at_sec.saturating_sub(ctx.now_unix()) as i64;
                    let end_ft = ctx.now_blueos_filetime_ticks() + rem_sec.max(1) * 10_000_000;
                    // DATA GAP: V24 live room IDs and nebula graphic IDs are Tranquility server-only dynamic instancing data
                    let client_data = PyRep::Dict(vec![
                        (PyRep::String("content_id".into()), PyRep::Integer(persisted_cid)),
                        (PyRep::String("nebula_graphic_id".into()), PyRep::Integer(200_001)),
                        (PyRep::String("end_time".into()), PyRep::Integer(end_ft)),
                    ]);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnVoidSpaceRelog",
                        "charid",
                        vec![client_data],
                    );
                }
            }
            Some(Ok(PyRep::keyval([("ready", PyRep::Bool(true))])))
        }

        ("voidSpaceMgr", "VoidSpacePlayerJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "MissingRequiredArgument".into(),
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
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot activate Abyssal entrance gate while docked"));
    }
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, char_id, ship_id, cur_sys)?;

    if trace.origin_solar_system_id != cur_sys {
        return Err(BridgeRpcError {
            code: "WrongSolarSystem".into(),
            message: "Abyssal entrance gate is located in another solar system".into(),
        });
    }

    let now_u = ctx.now_unix();
    let fallback_ttl = if trace.pocket_id.is_some() {
        1_200
    } else {
        crate::services::pve_special_sites_abyssal::UNENTERED_GATE_LIFETIME_SEC
    };
    if (trace.expires_at_sec > 0 && now_u >= trace.expires_at_sec)
        || (trace.expires_at_sec == 0
            && trace.created_unix > 0
            && now_u >= trace.created_unix + fallback_ttl)
    {
        with_pve_special_reg(ctx, |reg| {
            reg.abyssal_traces.remove(&trace.trace_ball_id);
        });
        ctx.systems.get_or_spawn(cur_sys).try_remove_ball(trace.trace_ball_id);
        return Err(BridgeRpcError {
            code: "TraceExpired".into(),
            message: "Abyssal entrance trace has expired and collapsed".into(),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
        if cs.is_cloaked {
            return Err(BridgeRpcError {
                code: "ShipIsCloaked".into(),
                message: "Cannot activate Abyssal entrance trace while cloaked".into(),
            });
        }
        if cs.is_tethered {
            return Err(BridgeRpcError {
                code: "ShipIsTethered".into(),
                message: "Cannot activate Abyssal entrance trace while tethered".into(),
            });
        }
    }
    let ship_pos = if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ball.position
    } else {
        let p = ctx.db.with_snapshot(|snap| {
            snap.item_positions
                .get(&ship_id)
                .copied()
                .unwrap_or([0.0, 0.0, 0.0])
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

    let eff_cap = if trace.max_ships == 0 { 1 } else { trace.max_ships };
    let (_, _, hull_cap) = validate_abyssal_hull_and_capacity(ctx, ship_id)?;
    if hull_cap != eff_cap {
        return Err(BridgeRpcError {
            code: "DisallowedShipType".into(),
            message: format!(
                "Trace requires ship class capacity {eff_cap}, but activating ship has capacity {hull_cap}"
            ),
        });
    }
    if eff_cap == 1 {
        if char_id != trace.char_id {
            return Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Solo Cruiser Abyssal trace may only be entered by its creator".into(),
            });
        }
    } else if char_id != trace.char_id && !is_in_trace_fleet(ctx, char_id, trace.char_id) {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Caller is neither the trace owner nor in the owner's fleet".into(),
        });
    }

    let (tier, _weather) =
        crate::services::pve_special_sites::classify_abyssal_filament(trace.filament_type_id)
            .unwrap_or((1, crate::services::pve_special_sites::AbyssalWeatherKind::Electrical));
    let origin_sec = ctx
        .sde
        .solar_systems
        .get(&cur_sys)
        .map(|s| s.security_status)
        .unwrap_or(0.9);
    let gives_suspect =
        crate::services::pve_special_sites_abyssal::evaluate_highsec_abyssal_tier_policy(
            origin_sec, tier,
        )?;

    let entry_check = with_pve_special_reg(ctx, |reg| {
        if let Some(t) = reg.abyssal_traces.get_mut(&trace.trace_ball_id) {
            if t.entered_char_ids.contains(&char_id) {
                return Err(BridgeRpcError {
                    code: "TraceAlreadyEntered".into(),
                    message: "Pilot has already entered this Abyssal entrance trace".into(),
                });
            }
            if t.entered_char_ids.len() >= eff_cap as usize {
                return Err(BridgeRpcError {
                    code: "TraceShipLimitReached".into(),
                    message: format!(
                        "Abyssal trace ship limit ({eff_cap}) reached for this hull class"
                    ),
                });
            }
            t.entered_char_ids.push(char_id);
        }
        Ok(())
    });
    entry_check?;

    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions
                .insert(ship_id, [ball.position.x, ball.position.y, ball.position.z]);
        });
    }

    let entry_now = ctx.now_unix();
    if gives_suspect {
        ctx.crimewatch
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .trigger_suspect_flag(char_id, entry_now);
    }
    let (pocket_id, room1_npcs) =
        crate::services::pve_special_sites_abyssal::enter_abyssal_pocket_with_trace(
            ctx,
            session,
            char_id,
            trace.filament_type_id,
            tier,
            cur_sys,
            Some(trace.trace_ball_id),
        )
        .await;
    let remaining_sec = with_pve_special_reg(ctx, |reg| {
        reg.abyssal_pockets
            .get(&char_id)
            .map(|p| {
                if p.fleet_ship_ids.len() <= 1 {
                    1_200
                } else {
                    p.collapse_at_sec.saturating_sub(entry_now).min(1_200) as i64
                }
            })
            .unwrap_or(1_200)
    });

    sys_handle.try_remove_ball(ship_id);
    let abyss_handle = ctx.systems.get_or_spawn(ABYSSAL_SPACE_SYSTEM_ID);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    for r in cs.shield_resonance.iter_mut().chain(cs.armor_resonance.iter_mut()).chain(cs.structure_resonance.iter_mut()) {
        *r = r.clamp(0.0, 1.0);
    }
    cs.capacitor_recharge_time_ms = cs.capacitor_recharge_time_ms.max(1_000.0);
    let base = DVec3::from_array(pocket_grid_offset(ctx, pocket_id));
    let ball = crate::services::pve_abyssal_pylons::build_fitted_ship_space_ball(
        ctx,
        char_id,
        ship_id,
        ship_type,
        base + DVec3::new(0.0, 0.0, -50_000.0),
    );
    let _ = abyss_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(ABYSSAL_SPACE_SYSTEM_ID);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, ABYSSAL_SPACE_SYSTEM_ID);
    emit_abyssal_entry_notifications(ctx, char_id, pocket_id, tier, remaining_sec);

    Ok(PyRep::keyval([
        ("pocketID", PyRep::Integer(pocket_id)),
        ("solarSystemID", PyRep::Integer(ABYSSAL_SPACE_SYSTEM_ID as i64)),
        ("originSolarSystemID", PyRep::Integer(cur_sys as i64)),
        ("filamentTypeID", PyRep::Integer(trace.filament_type_id as i64)),
        ("tier", PyRep::Integer(tier as i64)),
        ("suspectFlagOnExit", PyRep::Bool(gives_suspect)),
        ("currentRoom", PyRep::Integer(1)),
        ("timerSeconds", PyRep::Integer(remaining_sec)),
        (
            "npcBallIDs",
            PyRep::List(room1_npcs.iter().map(|&id| PyRep::Integer(id)).collect()),
        ),
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
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    let (_iid, filament_type_id, tier, max_ships, origin_sec, ship_pos) =
        validate_and_consume_abyssal_filaments(ctx, char_id, ship_id, cur_sys, filament_arg).await?;

    let now_unix = ctx.now_unix();
    let origin_handle = ctx.systems.get_or_spawn(cur_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions
            .insert(ship_id, [ship_pos.x, ship_pos.y, ship_pos.z]);
    });

    // Spawn scannable Abyssal Trace in the origin system so direct filament entry also leaves a real trace
    let trace_pos = ship_pos + DVec3::new(500.0, 0.0, 0.0);
    let trace_ball_id = with_pve_special_reg(ctx, |reg| {
        let tid = 1_897_000_000 + next_spec_seq(reg);
        reg.abyssal_traces.insert(
            tid,
            AbyssalTraceRecord {
                trace_ball_id: tid,
                char_id,
                filament_type_id,
                origin_solar_system_id: cur_sys,
                position: [trace_pos.x, trace_pos.y, trace_pos.z],
                created_unix: now_unix,
                expires_at_sec: now_unix + 1_200,
                max_ships,
                entered_char_ids: vec![char_id],
                pocket_id: None,
            },
        );
        tid
    });
    let _ = origin_handle
        .spawn_ball(make_site_npc_ball(
            trace_ball_id,
            ABYSSAL_TRACE_TYPE_ID,
            trace_pos,
        ))
        .await;

    let (pocket_id, room1_npcs) =
        crate::services::pve_special_sites_abyssal::enter_abyssal_pocket_with_trace(
            ctx,
            session,
            char_id,
            filament_type_id,
            tier,
            cur_sys,
            Some(trace_ball_id),
        )
        .await;

    origin_handle.try_remove_ball(ship_id);
    let abyss_handle = ctx.systems.get_or_spawn(ABYSSAL_SPACE_SYSTEM_ID);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    for r in cs.shield_resonance.iter_mut().chain(cs.armor_resonance.iter_mut()).chain(cs.structure_resonance.iter_mut()) {
        *r = r.clamp(0.0, 1.0);
    }
    cs.capacitor_recharge_time_ms = cs.capacitor_recharge_time_ms.max(1_000.0);
    let base = DVec3::from_array(pocket_grid_offset(ctx, pocket_id));
    let ball = crate::services::pve_abyssal_pylons::build_fitted_ship_space_ball(
        ctx,
        char_id,
        ship_id,
        ship_type,
        base + DVec3::new(0.0, 0.0, -50_000.0),
    );
    let _ = abyss_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(ABYSSAL_SPACE_SYSTEM_ID);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, ABYSSAL_SPACE_SYSTEM_ID);
    emit_abyssal_entry_notifications(ctx, char_id, pocket_id, tier, 1_200);
    let gives_suspect =
        crate::services::pve_special_sites_abyssal::evaluate_highsec_abyssal_tier_policy(
            origin_sec, tier,
        )
        .unwrap_or(false);

    Ok(PyRep::keyval([
        ("pocketID", PyRep::Integer(pocket_id)),
        ("solarSystemID", PyRep::Integer(ABYSSAL_SPACE_SYSTEM_ID as i64)),
        ("originSolarSystemID", PyRep::Integer(cur_sys as i64)),
        ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
        ("tier", PyRep::Integer(tier as i64)),
        ("suspectFlagOnExit", PyRep::Bool(gives_suspect)),
        ("currentRoom", PyRep::Integer(1)),
        ("timerSeconds", PyRep::Integer(1_200)),
        ("traceBallID", PyRep::Integer(trace_ball_id)),
        (
            "npcBallIDs",
            PyRep::List(room1_npcs.iter().map(|&id| PyRep::Integer(id)).collect()),
        ),
    ]))
}

fn emit_abyssal_entry_notifications(
    ctx: &ServerContext,
    char_id: i64,
    pocket_id: i64,
    tier: u8,
    remaining_sec: i64,
) {
    // DATA GAP: V24 live room IDs and nebula graphic IDs are Tranquility server-only dynamic instancing data
    let client_data = PyRep::Dict(vec![
        (PyRep::String("content_id".into()), PyRep::Integer(pocket_id)),
        (PyRep::String("nebula_graphic_id".into()), PyRep::Integer(21_000 + tier as i64)),
        (PyRep::String("difficulty_tier".into()), PyRep::Integer(tier as i64)),
        (PyRep::String("location_id".into()), PyRep::Integer(ABYSSAL_SPACE_SYSTEM_ID as i64)),
        (PyRep::String("rooms_completed".into()), PyRep::Integer(0)),
    ]);
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAbyssalLocationEntered",
        "charid",
        vec![client_data],
    );
    let end_ft = ctx.now_blueos_filetime_ticks() + remaining_sec.max(1) * 10_000_000;
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAddExpiryDurationForAbyssalContent",
        "charid",
        vec![PyRep::Integer(end_ft)],
    );
}
