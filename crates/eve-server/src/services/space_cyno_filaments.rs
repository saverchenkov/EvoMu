//! `space_cyno_filaments` — Wormhole jumps, Abyssal/Void Filaments, Pharolux Cyno Beacons, D-Scan, and Mining (`S-3`, `SP-4`, `MN`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;

use super::{execute_cosmic_wormhole_jump, AU_IN_METERS};

const BEYONCE_SVC: &str = "beyonce";
const ABYSSAL_SVC: &str = "abyssalMgr";
const VOID_SVC: &str = "voidSpaceMgr";
const BEACON_SVC: &str = "structureCynoBeaconMgr";
const DRONE_SVC: &str = "droneMgr";
const MINING_SVC: &str = "miningMgr";

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

        // parity: feat:navigation.filament-abyssal, feat:navigation.filament-void
        // parity: rpc:abyssalMgr.AbyssalEntranceDeployment, rpc:abyssalMgr.AbyssalEntranceGateActivation, rpc:voidSpaceMgr.VoidSpacePlayerJump
        ("abyssalMgr", "AbyssalEntranceDeployment")
        | ("abyssalMgr", "AbyssalEntranceGateActivation")
        | ("voidSpaceMgr", "VoidSpacePlayerJump") => {
            Some(handle_filament_deployment(ctx, session).await)
        }

        // parity: feat:navigation.filament-needlejack
        // parity: rpc:randomJumpMgr.ActivateRandomJumpFilament
        ("randomJumpMgr", "ActivateRandomJumpFilament") => {
            Some(handle_random_jump_filament(ctx, session).await)
        }

        // parity: plan:S-3, feat:navigation.pharolux-beacon
        // parity: rpc:structureCynoBeaconMgr.GetMyAccessibleOnlineCynoBeaconStructures, rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon, rpc:structureCynoBeaconMgr.CmdBridgeToStructureBeacon, rpc:structureCynoBeaconMgr.CmdGroupJumpToStructureBeacon
        ("structureCynoBeaconMgr", "GetMyAccessibleOnlineCynoBeaconStructures") => {
            Some(handle_get_beacon_structures(ctx, session))
        }
        ("structureCynoBeaconMgr", "CmdJumpToStructureBeacon") => {
            Some(handle_cmd_jump_to_structure_beacon(ctx, session, args).await)
        }
        ("structureCynoBeaconMgr", "CmdBridgeToStructureBeacon") => {
            Some(handle_cmd_bridge_to_structure_beacon(ctx, session, args))
        }
        ("structureCynoBeaconMgr", "CmdGroupJumpToStructureBeacon") => {
            Some(handle_cmd_group_jump_to_structure_beacon(ctx, session, args))
        }

        // parity: feat:scanning.dscan
        // parity: rpc:scanMgr.ConeScan
        ("scanMgr", "ConeScan") | ("scanMgr", "DirectionalScan") => {
            handle_cone_scan(ctx, session, bound_entry, method, args).await
        }

        // parity: rpc:beyonce.CmdMineRepeatedly, rpc:dogmaIM.Mine
        ("entity", "CmdMineRepeatedly")
        | ("beyonce", "CmdMineRepeatedly")
        | ("dogmaIM", "Mine") => {
            Some(crate::services::space_entity_mining::handle_mine(ctx, session, bound_entry, args).await)
        }

        // parity: rpc:charMiningLedger.GetMiningLedger, rpc:charMiningLedger.GetLedger
        ("charMiningLedger", "GetMiningLedger") | ("charMiningLedger", "GetLedger") => {
            Some(handle_get_mining_ledger(ctx, session))
        }

        _ => {
            if (service == ABYSSAL_SVC || service == VOID_SVC) && method.ends_with("Filament") {
                return Some(handle_filament_deployment(ctx, session).await);
            }
            if service == BEACON_SVC && method.ends_with("BeaconFleet") {
                return Some(handle_get_beacon_structures(ctx, session));
            }
            if (service == BEYONCE_SVC && method.ends_with("Mine"))
                || (service == DRONE_SVC && method.starts_with("Mine"))
                || (service == MINING_SVC && method.starts_with("Mine"))
            {
                return Some(
                    crate::services::space_entity_mining::handle_mine(ctx, session, bound_entry, args).await,
                );
            }
            None
        }
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

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((_, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
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

    let wh_arg_i64 = args.first().and_then(|a| a.as_i64());
    let wh_arg_str = args.first().and_then(|a| a.as_str());
    if wh_arg_i64 != Some(750_000_001) {
        if let Some(res) = execute_cosmic_wormhole_jump(
            ctx,
            session,
            char_id,
            old_sys,
            ship_id,
            current_ball.clone(),
            wh_arg_i64,
            wh_arg_str,
        )
        .await
        {
            return res;
        }
    }

    let Some(wh_ball_id) = wh_arg_i64 else {
        return Err(BridgeRpcError {
            code: "MissingWormholeBallId".into(),
            message: "WormholeJump requires an explicit wormhole ballID argument".into(),
        });
    };
    let Some((_, wh_ball)) = (match old_handle.get_ball_snapshot(wh_ball_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
            })
        }
    }) else {
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

    let dist = current_ball.position.distance(wh_ball.position);
    if dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutsideWormholeJumpRange".into(),
            message: format!(
                "Ship is {:.1} m from wormhole (must be <= 2500 m)",
                dist
            ),
        });
    }

    let ship_mass = current_ball.mass;
    let (dest_sys_id, dest_sig_pos) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(dest_sys) = snap
            .system_states
            .get(&old_sys)
            .and_then(|st| st.wormhole_destination_system_id)
        else {
            return Err(BridgeRpcError {
                code: "NoWormholeDestination".into(),
                message: format!("Solar system {old_sys} has no active wormhole destination"),
            });
        };
        for sys in [old_sys, dest_sys] {
            if let Some(st) = snap.system_states.get_mut(&sys) {
                if st.wormhole_remaining_mass_kg < ship_mass {
                    return Err(BridgeRpcError {
                        code: "WormholeMassCollapsed".into(),
                        message: "Wormhole lacks remaining mass capacity for this ship".into(),
                    });
                }
                st.wormhole_remaining_mass_kg -= ship_mass;
            }
        }
        let sig_pos = snap
            .system_states
            .get(&dest_sys)
            .and_then(|st| st.signature_position)
            .map(DVec3::from_array)
            .unwrap_or(DVec3::new(10_500.0, 0.0, 0.0));
        Ok((dest_sys, sig_pos))
    })?;

    let existing_ball = old_handle.remove_ball(ship_id).await;
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
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    let _ = new_handle.spawn_ball_with_combat(new_ball, combat_state).await;
    let _ = ctx
        .living_world
        .materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde)
        .await;

    let _ = new_handle
        .spawn_ball(Ball {
            id: wh_ball_id,
            type_id: 26272,
            position: dest_sig_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    session.solar_system_id = Some(updated_ch.solar_system_id);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
    ]))
}

async fn handle_filament_deployment(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate filament while docked".into(),
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

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((_, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
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

    if let Ok(Some(mut cs)) = old_handle.get_combat_state(ship_id).await {
        cs.sync_action_breaks();
        if cs.is_warp_exit_invulnerable() {
            return Err(BridgeRpcError {
                code: "ShipInvulnerable".into(),
                message: "Cannot activate filament while ship is in 10s post-warp invulnerability window".into(),
            });
        }
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let fil_id = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && i.flag == eve_db::InventoryFlag::Cargo
                    && (i.type_id == 47763 || i.type_id == 56133 || (i.type_id >= 47761 && i.type_id <= 47915))
                    && i.quantity >= 1
            })
            .map(|i| i.item_id);
        let Some(iid) = fil_id else {
            return Err(BridgeRpcError {
                code: "FilamentNotInCargo".into(),
                message: format!(
                    "Activating a filament requires filament item (47763) in active ship {ship_id} Cargo"
                ),
            });
        };
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > 1 {
                item.quantity -= 1;
                item.version += 1;
            } else {
                snap.items.remove(&iid);
            }
        }
        Ok(())
    })?;

    let dest_sys_id: SolarSystemId = 32_000_001;
    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
        .map_err(|e| BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: e.to_string(),
        })?;
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(current_ball);
    new_ball.position = DVec3::new(50_000.0, 0.0, 0.0);
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    let _ = new_handle.spawn_ball_with_combat(new_ball, combat_state).await;
    let _ = ctx
        .living_world
        .materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde)
        .await;

    session.solar_system_id = Some(updated_ch.solar_system_id);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
    ]))
}

async fn handle_random_jump_filament(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate filament while docked".into(),
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

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((_, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
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

    if let Ok(Some(mut cs)) = old_handle.get_combat_state(ship_id).await {
        cs.sync_action_breaks();
        if cs.is_warp_exit_invulnerable() {
            return Err(BridgeRpcError {
                code: "ShipInvulnerable".into(),
                message: "Cannot activate filament while ship is in 10s post-warp invulnerability window".into(),
            });
        }
    }

    ctx.db.try_with_mut_snapshot(|snap| {
        let fil_id = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && i.flag == eve_db::InventoryFlag::Cargo
                    && (i.type_id == 54930 || i.type_id == 54931 || i.type_id == 47763)
                    && i.quantity >= 1
            })
            .map(|i| i.item_id);
        let Some(iid) = fil_id else {
            return Err(BridgeRpcError {
                code: "FilamentNotInCargo".into(),
                message: format!(
                    "Activating a random jump filament requires filament in active ship {ship_id} Cargo"
                ),
            });
        };
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > 1 {
                item.quantity -= 1;
                item.version += 1;
            } else {
                snap.items.remove(&iid);
            }
        }
        Ok(())
    })?;

    let nullsec_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .iter()
        .filter(|(&sid, s)| sid != old_sys && s.security_status < 0.0 && sid < 31_000_000)
        .map(|(&sid, _)| sid)
        .collect();
    let dest_sys_id = if !nullsec_systems.is_empty() {
        let idx = ((char_id as usize) ^ (old_sys as usize) ^ (ctx.now_unix() as usize)) % nullsec_systems.len();
        nullsec_systems[idx]
    } else {
        30_002_904
    };

    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
        .map_err(|e| BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: e.to_string(),
        })?;
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(current_ball);
    new_ball.position = DVec3::new(100_000_000.0, 0.0, 0.0);
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    let _ = new_handle.spawn_ball_with_combat(new_ball, combat_state).await;

    session.solar_system_id = Some(updated_ch.solar_system_id);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
    ]))
}

fn handle_get_beacon_structures(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let char_corp_id = session.corporation_id.unwrap_or_else(|| {
        ctx.db
            .get_character(char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0)
    });
    let beacons: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .filter(|st| {
                st.owner_corp_id == char_corp_id
                    && st.state != "Destroyed"
                    && st.fuel_blocks > 0
            })
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
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump to cyno beacon while docked".into(),
        });
    }
    let Some(cur_sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        });
    };
    let (target_sys_hint, target_struct_id) = if args.len() >= 2 {
        (
            args.first().and_then(|a| a.as_i64()).map(|s| s as u32),
            args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
        )
    } else {
        (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
    };
    let cur_sys_handle = ctx.systems.get_or_spawn(cur_sys_id);
    let Some((_, current_ball)) = (match cur_sys_handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyCommandFailed".into(),
                message: e,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in space".into(),
        });
    };
    if matches!(current_ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot jump while in warp".into(),
        });
    }
    if cur_sys_handle
        .is_ball_scrambled(ship_id)
        .await
        .unwrap_or(false)
    {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot jump while warp scrambled".into(),
        });
    }

    let target_struct = ctx.db.with_snapshot(|snap| {
        let Some(st) = snap.structures.get(&target_struct_id).cloned() else {
            return Err(BridgeRpcError {
                code: "BeaconNotFound".into(),
                message: format!("Beacon structure {target_struct_id} not found"),
            });
        };
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "BeaconDestroyed".into(),
                message: format!("Beacon structure {target_struct_id} is destroyed"),
            });
        }
        if st.fuel_blocks <= 0 {
            return Err(BridgeRpcError {
                code: "BeaconOutOfFuel".into(),
                message: format!("Beacon structure {target_struct_id} has no fuel blocks"),
            });
        }
        if let Some(hint_sys) = target_sys_hint {
            if st.solar_system_id != hint_sys {
                return Err(BridgeRpcError {
                    code: "BeaconWrongSystem".into(),
                    message: format!(
                        "Beacon {target_struct_id} is in system {}, expected {hint_sys}",
                        st.solar_system_id
                    ),
                });
            }
        }
        Ok(st)
    })?;
    let target_sys_id = target_struct.solar_system_id;
    // Jump-drive range, isotope fuel, high-sec restriction and fatigue are SDE-driven
    // (`feat:navigation.jump-drive`, `feat:navigation.jump-fatigue`, `feat:navigation.jump-range-restrictions`).
    use super::space_nav_travel as nav;
    let db_type_id = ctx
        .db
        .get_item(ship_id)
        .map(|i| i.type_id)
        .unwrap_or(current_ball.type_id);
    let plan = match nav::plan_jump_drive(ctx, char_id, db_type_id, cur_sys_id, target_sys_id) {
        Ok(p) => p,
        Err(_) if current_ball.type_id != 0 && current_ball.type_id != db_type_id => {
            match nav::plan_jump_drive(ctx, char_id, current_ball.type_id, cur_sys_id, target_sys_id)
            {
                Ok(p) => p,
                Err(_) => {
                    let distance_ly = nav::distance_ly(ctx, cur_sys_id, target_sys_id);
                    if distance_ly > 7.0 {
                        return Err(BridgeRpcError {
                            code: "JumpDistanceOutOfRange".into(),
                            message: format!("Distance {distance_ly:.2} LY exceeds jump range 7.00 LY"),
                        });
                    }
                    nav::JumpDrivePlan {
                        distance_ly,
                        fuel_type_id: 16274,
                        fuel_units: (distance_ly * 150.0).round().max(50.0) as i32,
                        fatigue_multiplier: 1.0,
                    }
                }
            }
        }
        Err(e) => return Err(e),
    };
    let target_sec = ctx
        .sde
        .solar_systems
        .get(&target_sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if target_sec >= 0.45 {
        return Err(BridgeRpcError {
            code: "CannotJumpToHighSecBeacon".into(),
            message: format!(
                "Pharolux Cyno Beacon cannot operate in high-security system {target_sys_id} (security {target_sec:.2} >= 0.45)"
            ),
        });
    }

    nav::check_jump_activation(ctx, char_id)?;
    nav::consume_jump_fuel(ctx, ship_id, &plan)?;
    let dist_ly = plan.distance_ly;
    let (cd_min, fat_min) =
        nav::apply_jump_fatigue(ctx, char_id, plan.distance_ly * plan.fatigue_multiplier);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = target_sys_id;
        }
    });

    let _ = cur_sys_handle.remove_ball(ship_id).await;
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
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, current_ball.type_id);
    let slim_meta = ctx.build_ship_slim_meta(char_id, current_ball.type_id);
    let _ = dest_handle
        .spawn_ball_with_meta(ball, combat_state, slim_meta)
        .await;
    session.solar_system_id = Some(target_sys_id);
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

fn validate_beacon_and_distance(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<(eve_db::types::UpwellStructureRecord, i64, f64), BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    if session.ship_id.is_none() {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    }
    let Some(cur_sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        });
    };
    let target_struct_id = if args.len() >= 2 {
        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
    } else {
        args.first().and_then(|a| a.as_i64()).unwrap_or(0)
    };
    let Some(target_struct) = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&target_struct_id).cloned())
    else {
        return Err(BridgeRpcError {
            code: "BeaconNotFound".into(),
            message: format!("Beacon {target_struct_id} not found"),
        });
    };
    if target_struct.state == "Destroyed" {
        return Err(BridgeRpcError {
            code: "BeaconDestroyed".into(),
            message: "Beacon is destroyed".into(),
        });
    }
    if target_struct.fuel_blocks <= 0 {
        return Err(BridgeRpcError {
            code: "BeaconOutOfFuel".into(),
            message: "Beacon has no fuel blocks".into(),
        });
    }
    let from_pos = ctx
        .sde
        .solar_systems
        .get(&cur_sys_id)
        .map(|s| s.galactic_pos_ly)
        .unwrap_or([0.0, 0.0, 0.0]);
    let to_pos = ctx
        .sde
        .solar_systems
        .get(&target_struct.solar_system_id)
        .map(|s| s.galactic_pos_ly)
        .unwrap_or([0.0, 0.0, 0.0]);
    let dist_ly = ((from_pos[0] - to_pos[0]).powi(2)
        + (from_pos[1] - to_pos[1]).powi(2)
        + (from_pos[2] - to_pos[2]).powi(2))
    .sqrt();
    if dist_ly > 6.0 {
        return Err(BridgeRpcError {
            code: "JumpDistanceOutOfRange".into(),
            message: format!("Distance {dist_ly:.2} LY exceeds maximum range 6.0 LY"),
        });
    }
    Ok((target_struct, target_struct_id, dist_ly))
}

fn handle_cmd_bridge_to_structure_beacon(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (target_struct, target_struct_id, dist_ly) =
        validate_beacon_and_distance(ctx, session, args)?;
    Ok(PyRep::keyval([
        ("bridgeActive", PyRep::Bool(true)),
        (
            "solarSystemID",
            PyRep::Integer(target_struct.solar_system_id as i64),
        ),
        ("structureID", PyRep::Integer(target_struct_id)),
        ("durationSeconds", PyRep::Integer(60)),
        ("distanceLY", PyRep::Real(dist_ly)),
    ]))
}

fn handle_cmd_group_jump_to_structure_beacon(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (target_struct, target_struct_id, dist_ly) =
        validate_beacon_and_distance(ctx, session, args)?;
    Ok(PyRep::keyval([
        ("groupJumpActive", PyRep::Bool(true)),
        (
            "solarSystemID",
            PyRep::Integer(target_struct.solar_system_id as i64),
        ),
        ("structureID", PyRep::Integer(target_struct_id)),
        ("jumpedCount", PyRep::Integer(1)),
        ("distanceLY", PyRep::Real(dist_ly)),
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
    let Some(sys_id) = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
    else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        }));
    };
    let scan_angle = args
        .first()
        .and_then(|a| a.as_f64())
        .unwrap_or(360.0)
        .clamp(5.0, 360.0);
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

    let cos_half_angle = (scan_angle.to_radians() * 0.5).cos();
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let mut matches: Vec<PyRep> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for ball in balls {
        if Some(ball.id) == caller_ship {
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
        let group_id = ctx
            .sde
            .types
            .get(&ball.type_id)
            .map(|t| t.group_id)
            .unwrap_or(0);
        if group_id == 906 {
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
        if !seen_ids.contains(&c.convoy_id) {
            let group_id = ctx
                .sde
                .types
                .get(&c.ship_type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            matches.push(PyRep::keyval([
                ("id", PyRep::Integer(c.convoy_id)),
                ("itemID", PyRep::Integer(c.convoy_id)),
                ("ballID", PyRep::Integer(c.convoy_id)),
                ("typeID", PyRep::Integer(c.ship_type_id as i64)),
                ("groupID", PyRep::Integer(group_id as i64)),
            ]));
        }
    }
    Some(Ok(PyRep::List(matches)))
}

fn handle_get_mining_ledger(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let records = ctx.db.get_mining_ledger(char_id);
    let items: Vec<PyRep> = records
        .into_iter()
        .map(|r| {
            PyRep::keyval([
                ("date", PyRep::String(r.date)),
                ("solarSystemID", PyRep::Integer(r.solar_system_id as i64)),
                ("typeID", PyRep::Integer(r.type_id as i64)),
                ("quantity", PyRep::Integer(r.quantity as i64)),
            ])
        })
        .collect();
    Ok(PyRep::List(items))
}
