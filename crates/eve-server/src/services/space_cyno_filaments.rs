//! `space_cyno_filaments` — Wormhole jumps, Abyssal/Void Filaments, Pharolux Cyno Beacons, D-Scan, and Mining (`S-3`, `SP-4`, `MN`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

use super::{execute_cosmic_wormhole_jump, AU_IN_METERS};

const BEYONCE_SVC: &str = "beyonce";

pub(crate) async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera
        // parity: rpc:wormholeMgr.WormholeJump
        ("wormholeMgr", "WormholeJump") => Some(handle_wormhole_jump(ctx, session, args).await),

        // parity: feat:navigation.filament-needlejack
        ("randomJumpMgr", "ActivateRandomJumpFilament") => {
            Some(handle_random_jump_filament(ctx, session, args).await)
        }

        // parity: plan:S-3, feat:navigation.pharolux-beacon
        // parity: rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon, rpc:structureCynoBeaconMgr.CmdBridgeToStructureBeacon, rpc:structureCynoBeaconMgr.CmdGroupJumpToStructureBeacon
        ("structureCynoBeaconMgr", "GetMyAccessibleOnlineCynoBeaconStructures") => {
            Some(handle_get_beacon_structures(ctx, session))
        }
        ("structureCynoBeaconMgr", "CmdJumpToStructureBeacon") => {
            Some(handle_cmd_jump_to_structure_beacon(ctx, session, args).await)
        }
        ("structureCynoBeaconMgr", "CmdBridgeToStructureBeacon") => {
            Some(handle_cmd_bridge_to_structure_beacon(ctx, session, args).await)
        }
        ("structureCynoBeaconMgr", "CmdGroupJumpToStructureBeacon") => {
            Some(handle_cmd_group_jump_to_structure_beacon(ctx, session, args).await)
        }
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        ("beyonce", "CmdBeaconJumpFleet") => {
            Some(handle_cmd_beacon_jump_fleet(ctx, session, args).await)
        }

        // parity: feat:scanning.dscan
        ("scanMgr", "ConeScan") | ("scanMgr", "DirectionalScan") => {
            handle_cone_scan(ctx, session, bound_entry, method, args).await
        }

        ("entity", "CmdMineRepeatedly")
        | ("beyonce", "CmdMineRepeatedly")
        | ("dogmaIM", "Mine") => {
            Some(crate::services::space_entity_mining::handle_mine(ctx, session, bound_entry, args).await)
        }

        _ => None,
    }
}

async fn handle_wormhole_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump wormhole while docked".into(),
        });
    }
    let Some(old_sys) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        });
    };
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "jump wormhole")?;

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((_, mut current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not in space".into(),
        });
    };
    let now_unix = ctx.now_unix();
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx, char_id, ship_id, old_sys, "wormhole_jump", now_unix,
    )?;

    let ship_type_id = ctx
        .db
        .get_item(ship_id)
        .map(|i| i.type_id)
        .unwrap_or(current_ball.type_id);
    ctx.validate_ship_movement(
        char_id,
        ship_id,
        crate::services::space_nav_movement_guard::MovementIntent::WormholeJump {
            from_sys: old_sys,
            max_jump_mass: None,
        },
    )
    .await?;

    let _ship_group_id = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);

    crate::services::tactical_modules::prepare_ship_for_wormhole_jump(
        ctx,
        &old_handle,
        char_id,
        ship_id,
        &mut current_ball,
    )
    .await?;

    let wh_arg_i64 = args.first().and_then(|a| a.as_i64());
    let wh_arg_str = args.first().and_then(|a| a.as_str());
    if wh_arg_i64 != Some(750_000_001) {
        if let Some(res) = execute_cosmic_wormhole_jump(
            ctx, session, char_id, old_sys, ship_id, current_ball.clone(), wh_arg_i64, wh_arg_str,
        )
        .await
        {
            return res;
        }
    }
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx, char_id, ship_id, old_sys, "wh_jump", now_unix,
    )?;

    let Some(wh_ball_id) = wh_arg_i64 else {
        return Err(BridgeRpcError {
            code: "MissingWormholeBallId".into(),
            message: "WormholeJump requires an explicit wormhole ballID argument".into(),
        });
    };
    let Some((_, wh_ball)) = old_handle
        .get_ball_snapshot(wh_ball_id)
        .await
        .map_err(|e| BridgeRpcError { code: "DestinyQueryFailed".into(), message: e })?
    else {
        return Err(BridgeRpcError {
            code: "WormholeNotPresent".into(),
            message: format!("No wormhole present in solar system {old_sys}"),
        });
    };
    if wh_ball.type_id != 26272 {
        return Err(BridgeRpcError {
            code: "WormholeNotPresent".into(),
            message: format!("Ball {wh_ball_id} is not a Wormhole K162 (26272)"),
        });
    }

    let jump_err = |code: &str, message: String| -> BridgeRpcError {
        BridgeRpcError { code: code.into(), message }
    };

    let prep_res: Result<(u32, DVec3), BridgeRpcError> = (|| {
        let dist = current_ball.position.distance(wh_ball.position);
        if dist > 2_500.0 {
            return Err(jump_err(
                "OutsideWormholeJumpRange",
                format!("Ship is {:.1} m from wormhole (must be <= 2500 m)", dist),
            ));
        }

        let (ship_mass, _) =
            super::space_docking_cloaking::compute_effective_ship_jump_mass(ctx, ship_id, current_ball.mass);
        let now_unix = ctx.now_unix();
        if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
            return Err(jump_err(
                "SessionChangeTimerActive",
                "Session change timer is active".into(),
            ));
        }

        ctx.db.try_with_mut_snapshot(|snap| {
            let Some(dest_sys) = snap
                .system_states
                .get(&old_sys)
                .and_then(|st| st.wormhole_destination_system_id)
            else {
                return Err(jump_err(
                    "NoWormholeDestination",
                    format!("Solar system {old_sys} has no active wormhole destination"),
                ));
            };
            let pol_expiry = snap
                .wh_polarizations
                .get(&(char_id, dest_sys as i64))
                .copied()
                .unwrap_or(0);
            if pol_expiry > now_unix {
                return Err(jump_err(
                    "WormholePolarized",
                    format!(
                        "Character is polarized for system {} for another {} seconds",
                        dest_sys,
                        pol_expiry.saturating_sub(now_unix)
                    ),
                ));
            }
            let max_jump = snap
                .cosmic_signatures
                .get(&old_sys)
                .and_then(|sigs| {
                    sigs.iter()
                        .find(|s| s.sig_group == "Wormhole" && s.destination_system_id == Some(dest_sys))
                        .map(|s| s.max_jump_mass_kg)
                })
                .unwrap_or(0.0);
            if max_jump > 0.0 && ship_mass > max_jump {
                return Err(jump_err(
                    "ShipTooHeavyForWormhole",
                    format!(
                        "Ship mass {:.1} kg exceeds wormhole jump limit {:.1} kg",
                        ship_mass, max_jump
                    ),
                ));
            }
            for sys in [old_sys, dest_sys] {
                if let Some(st) = snap.system_states.get_mut(&sys) {
                    if st.wormhole_remaining_mass_kg < ship_mass {
                        return Err(jump_err(
                            "WormholeMassCollapsed",
                            "Wormhole lacks remaining mass capacity for this ship".into(),
                        ));
                    }
                    st.wormhole_remaining_mass_kg -= ship_mass;
                }
            }
            snap.wh_polarizations
                .insert((char_id, dest_sys as i64), now_unix + 300);
            snap.wh_polarizations
                .insert((char_id, old_sys as i64), now_unix + 300);
            let sig_pos = snap
                .system_states
                .get(&dest_sys)
                .and_then(|st| st.signature_position)
                .map(DVec3::from_array)
                .unwrap_or(DVec3::new(10_500.0, 0.0, 0.0));
            Ok((dest_sys, sig_pos))
        })
    })();

    let (dest_sys_id, dest_sig_pos) = match prep_res {
        Ok(v) => v,
        Err(e) => {
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnWormholeJumpCancel",
                "charid",
                vec![],
            );
            return Err(e);
        }
    };

    let prev_combat_state = old_handle.get_combat_state(ship_id).await.ok().flatten();
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, old_sys);
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
    let existing_ball = old_handle.remove_ball(ship_id).await;
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
        .map_err(|e| BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: e.to_string(),
        })?;
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

    let arrival_pos = dest_sig_pos + DVec3::new(2_000.0, 0.0, 0.0);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(current_ball);
    new_ball.position = arrival_pos;
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(&mut combat_state, prev_combat_state);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;
    let dest_tick = new_handle
        .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
        .await
        .unwrap_or(0);
    let _ = new_handle.push_set_state(ship_id).await;
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions
            .insert(ship_id, [arrival_pos.x, arrival_pos.y, arrival_pos.z]);
    });
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys_id,
        dest_tick,
        "wormhole_jump",
    );
    let _ = ctx
        .living_world
        .materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde)
        .await;

    let exit_wh_ball_id = crate::services::r6_t1_p1_helpers::spawn_distinct_exit_wormhole(
        ctx,
        &new_handle,
        old_sys,
        dest_sys_id,
        wh_ball_id,
        wh_ball.position,
        dest_sig_pos,
    )
    .await;

    let _ = updated_ch;
    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::TransitionKind::JumpSystem { solar_system_id: dest_sys_id },
            "wormhole_jump",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );
    crate::services::map_service::record_solar_system_jump(ctx, char_id, dest_sys_id);
    crate::services::combat_crimewatch::schedule_highsec_criminal_concord_if_needed(
        ctx,
        char_id,
        ship_id,
        dest_sys_id,
        now_unix,
    );

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
        ("exitWormholeBallID", PyRep::Integer(exit_wh_ball_id)),
    ]))
}

pub use super::space_docking_cloaking::{
    derive_wormhole_buckets, derive_wormhole_buckets_raw, WormholeBuckets,
};

async fn handle_random_jump_filament(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError { code: "ShipDocked".into(), message: "Cannot activate filament while docked".into() });
    }
    let Some(old_sys) = session.solar_system_id else {
        return Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solarSystemID".into() });
    };
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError { code: "NoActiveShip".into(), message: "Session has no active ship".into() });
    };
    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, char_id, ship_id, old_sys)?;

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((cur_tick, mut current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => return Err(BridgeRpcError { code: "DestinyQueryFailed".into(), message: e }),
    }) else {
        return Err(BridgeRpcError { code: "ShipNotInSpace".into(), message: "Active ship ball is not in space".into() });
    };
    let now_unix = ctx.now_unix();
    if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(
        ctx, char_id, now_unix, Some((old_sys, cur_tick)),
    ) {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!("Cannot activate jump filament while 10s session-change timer ({}) is active", active_timer.reason),
        });
    }
    crate::services::tactical_modules::prepare_ship_for_wormhole_jump(ctx, &old_handle, char_id, ship_id, &mut current_ball).await?;

    if let Ok(Some(mut cs)) = old_handle.get_combat_state(ship_id).await {
        cs.sync_action_breaks();
        if cs.is_warp_exit_invulnerable() {
            return Err(BridgeRpcError {
                code: "ShipInvulnerable".into(),
                message: "Cannot activate filament while ship is in 10s post-warp invulnerability window".into(),
            });
        }
        if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 {
            return Err(BridgeRpcError {
                code: "CannotActivateFilamentWhileCloaked".into(),
                message: "Cannot activate filament while ship is cloaked".into(),
            });
        }
    }

    let caller_ship_group = ctx.sde.types.get(&current_ball.type_id).map(|t| t.group_id).unwrap_or(0);
    if matches!(caller_ship_group, 30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594) {
        return Err(BridgeRpcError { code: "ShipCapitalRestricted".into(), message: "Random jump filaments cannot be activated on capital ships".into() });
    }

    let caller_pos = current_ball.position;
    crate::services::pve_ess_reserve_banks::check_ess_intrusion_bubble_restriction(
        ctx, old_sys, ship_id, "filament",
    )
    .await?;
    crate::services::pve_abyssal_hazards::check_not_in_deadspace_pocket(ctx, old_sys, caller_pos)
        .await?;

    let fleet_members: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.fleets.values().find(|f| f.members.contains(&char_id)).map(|f| f.members.clone())
    }).ok_or_else(|| BridgeRpcError {
        code: "NotInFleet".into(),
        message: "Random jump filaments require being in a fleet".into(),
    })?;

    let too_close = |pos: DVec3| caller_pos.distance(pos) < 1_000_000.0;
    let station_close = ctx.sde.stations.values().any(|s| s.solar_system_id == old_sys && too_close(s.position));
    let gate_close = ctx.sde.stargates().values().any(|g| g.from_solar_system_id == old_sys && too_close(g.position));
    let struct_close = ctx.db.with_snapshot(|snap| {
        snap.structures.values().any(|s| s.solar_system_id == old_sys && s.state != "Destroyed" && too_close(DVec3::from_array(s.position)))
    });
    if station_close || gate_close || struct_close {
        return Err(BridgeRpcError {
            code: "TooCloseToStructureOrStation".into(),
            message: "Must be at least 1,000 km away from stations, stargates, and structures to activate filament".into(),
        });
    }

    const GROUP_JUMP_FILAMENTS: u32 = 4041;
    const GROUP_POCHVEN_FILAMENTS: u32 = 4087;
    let fil_item_id = if let Some(id) = args.first().and_then(|a| a.as_i64()) {
        id
    } else {
        ctx.db.with_snapshot(|snap| {
            snap.items.values().find(|i| {
                i.owner_id == char_id && i.location_id == ship_id && i.flag == eve_db::InventoryFlag::Cargo && i.quantity >= 1
                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    && matches!(ctx.sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0), GROUP_JUMP_FILAMENTS | GROUP_POCHVEN_FILAMENTS)
            }).map(|i| i.item_id)
        }).ok_or_else(|| BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "ActivateRandomJumpFilament requires filament itemID argument".into(),
        })?
    };

    let (fil_type_id, max_capacity, is_signal, is_pochven_filament) = ctx.db.with_snapshot(|snap| {
        if let Some(raw_fil) = snap.items.get(&fil_item_id) {
            if raw_fil.flag == eve_db::InventoryFlag::Locked
                || snap.is_item_locked_in_contract_or_job(fil_item_id)
            {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: format!("Filament {fil_item_id} is locked in a contract or industry job"),
                });
            }
        }
        let fil = snap.items.get(&fil_item_id).filter(|i| {
            i.owner_id == char_id && i.location_id == ship_id && i.flag == eve_db::InventoryFlag::Cargo && i.quantity >= 1
        }).ok_or_else(|| BridgeRpcError {
            code: "FilamentNotInCargo".into(),
            message: format!("Filament {fil_item_id} not found in active ship Cargo"),
        })?;
        let sde_type = ctx.sde.types.get(&fil.type_id).filter(|t| matches!(t.group_id, GROUP_JUMP_FILAMENTS | GROUP_POCHVEN_FILAMENTS)).ok_or_else(|| {
            BridgeRpcError { code: "InvalidItemType".into(), message: "Item is not a jump or Pochven filament".into() }
        })?;
        if sde_type.group_id == GROUP_POCHVEN_FILAMENTS || (56_065..=56_081).contains(&fil.type_id) {
            let cap = sde_type.default_attributes.get(&2832).copied().map(|v| v as usize).unwrap_or_else(|| {
                if fil.type_id == 56_065 { 1 } else if matches!(fil.type_id, 56_067 | 56_069 | 56_071 | 56_073 | 56_075 | 56_077 | 56_079 | 56_081) { 15 } else { 5 }
            });
            return Ok((fil.type_id, cap, false, true));
        }
        let attr_3025 = sde_type.default_attributes.get(&3025).copied().ok_or_else(|| BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: format!("Filament type {} does not support needlejack random jump", fil.type_id),
        })?;
        let cap = sde_type.default_attributes.get(&2832).copied().unwrap_or(5.0) as usize;
        Ok((fil.type_id, cap, attr_3025 >= 0.5, false))
    })?;

    let old_reg = ctx.sde.solar_systems.get(&old_sys).map(|s| s.region_id).unwrap_or(0);
    let candidate_systems: Vec<u32> = if is_pochven_filament {
        let is_exit = (56_065..=56_069).contains(&fil_type_id);
        if is_exit && old_reg != 10_000_070 {
            return Err(BridgeRpcError::user_error("PochvenExitFilamentRequiresPochvenOrigin"));
        }
        if !is_exit && (old_reg == 10_000_070 || old_sys == 30_100_000 || old_sys >= 31_000_000) {
            return Err(BridgeRpcError::user_error("PochvenEntryFilamentDisallowedOrigin"));
        }
        crate::services::space_destiny::space_pochven_wormholes::resolve_pochven_filament_candidates(
            ctx,
            fil_type_id,
            old_sys,
            char_id,
        )?
    } else {
        if old_reg == 10_000_070
            || old_sys == 30_100_000
            || (32_000_000..=32_999_999).contains(&old_sys)
        {
            return Err(BridgeRpcError::user_error("NeedlejackDisallowedOrigin"));
        }
        ctx.sde.solar_systems.iter().filter(|(&sid, s)| {
            sid != old_sys && s.security_status < 0.0 && sid < 31_000_000
                && !matches!(s.region_id, 10000070 | 10000004 | 10000017 | 10000019 | 10001000)
        }).map(|(&sid, _)| sid).collect()
    };
    if candidate_systems.is_empty() {
        return Err(BridgeRpcError { code: "NoNullsecSystemInSde".into(), message: "No destination solar system found in SDE".into() });
    }
    let idx = ((char_id as usize) ^ (old_sys as usize) ^ (now_unix as usize) ^ if is_signal { 0x5160 } else { 0 }) % candidate_systems.len();
    let dest_sys_id = candidate_systems[idx];
    let dest_sys = ctx.sde.solar_systems.get(&dest_sys_id).ok_or_else(|| {
        BridgeRpcError { code: "NoNullsecSystemInSde".into(), message: "Destination nullsec solar system not found in SDE".into() }
    })?;

    let landing_base = if !dest_sys.planet_ids.is_empty() {
        let p_idx = ((now_unix as usize) ^ (char_id as usize)) % dest_sys.planet_ids.len();
        let pid = dest_sys.planet_ids[p_idx];
        ctx.sde.celestials_in_system(dest_sys_id).into_iter().find(|c| c.celestial_id == pid)
            .map(|c| c.position + DVec3::new(c.radius.max(50_000.0) + 1_000_000.0, 0.0, 0.0))
    } else {
        None
    }.unwrap_or(DVec3::new(100_000_000.0, 0.0, 0.0));

    ctx.validate_ship_movement(
        char_id,
        ship_id,
        crate::services::space_nav_movement_guard::MovementIntent::RandomFilament {
            from_sys: old_sys,
        },
    )
    .await?;
    let mut jumpers = vec![(char_id, ship_id, current_ball)];
    for mate_id in fleet_members {
        if mate_id == char_id || jumpers.len() >= max_capacity { continue; }
        let mate_ship_info = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&mate_id).filter(|ch| ch.solar_system_id == old_sys && ch.station_id.is_none() && ch.structure_id.is_none())
                .map(|ch| (ch.active_ship_id, snap.items.get(&ch.active_ship_id).map(|i| i.type_id).unwrap_or(0)))
        });
        let Some((m_ship_id, m_type_id)) = mate_ship_info else { continue; };
        let m_grp = ctx.sde.types.get(&m_type_id).map(|t| t.group_id).unwrap_or(0);
        if matches!(m_grp, 29 | 30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594) { continue; }
        if matches!(dest_sys_id, 30_001_372 | 30_003_504 | 30_002_702) {
            let m_trig = ctx.db.with_snapshot(|snap| snap.characters.get(&mate_id).and_then(|c| c.standings.get(&500_026).copied()).unwrap_or(0.0));
            if m_trig < 6.0 { continue; }
        }
        if crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, mate_id, m_ship_id, old_sys).is_err() { continue; }
        if crate::services::space_destiny::check_session_change_timer_active(ctx, mate_id, now_unix, Some((old_sys, cur_tick))).is_some() { continue; }
        if old_handle.is_ball_scrambled(m_ship_id).await.unwrap_or(false) { continue; }
        if old_handle.get_combat_state(m_ship_id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) { continue; }
        if let Ok(Some((_, mut m_ball))) = old_handle.get_ball_snapshot(m_ship_id).await {
            if matches!(m_ball.mode, MovementMode::Warp { .. }) { continue; }
            if crate::services::tactical_modules::prepare_ship_for_wormhole_jump(ctx, &old_handle, mate_id, m_ship_id, &mut m_ball).await.is_err() { continue; }
            if caller_pos.distance(m_ball.position) <= 6_000.0 {
                jumpers.push((mate_id, m_ship_id, m_ball));
            }
        }
    }

    for &(p_char, p_ship, _) in &jumpers {
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, p_char, p_ship, old_sys);
        crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, p_ship);
        crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, p_char, p_ship);
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(fil_owner) = snap.items.get(&fil_item_id).map(|i| i.owner_id) {
            snap.consume_exact_item_quantity(
                fil_item_id,
                1,
                eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), fil_owner),
            )
            .map_err(|e| BridgeRpcError {
                code: "FilamentLocked".into(),
                message: e.to_string(),
            })?;
        }
        for &(p_char, _p_ship, _) in &jumpers {
            let _ = crate::router::apply_character_transition_in_snap(
                snap,
                &ctx.sde,
                p_char,
                crate::router::TransitionLocation::Space { solar_system_id: dest_sys_id },
                None,
                true,
                false,
            );
        }
        Ok::<(), BridgeRpcError>(())
    })?;
    let (old_const, old_reg) = ctx.sde.solar_systems.get(&old_sys).map(|s| (s.constellation_id, s.region_id)).unwrap_or((0, 0));
    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let pvp_until = now_unix.saturating_add(crate::sp6_lifecycle::PVP_TIMER_SECS);
    for (j_idx, (p_char, p_ship, mut b)) in jumpers.into_iter().enumerate() {
        let prev_cs = old_handle.get_combat_state(p_ship).await.ok().flatten();
        let _ = old_handle.remove_ball(p_ship).await;
        ctx.sync_character_local_chat(p_char, Some(old_sys), dest_sys_id);
        crate::services::map_service::record_solar_system_jump(ctx, p_char, dest_sys_id);

        b.position = landing_base + DVec3::new((j_idx as f64) * 200.0, 0.0, 0.0);
        let land_arr = b.position.to_array();
        b.velocity = DVec3::ZERO;
        b.speed_fraction = 0.0;
        b.mode = MovementMode::Stop;
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(p_ship, land_arr);
        });
        let mut combat_state = ctx.build_ship_combat_state(p_char, p_ship, b.type_id);
        crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(&mut combat_state, prev_cs);
        combat_state.is_cloaked = true;
        combat_state.gate_cloak_remaining_ticks = 60;
        combat_state.undock_invulnerable_remaining_ticks = 60;
        combat_state.sync_invuln_with_undock_timer = true;
        let mut slim_meta = ctx.build_ship_slim_meta(p_char, b.type_id);
        slim_meta.is_cloaked = true;
        let dest_tick = new_handle
            .spawn_ball_with_meta(b, combat_state, slim_meta)
            .await
            .unwrap_or(0);
        let _ = new_handle.push_set_state(p_ship).await;
        crate::services::space_destiny::record_session_change_timer(
            ctx,
            p_char,
            now_unix,
            dest_sys_id,
            dest_tick,
            "filamentJump",
        );
        {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let pilot_cw = cw.get_or_create(p_char);
            pilot_cw.pvp_timer_until_unix = pilot_cw.pvp_timer_until_unix.max(pvp_until);
        }
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
            st.pvp_timers_until.insert(p_char, pvp_until);
        });
        crate::services::combat_crimewatch::schedule_highsec_criminal_concord_if_needed(
            ctx,
            p_char,
            p_ship,
            dest_sys_id,
            now_unix,
        );
        crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(ctx, p_char, now_unix);

        if p_char != char_id {
            let sess_rep = PyRep::keyval([
                ("charid", PyRep::Integer(p_char)),
                ("stationid", PyRep::None),
                ("structureid", PyRep::None),
                ("solarsystemid2", PyRep::Integer(dest_sys_id as i64)),
                ("solarsystemid", PyRep::Integer(dest_sys_id as i64)),
                ("constellationid", PyRep::Integer(dest_sys.constellation_id as i64)),
                ("regionid", PyRep::Integer(dest_sys.region_id as i64)),
            ]);
            let change_dict = PyRep::Dict(vec![
                (PyRep::String("solarsystemid".into()), PyRep::Tuple(vec![PyRep::Integer(old_sys as i64), PyRep::Integer(dest_sys_id as i64)])),
                (PyRep::String("solarsystemid2".into()), PyRep::Tuple(vec![PyRep::Integer(old_sys as i64), PyRep::Integer(dest_sys_id as i64)])),
                (PyRep::String("constellationid".into()), PyRep::Tuple(vec![PyRep::Integer(old_const as i64), PyRep::Integer(dest_sys.constellation_id as i64)])),
                (PyRep::String("regionid".into()), PyRep::Tuple(vec![PyRep::Integer(old_reg as i64), PyRep::Integer(dest_sys.region_id as i64)])),
            ]);
            ctx.notify(crate::router::NotificationTarget::Character(p_char), "OnSessionChanged", "charid", vec![PyRep::Bool(false), sess_rep, change_dict]);
        }
    }

    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::TransitionKind::JumpSystem { solar_system_id: dest_sys_id },
            "filament_jump",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
    ]))
}

pub(crate) fn validate_structure_cyno_beacon(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    target_struct_id: i64,
    target_sys_hint: Option<u32>,
) -> Result<eve_db::types::UpwellStructureRecord, BridgeRpcError> {
    let st = snap.structures.get(&target_struct_id).cloned().ok_or_else(|| BridgeRpcError {
        code: "BeaconNotFound".into(),
        message: format!("Beacon structure {target_struct_id} not found"),
    })?;
    if st.type_id != 35840 {
        return Err(BridgeRpcError { code: "InvalidStructureBeacon".into(), message: format!("Structure {target_struct_id} (type {}) is not a Pharolux Cyno Beacon", st.type_id) });
    }
    if st.state == "Destroyed" {
        return Err(BridgeRpcError { code: "BeaconDestroyed".into(), message: format!("Beacon structure {target_struct_id} is destroyed") });
    }
    if st.fuel_blocks <= 0 {
        return Err(BridgeRpcError { code: "BeaconOutOfFuel".into(), message: format!("Beacon structure {target_struct_id} has no fuel blocks") });
    }
    if !st.quantum_core_installed || matches!(st.state.as_str(), "Unanchored" | "Anchored" | "AnchorVulnerable" | "Anchoring" | "NoCore" | "Onlining") {
        return Err(BridgeRpcError { code: "StructureBeaconOffline".into(), message: format!("Beacon structure {target_struct_id} is offline (state '{}')", st.state) });
    }
    if !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
        snap,
        target_struct_id,
        char_id,
    ) {
        return Err(BridgeRpcError {
            code: "BeaconAccessDenied".into(),
            message: format!("Character {char_id} is denied access to beacon structure {target_struct_id}"),
        });
    }
    if let Some(hint_sys) = target_sys_hint {
        if st.solar_system_id != hint_sys {
            return Err(BridgeRpcError {
                code: "BeaconWrongSystem".into(),
                message: format!("Beacon {target_struct_id} is in system {}, expected {hint_sys}", st.solar_system_id),
            });
        }
    }
    Ok(st)
}

fn handle_get_beacon_structures(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let beacons: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .filter(|st| validate_structure_cyno_beacon(snap, char_id, st.structure_id, None).is_ok())
            .map(|st| {
                PyRep::keyval([
                    ("structureID", PyRep::Integer(st.structure_id)),
                    ("solarSystemID", PyRep::Integer(st.solar_system_id as i64)),
                    ("fuelBlocks", PyRep::Integer(st.fuel_blocks as i64)),
                ])
            })
            .collect()
    });
    Ok(PyRep::List(beacons))
}

async fn handle_cmd_jump_to_structure_beacon(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship".into(),
    })?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump to cyno beacon while docked".into(),
        });
    }
    let cur_sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solarSystemID".into(),
    })?;
    let (target_sys_hint, target_struct_id) = if args.len() >= 2 {
        (
            args.first().and_then(|a| a.as_i64()).map(|s| s as u32),
            args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
        )
    } else {
        (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
    };
    let cur_sys_handle = ctx.systems.get_or_spawn(cur_sys_id);
    let validated_jump = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::CynoJump {
                from_sys: cur_sys_id,
                to_sys: cur_sys_id,
                allow_bubbled: false,
            },
        )
        .await?;
    let current_ball = validated_jump.ball.ok_or_else(|| BridgeRpcError {
        code: "ShipNotInSpace".into(),
        message: "Active ship is not spawned in space".into(),
    })?;
    if validated_jump
        .combat_state
        .as_ref()
        .is_some_and(|cs| cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
    {
        return Err(BridgeRpcError {
            code: "CannotJumpWhileCloaked".into(),
            message: "Cannot jump while cloaked".into(),
        });
    }
    let now_check = ctx.now_unix();
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx, char_id, ship_id, cur_sys_id, "jump_drive", now_check,
    )?;

    let target_struct = ctx
        .db
        .with_snapshot(|snap| validate_structure_cyno_beacon(snap, char_id, target_struct_id, target_sys_hint))?;
    let target_sys_id = target_struct.solar_system_id;
    crate::services::space_nav_movement_guard::validate_pre_movement_locks(
        ctx,
        ship_id,
        &crate::services::space_nav_movement_guard::MovementIntent::CynoJump {
            from_sys: cur_sys_id,
            to_sys: target_sys_id,
            allow_bubbled: false,
        },
    )?;
    if (32_000_000..=32_999_999).contains(&cur_sys_id)
        || (32_000_000..=32_999_999).contains(&target_sys_id)
    {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInAbyssalSpace".into(), message: "Jump drive and cynosural beacon operations are prohibited in Abyssal Deadspace".into() });
    }
    if ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(BridgeRpcError { code: "CannotJumpToPochven".into(), message: format!("Region-wide cyno jammer prevents jump drives from entering Pochven system {target_sys_id}") });
    }
    if crate::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(ctx, target_sys_id).is_some() {
        return Err(BridgeRpcError { code: "CynoJammed".into(), message: format!("Cynosural jammer in system {target_sys_id} blocks jumping to structure beacon {target_struct_id}") });
    }
    // Jump-drive range, isotope fuel, high-sec restriction and fatigue are SDE-driven
    // (`feat:navigation.jump-drive`, `feat:navigation.jump-fatigue`, `feat:navigation.jump-range-restrictions`).
    use super::space_nav_travel as nav;
    let db_type_id = ctx.db.get_item(ship_id).map(|i| i.type_id).unwrap_or(current_ball.type_id);
    let plan_res = match nav::plan_jump_drive(ctx, char_id, db_type_id, cur_sys_id, target_sys_id) {
        Ok(p) => Ok(p),
        Err(_) if current_ball.type_id != 0 && current_ball.type_id != db_type_id => {
            nav::plan_jump_drive(ctx, char_id, current_ball.type_id, cur_sys_id, target_sys_id)
        }
        Err(e) => Err(e),
    };
    let target_sec = ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if target_sec >= 0.45 {
        if let Err(ref e) = plan_res {
            if e.code == "JumpDriveHighSecForbidden" {
                return Err(e.clone());
            }
        }
        return Err(BridgeRpcError { code: "CannotJumpToHighSecBeacon".into(), message: format!("Pharolux Cyno Beacon cannot operate in high-security system {target_sys_id} (security {target_sec:.2} >= 0.45)") });
    }
    let plan = plan_res?;

    let now_unix = ctx.now_unix();
    if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Err(BridgeRpcError { code: "SessionChangeTimerActive".into(), message: "Session change timer is active".into() });
    }

    nav::check_jump_activation(ctx, char_id)?;
    let cap_ship_type = if nav::type_attr(ctx, db_type_id, "jumpDriveCapacitorNeed").is_some() { db_type_id } else { current_ball.type_id };
    let updated_cap_cs = nav::validate_and_prepare_jump_capacitor(ctx, char_id, ship_id, cur_sys_id, cap_ship_type).await?;
    nav::consume_jump_fuel(ctx, ship_id, &plan)?;
    if let Some(ref cap_cs) = updated_cap_cs {
        let _ = cur_sys_handle.set_combat_state(ship_id, cap_cs.clone()).await;
    }
    let dist_ly = plan.distance_ly;
    let (cd_min, fat_min) =
        nav::apply_jump_fatigue(ctx, char_id, plan.distance_ly);
    let prev_cs = cur_sys_handle.get_combat_state(ship_id).await.ok().flatten();
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, cur_sys_id);
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let _ = cur_sys_handle.remove_ball(ship_id).await;
    let (dest_const_id, dest_reg_id) = ctx
        .sde
        .solar_systems
        .get(&target_sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        let _ = crate::router::apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            char_id,
            crate::router::TransitionLocation::Space { solar_system_id: target_sys_id },
            Some(ship_id),
            true,
            false,
        );
    });
    let target_pos = DVec3::from_array(target_struct.position) + DVec3::new(5_000.0, 0.0, 0.0);
    let dest_handle = ctx.systems.get_or_spawn(target_sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: current_ball.type_id,
        position: target_pos,
        velocity: DVec3::ZERO,
        max_velocity: current_ball.max_velocity,
        inertia: current_ball.inertia,
        mass: current_ball.mass,
        radius: current_ball.radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, current_ball.type_id);
    crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(&mut combat_state, prev_cs);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, current_ball.type_id);
    slim_meta.is_cloaked = true;
    let dest_tick = dest_handle.spawn_ball_with_meta(ball, combat_state, slim_meta).await.unwrap_or(0);
    let now_unix = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(
        ctx, char_id, now_unix, target_sys_id, dest_tick, "jump_drive",
    );
    ctx.sync_character_local_chat(char_id, Some(cur_sys_id), target_sys_id);
    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::TransitionKind::JumpSystem { solar_system_id: target_sys_id },
            "cyno_jump",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );
    let _ = (dest_const_id, dest_reg_id);
    crate::services::map_service::record_solar_system_jump(ctx, char_id, target_sys_id);
    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, target_sys_id as i64)?;

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(target_sys_id as i64)),
        ("structureID", PyRep::Integer(target_struct_id)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
        ("jumpFatigueMinutes", PyRep::Real(fat_min)),
        ("jumpCooldownMinutes", PyRep::Real(cd_min)),
        ("distanceLY", PyRep::Real(dist_ly)),
    ]))
}

fn parse_structure_beacon_args(args: &[PyRep]) -> (Option<u32>, i64) {
    if args.len() >= 2 {
        (args.first().and_then(|a| a.as_i64()).map(|s| s as u32), args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
    } else {
        (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
    }
}

fn build_structure_beacon_record(
    ctx: &ServerContext,
    target_struct: &eve_db::types::UpwellStructureRecord,
) -> crate::services::inventory_fitting::fitting_cyno_portal::FleetModuleBeacon {
    crate::services::inventory_fitting::fitting_cyno_portal::FleetModuleBeacon {
        beacon_id: target_struct.structure_id,
        char_id: target_struct.owner_corp_id,
        ship_id: target_struct.structure_id,
        beacon_type_id: target_struct.type_id,
        solar_system_id: target_struct.solar_system_id,
        position: DVec3::from_array(target_struct.position),
        expires_at_unix: ctx.now_unix().saturating_add(300),
    }
}

async fn validate_pharolux_beacon_target(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    allow_bubbled: bool,
) -> Result<(u32, eve_db::types::UpwellStructureRecord, i64), BridgeRpcError> {
    use crate::services::inventory_fitting::fitting_cyno_portal as portal;
    let (char_id, _, cur_sys_id, _) = portal::caller_in_space(ctx, session, allow_bubbled).await?;
    let (target_sys_hint, target_struct_id) = parse_structure_beacon_args(args);
    let target_struct = ctx
        .db
        .with_snapshot(|snap| validate_structure_cyno_beacon(snap, char_id, target_struct_id, target_sys_hint))?;
    let target_sys_id = target_struct.solar_system_id;
    if (31_000_000..32_000_000).contains(&cur_sys_id)
        || (31_000_000..32_000_000).contains(&target_sys_id)
    {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInWormholeSpace".into(), message: "Jump drive and cynosural beacon operations are prohibited in wormhole space".into() });
    }
    if (32_000_000..=32_999_999).contains(&cur_sys_id)
        || (32_000_000..=32_999_999).contains(&target_sys_id)
    {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInAbyssalSpace".into(), message: "Jump drive and cynosural beacon operations are prohibited in Abyssal Deadspace".into() });
    }
    if cur_sys_id == 30_100_000 || target_sys_id == 30_100_000 {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInZarzakh".into(), message: "Jump drive and cynosural beacon operations are prohibited in Zarzakh".into() });
    }
    if ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(BridgeRpcError { code: "CannotJumpToPochven".into(), message: format!("Region-wide cyno jammer prevents jump drives from entering Pochven system {target_sys_id}") });
    }
    let target_sec = ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if target_sec >= 0.45 {
        return Err(BridgeRpcError { code: "CannotJumpToHighSecBeacon".into(), message: format!("Pharolux Cyno Beacon cannot operate in high-security system {target_sys_id}") });
    }
    if portal::active_cyno_jammer(ctx, target_sys_id).is_some() {
        return Err(BridgeRpcError { code: "CynoJammed".into(), message: format!("Cynosural jammer in system {target_sys_id} blocks bridging or conduit jumping to structure beacon {target_struct_id}") });
    }
    Ok((cur_sys_id, target_struct, target_struct_id))
}

async fn handle_cmd_bridge_to_structure_beacon(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use crate::services::inventory_fitting::fitting_cyno_portal as portal;
    let (_, target_struct, target_struct_id) = validate_pharolux_beacon_target(ctx, session, args, true).await?;
    let beacon = build_structure_beacon_record(ctx, &target_struct);
    portal::bridge_to_resolved_beacon(ctx, session, beacon, Some(target_struct_id)).await
}

async fn handle_cmd_group_jump_to_structure_beacon(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    use crate::services::inventory_fitting::fitting_cyno_portal as portal;
    let (cur_sys_id, target_struct, target_struct_id) = validate_pharolux_beacon_target(ctx, session, args, false).await?;
    let dest_sys_id = target_struct.solar_system_id;
    if dest_sys_id == cur_sys_id {
        return Err(BridgeRpcError { code: "InvalidJumpDestination".into(), message: format!("Already in beacon system {dest_sys_id}") });
    }
    let beacon = build_structure_beacon_record(ctx, &target_struct);
    let (outcome, jumped_count) = portal::group_jump_to_resolved_beacon(ctx, session, beacon, false).await?;
    Ok(PyRep::keyval([
        ("groupJumpActive", PyRep::Bool(true)),
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("structureID", PyRep::Integer(target_struct_id)),
        ("jumpedCount", PyRep::Integer(jumped_count as i64)),
        ("beyonceBoundID", PyRep::Integer(outcome.beyonce_bound_id as i64)),
        ("distanceLY", PyRep::Real(outcome.dist_ly)),
        ("fuelTypeID", PyRep::Integer(i64::from(outcome.fuel_type))),
        ("fuelQuantity", PyRep::Integer(outcome.fuel_qty)),
        ("jumpFatigueMinutes", PyRep::Real(outcome.fatigue_min)),
        ("jumpCooldownMinutes", PyRep::Real(outcome.cooldown_min)),
    ]))
}

async fn handle_cone_scan(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if method == "DirectionalScan" {
        return None;
    }
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Some(Err(BridgeRpcError {
            code: "CannotDirectionalScanWhileDocked".into(),
            message: "Directional scanner cannot be used while docked".into(),
        }));
    }
    let Some(sys_id) = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
    else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        }));
    };
    if sys_id == 30_100_000 || (32_000_000..=32_999_999).contains(&sys_id) {
        return Some(Err(BridgeRpcError::user_error("CannotDscanInThisSystem")));
    }
    let raw_angle = args
        .first()
        .and_then(|a| a.as_f64())
        .unwrap_or(360.0);
    // V24 client (`directionalScanSvc.pyj`) passes `scanAngle` in radians (`0.087..=2*pi`),
    // whereas internal tests pass degrees (`5.0..=360.0`).
    let scan_angle = if raw_angle > 0.0
        && (raw_angle - 5.0).abs() > 1e-6
        && raw_angle <= std::f64::consts::TAU + 0.001
    {
        raw_angle.to_degrees().clamp(5.0, 360.0)
    } else {
        raw_angle.clamp(5.0, 360.0)
    };
    let max_scan_range = 14.3 * AU_IN_METERS;
    let scan_range = args
        .get(1)
        .and_then(|a| a.as_f64())
        .unwrap_or(max_scan_range)
        .clamp(0.0, max_scan_range);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let caller_ship = session.ship_id;
    let caller_ball = if let Some(sid) = caller_ship {
        sys_handle
            .get_ball_snapshot(sid)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b)
    } else {
        None
    };
    let caller_pos = caller_ball
        .as_ref()
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);

    let explicit_dir = if let (Some(dx), Some(dy), Some(dz)) = (
        args.get(2).and_then(|a| a.as_f64()),
        args.get(3).and_then(|a| a.as_f64()),
        args.get(4).and_then(|a| a.as_f64()),
    ) {
        Some(DVec3::new(dx, dy, dz))
    } else if let Some(PyRep::Tuple(coords) | PyRep::List(coords)) = args.get(2) {
        if coords.len() >= 3 {
            Some(DVec3::new(
                coords[0].as_f64().unwrap_or(0.0),
                coords[1].as_f64().unwrap_or(0.0),
                coords[2].as_f64().unwrap_or(1.0),
            ))
        } else {
            None
        }
    } else {
        None
    };

    let caller_dir = explicit_dir
        .filter(|d| d.length_squared() > 1e-9)
        .map(|d| d.normalize())
        .or_else(|| {
            caller_ball.as_ref().map(|b| {
                if b.velocity.length_squared() > 0.001 {
                    b.velocity.normalize()
                } else {
                    DVec3::new(0.0, 0.0, 1.0)
                }
            })
        })
        .unwrap_or(DVec3::new(0.0, 0.0, 1.0));

    crate::services::mobile_deployables_ext::sync_system_deployables(ctx, sys_id).await;
    let scan_inhibitors = crate::services::mobile_deployables_ext::active_scan_inhibitor_positions(ctx, sys_id);
    if caller_ship.is_some()
        && crate::services::mobile_deployables_ext::is_pos_scan_inhibited(&scan_inhibitors, caller_pos)
    {
        return Some(Ok(PyRep::List(Vec::new())));
    }
    let cos_half_angle = (scan_angle.to_radians() * 0.5).cos();
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let inhibitor_positions = active_scan_inhibitor_positions(ctx, sys_id, &balls);
    // DERIVED: 30_000.0 m Mobile Scan Inhibitor (`33_589`, group `1275`) cloak sphere radius;
    // ships inside an active inhibitor sphere have inoperable sensors (`Mobile_structures.md:92`).
    if inhibitor_positions.iter().any(|pos| caller_pos.distance(*pos) <= 30_000.0) {
        return Some(Ok(PyRep::List(Vec::new())));
    }
    let all_ball_ids: std::collections::HashSet<i64> = balls.iter().map(|b| b.id).collect();
    let mut matches: Vec<PyRep> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for ball in balls {
        if Some(ball.id) == caller_ship {
            continue;
        }
        let is_ship_or_drone = ctx
            .sde
            .types
            .get(&ball.type_id)
            .is_some_and(|t| matches!(t.category_id, 6 | 18 | 87));
        if is_ship_or_drone
            && inhibitor_positions
                .iter()
                .any(|pos| ball.position.distance(*pos) <= 30_000.0)
        {
            continue;
        }
        let dist = ball.position.distance(caller_pos);
        if dist > scan_range {
            continue;
        }
        if scan_angle < 360.0 {
            let to_target = (ball.position - caller_pos).normalize_or_zero();
            if to_target.dot(caller_dir) < cos_half_angle {
                continue;
            }
        }
        let (group_id, cat_id) = ctx
            .sde
            .types
            .get(&ball.type_id)
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((0, 0));
        if group_id == 906 {
            continue;
        }
        if crate::services::inventory_fitting::fitting_cyno_portal::is_covert_beacon_type(ball.type_id) {
            let caller_bub = if let Some(sid) = session.ship_id {
                sys_handle.get_bubble_id_for_ball(sid).await.ok().flatten()
            } else {
                None
            };
            let b_bub = sys_handle.get_bubble_id_for_ball(ball.id).await.ok().flatten();
            if caller_bub.is_none() || b_bub != caller_bub {
                continue;
            }
        }
        if group_id != 1275
            && matches!(cat_id, 6 | 18 | 87)
            && crate::services::mobile_deployables_ext::is_pos_scan_inhibited(&scan_inhibitors, ball.position)
        {
            continue;
        }
        if let Ok(Some(cs)) = sys_handle.get_combat_state(ball.id).await {
            if cs.is_cloaked {
                continue;
            }
        }
        seen_ids.insert(ball.id);
        matches.push(PyRep::keyval([
            ("id", PyRep::Integer(ball.id)),
            ("itemID", PyRep::Integer(ball.id)),
            ("ballID", PyRep::Integer(ball.id)),
            ("typeID", PyRep::Integer(ball.type_id as i64)),
            ("groupID", PyRep::Integer(group_id as i64)),
            ("distance", PyRep::Real(dist)),
            ("x", PyRep::Real(ball.position.x)),
            ("y", PyRep::Real(ball.position.y)),
            ("z", PyRep::Real(ball.position.z)),
        ]));
    }

    let convoys = ctx.living_world.list_convoys();
    for c in convoys
        .into_iter()
        .filter(|c| c.current_system_id == sys_id && !c.destroyed)
    {
        if !seen_ids.contains(&c.convoy_id)
            && !all_ball_ids.contains(&c.convoy_id)
            && !ctx.living_world.is_convoy_docked(c.convoy_id)
        {
            let station_pos = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .and_then(|sys| {
                    sys.stations
                        .iter()
                        .find(|st| st.station_id == c.destination_station_id)
                        .or_else(|| sys.stations.first())
                        .map(|st| st.position)
                })
                .unwrap_or(DVec3::ZERO);
            let convoy_pos = station_pos + DVec3::new(3_000.0, 0.0, 0.0);
            let dist = convoy_pos.distance(caller_pos);
            if dist > scan_range {
                continue;
            }
            if scan_angle < 360.0 {
                let to_target = (convoy_pos - caller_pos).normalize_or_zero();
                if to_target.dot(caller_dir) < cos_half_angle {
                    continue;
                }
            }
            let group_id = ctx.sde.types.get(&c.ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if group_id == 906 { continue; }
            matches.push(PyRep::keyval([
                ("id", PyRep::Integer(c.convoy_id)), ("itemID", PyRep::Integer(c.convoy_id)), ("ballID", PyRep::Integer(c.convoy_id)),
                ("typeID", PyRep::Integer(c.ship_type_id as i64)), ("groupID", PyRep::Integer(group_id as i64)), ("distance", PyRep::Real(dist)),
                ("x", PyRep::Real(convoy_pos.x)), ("y", PyRep::Real(convoy_pos.y)), ("z", PyRep::Real(convoy_pos.z)),
            ]));
        }
    }
    Some(Ok(PyRep::List(matches)))
}

async fn handle_cmd_beacon_jump_fleet(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError { code: "NoActiveShip".into(), message: "Session has no active ship".into() })?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError { code: "ShipDocked".into(), message: "Cannot jump while docked".into() });
    }
    let cur_sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solarSystemID".into() })?;

    let cur_sys_handle = ctx.systems.get_or_spawn(cur_sys_id);
    let (_, current_ball) = cur_sys_handle.get_ball_snapshot(ship_id).await
        .map_err(|e| BridgeRpcError { code: "DestinyCommandFailed".into(), message: e })?
        .ok_or_else(|| BridgeRpcError { code: "ShipNotInSpace".into(), message: "Active ship is not spawned in space".into() })?;

    if cur_sys_handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
    {
        return Err(BridgeRpcError { code: "CannotJumpWhileCloaked".into(), message: "Cannot jump while cloaked".into() });
    }
    if matches!(current_ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError { code: "ShipInWarp".into(), message: "Cannot jump while in warp".into() });
    }
    if cur_sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError { code: "WarpScrambled".into(), message: "Cannot jump while warp scrambled".into() });
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "jump")?;
    let now_check = ctx.now_unix();
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(ctx, char_id, ship_id, cur_sys_id, "jump_drive", now_check)?;

    let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let _target_ship_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    let target_sys_id = args.get(2).and_then(|a| a.as_i64()).map(|s| s as u32).unwrap_or(0);

    let same_fleet = ctx.db.with_snapshot(|snap| {
        let f_a = snap.fleets.values().find(|f| f.members.contains(&char_id)).map(|f| f.fleet_id);
        let f_b = snap.fleets.values().find(|f| f.members.contains(&target_char_id)).map(|f| f.fleet_id);
        f_a.is_some() && f_a == f_b
    });
    if !same_fleet {
        return Err(BridgeRpcError { code: "FleetNotInFleet".into(), message: format!("Target character {target_char_id} is not in caller's fleet") });
    }
    if (31_000_000..32_000_000).contains(&cur_sys_id) || (31_000_000..32_000_000).contains(&target_sys_id) {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInWormholeSpace".into(), message: "Jump drive operations are prohibited in wormhole space".into() });
    }
    if (32_000_000..=32_999_999).contains(&cur_sys_id) || (32_000_000..=32_999_999).contains(&target_sys_id) {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInAbyssalSpace".into(), message: "Jump drive operations are prohibited in Abyssal Deadspace".into() });
    }
    if cur_sys_id == 30_100_000 || target_sys_id == 30_100_000 {
        return Err(BridgeRpcError { code: "JumpDriveForbiddenInZarzakh".into(), message: "Jump drive operations are prohibited in Zarzakh".into() });
    }
    if ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(BridgeRpcError { code: "CannotJumpToPochven".into(), message: format!("Region-wide cyno jammer prevents jump drives from entering Pochven system {target_sys_id}") });
    }
    let target_sec = ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if target_sec >= 0.45 {
        return Err(BridgeRpcError { code: "CannotJumpToHighSecBeacon".into(), message: format!("Cynosural beacon cannot operate in high-security system {target_sys_id}") });
    }

    let beacon = crate::services::inventory_fitting::fitting_cyno_portal::live_beacon(ctx, target_char_id, 0, target_sys_id)
        .map_err(|e| BridgeRpcError { code: "NoActiveBeacon".into(), message: e.message })?;
    crate::services::inventory_fitting::fitting_cyno_portal::check_cyno_jammer_for_beacon(ctx, &beacon)?;

    use super::space_nav_travel as nav;
    let db_type_id = ctx.db.get_item(ship_id).map(|i| i.type_id).unwrap_or(current_ball.type_id);
    let plan = nav::plan_jump_drive(ctx, char_id, db_type_id, cur_sys_id, target_sys_id)?;

    let now_unix = ctx.now_unix();
    if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Err(BridgeRpcError { code: "SessionChangeTimerActive".into(), message: "Session change timer is active".into() });
    }

    nav::check_jump_activation(ctx, char_id)?;
    let updated_cap_cs = nav::validate_and_prepare_jump_capacitor(ctx, char_id, ship_id, cur_sys_id, db_type_id).await?;
    nav::consume_jump_fuel(ctx, ship_id, &plan)?;
    if let Some(ref cap_cs) = updated_cap_cs {
        let _ = cur_sys_handle.set_combat_state(ship_id, cap_cs.clone()).await;
    }
    let dist_ly = plan.distance_ly;
    let (cd_min, fat_min) = nav::apply_jump_fatigue(ctx, char_id, plan.distance_ly);
    let prev_cs = cur_sys_handle.get_combat_state(ship_id).await.ok().flatten();
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, cur_sys_id);
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let _ = cur_sys_handle.remove_ball(ship_id).await;
    let (dest_const_id, dest_reg_id) = ctx.sde.solar_systems.get(&target_sys_id).map(|s| (s.constellation_id, s.region_id)).unwrap_or((0, 0));

    let _ = (dest_const_id, dest_reg_id);
    ctx.db.with_mut_snapshot(|snap| {
        let _ = crate::router::apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            char_id,
            crate::router::TransitionLocation::Space { solar_system_id: target_sys_id },
            Some(ship_id),
            true,
            false,
        );
    });
    let target_pos = beacon.position + DVec3::new(2_500.0, 0.0, 0.0);
    let dest_handle = ctx.systems.get_or_spawn(target_sys_id);
    let ball = Ball {
        id: ship_id, type_id: current_ball.type_id, position: target_pos, velocity: DVec3::ZERO,
        max_velocity: current_ball.max_velocity, inertia: current_ball.inertia, mass: current_ball.mass,
        radius: current_ball.radius, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, current_ball.type_id);
    crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(&mut combat_state, prev_cs);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, current_ball.type_id);
    slim_meta.is_cloaked = true;
    let dest_tick = dest_handle.spawn_ball_with_meta(ball, combat_state, slim_meta).await.unwrap_or(0);
    let now_unix = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(ctx, char_id, now_unix, target_sys_id, dest_tick, "jump_drive");
    ctx.sync_character_local_chat(char_id, Some(cur_sys_id), target_sys_id);
    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            char_id,
            crate::router::TransitionKind::JumpSystem { solar_system_id: target_sys_id },
            "cyno_jump",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );
    crate::services::map_service::record_solar_system_jump(ctx, char_id, target_sys_id);
    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, target_sys_id as i64)?;

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(target_sys_id as i64)), ("beaconID", PyRep::Integer(beacon.beacon_id)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)), ("jumpFatigueMinutes", PyRep::Real(fat_min)),
        ("jumpCooldownMinutes", PyRep::Real(cd_min)), ("distanceLY", PyRep::Real(dist_ly)),
        ("fuelConsumed", PyRep::Integer(plan.fuel_units as i64)), ("fuelTypeID", PyRep::Integer(plan.fuel_type_id as i64)),
    ]))
}

/// Returns positions of anchored, warmed-up (`60`s activation delay elapsed), non-expired
/// Mobile Scan Inhibitors (`33_589`, group `1275`) in `sys_id`.
pub fn active_scan_inhibitor_positions(
    ctx: &ServerContext,
    sys_id: u32,
    balls: &[Ball],
) -> Vec<DVec3> {
    let now_unix = ctx.now_unix();
    let dep_state = crate::services::mobile_deployables::read_deployable_state(ctx);
    balls
        .iter()
        .filter(|b| {
            b.type_id == 33_589
                && dep_state.deployables.get(&b.id).is_some_and(|rec| {
                    rec.solar_system_id == sys_id
                        && rec.group_id == crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR
                        && !rec.is_unanchored
                        && now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs)
                        && now_unix < rec.expires_at_unix
                })
        })
        .map(|b| b.position)
        .collect()
}

#[cfg(test)]
#[path = "space_cyno_filaments_tests.rs"]
mod space_cyno_filaments_tests;

