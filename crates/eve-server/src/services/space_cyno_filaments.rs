//! `space_cyno_filaments` — Wormhole jumps, Abyssal/Void Filaments, Pharolux Cyno Beacons, D-Scan, and Mining (`S-3`, `SP-4`, `MN`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::CosmicSignatureRecord;
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
            Some(handle_cmd_bridge_to_structure_beacon(ctx, session, args))
        }
        ("structureCynoBeaconMgr", "CmdGroupJumpToStructureBeacon") => {
            Some(handle_cmd_group_jump_to_structure_beacon(ctx, session, args))
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

        ("charMiningLedger", "GetMiningLedger") | ("charMiningLedger", "GetLedger") => {
            Some(handle_get_mining_ledger(ctx, session))
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
    crate::services::tactical_modules::prepare_ship_for_wormhole_jump(
        ctx,
        &old_handle,
        char_id,
        ship_id,
        &mut current_ball,
    )
    .await?;

    let ship_type_id = ctx
        .db
        .get_item(ship_id)
        .map(|i| i.type_id)
        .unwrap_or(current_ball.type_id);
    let ship_group_id = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    if matches!(ship_group_id, 30 | 659) {
        return Err(BridgeRpcError {
            code: "ShipRestrictedFromWormhole".into(),
            message: format!(
                "Ship type {ship_type_id} (group {ship_group_id}) is restricted from entering wormholes (Supercarrier/Titan prohibition)"
            ),
        });
    }

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
    let now_unix = ctx.now_unix();
    if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Err(BridgeRpcError {
            code: "SessionChangeTimerActive".into(),
            message: "Session change timer is active".into(),
        });
    }

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
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    let dest_tick = new_handle.spawn_ball_with_combat(new_ball, combat_state).await.unwrap_or(0);
    let now_unix = ctx.now_unix();
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WormholeBuckets {
    pub destination_class: &'static str,
    pub max_ship_size: &'static str,
    pub lifetime_bucket: &'static str,
    pub mass_stability_bucket: &'static str,
}

pub fn derive_wormhole_buckets(
    wh: &CosmicSignatureRecord,
    now_unix: u64,
    sde: &eve_sde::SdeRegistry,
) -> WormholeBuckets {
    derive_wormhole_buckets_raw(
        wh.destination_system_id.unwrap_or(0),
        wh.max_jump_mass_kg,
        wh.remaining_mass_kg,
        wh.max_mass_kg,
        wh.created_unix,
        wh.lifetime_seconds,
        wh.collapsed,
        now_unix,
        sde,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn derive_wormhole_buckets_raw(
    dest_sys: u32,
    max_jump_mass_kg: f64,
    remaining_mass_kg: f64,
    max_mass_kg: f64,
    created_unix: u64,
    lifetime_seconds: u64,
    collapsed: bool,
    now_unix: u64,
    sde: &eve_sde::SdeRegistry,
) -> WormholeBuckets {
    // DERIVED: Destination classification resolved from SDE solar system, constellation, or region wormholeClassID
    let wh_class_id = sde
        .solar_systems
        .get(&dest_sys)
        .and_then(|s| {
            s.wormhole_class_id.or_else(|| {
                sde.constellations
                    .get(&s.constellation_id)
                    .and_then(|c| c.wormhole_class_id)
                    .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.wormhole_class_id))
            })
        });

    let dest_class = match wh_class_id {
        Some(12) => "Thera",
        Some(14..=18) => "Drifter",
        Some(25) => "Triglavian",
        Some(1..=3 | 13) => "C1-3",
        Some(4..=5) => "C4-5",
        Some(6) => "C6",
        Some(7) => "High-Sec",
        Some(8) => "Low-Sec",
        Some(9..=11) => "Null-Sec",
        _ => {
            let sec = sde
                .solar_systems
                .get(&dest_sys)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            if sec >= 0.45 {
                "High-Sec"
            } else if sec > 0.0 {
                "Low-Sec"
            } else {
                "Null-Sec"
            }
        }
    };

    // DERIVED: Ship size category bucket thresholds derived from mass
    let max_ship_size = if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 20_000_000.0 {
        "Small"
    } else if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 62_000_000.0 {
        "Medium"
    } else if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 375_000_000.0 {
        "Large"
    } else if max_jump_mass_kg > 375_000_000.0 {
        "Extra Large"
    } else {
        "Large"
    };

    // DERIVED: Lifetime stage bucket intervals derived from elapsed time
    let elapsed = now_unix.saturating_sub(created_unix);
    let remaining_seconds = lifetime_seconds.saturating_sub(elapsed);
    let lifetime_bucket = if collapsed
        || lifetime_seconds == 0
        || elapsed >= lifetime_seconds
        || remaining_seconds == 0
    {
        "expired"
    } else if remaining_seconds < 3_600 {
        "<1h"
    } else if remaining_seconds < 4 * 3_600 {
        "<4h"
    } else {
        "<1 day"
    };

    // DERIVED: Mass stability bucket threshold derived from mass ratio
    let mass_ratio = if max_mass_kg > 0.0 {
        remaining_mass_kg / max_mass_kg
    } else {
        0.0
    };
    let mass_stability_bucket = if remaining_mass_kg <= 0.0 || mass_ratio < 0.10 {
        "<10%"
    } else if mass_ratio < 0.50 {
        "<50%"
    } else {
        ">50%"
    };

    WormholeBuckets {
        destination_class: dest_class,
        max_ship_size,
        lifetime_bucket,
        mass_stability_bucket,
    }
}

async fn handle_random_jump_filament(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
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
    crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id)?;

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((cur_tick, mut current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
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
    if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        Some((old_sys, cur_tick)),
    ) {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot activate jump filament while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        });
    }
    crate::services::tactical_modules::prepare_ship_for_wormhole_jump(
        ctx,
        &old_handle,
        char_id,
        ship_id,
        &mut current_ball,
    )
    .await?;

    if let Ok(Some(mut cs)) = old_handle.get_combat_state(ship_id).await {
        cs.sync_action_breaks();
        if cs.is_warp_exit_invulnerable() {
            return Err(BridgeRpcError {
                code: "ShipInvulnerable".into(),
                message: "Cannot activate filament while ship is in 10s post-warp invulnerability window".into(),
            });
        }
    }

    let nullsec_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .iter()
        .filter(|(&sid, s)| sid != old_sys && s.security_status < 0.0 && sid < 31_000_000)
        .map(|(&sid, _)| sid)
        .collect();
    if nullsec_systems.is_empty() {
        return Err(BridgeRpcError {
            code: "NoNullsecSystemInSde".into(),
            message: "No destination nullsec solar system found in SDE".into(),
        });
    }
    let idx = ((char_id as usize) ^ (old_sys as usize) ^ (ctx.now_unix() as usize)) % nullsec_systems.len();
    let dest_sys_id = nullsec_systems[idx];
    let dest_sys = ctx.sde.solar_systems.get(&dest_sys_id).ok_or_else(|| BridgeRpcError {
        code: "NoNullsecSystemInSde".into(),
        message: "Destination nullsec solar system not found in SDE".into(),
    })?;
    let requested_item_id = args.first().and_then(|a| a.as_i64());

    const GROUP_JUMP_FILAMENTS: u32 = 4041;
    let updated_ch = ctx.db.try_with_mut_snapshot(|snap| {
        let fil_id = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && i.flag == eve_db::InventoryFlag::Cargo
                    && requested_item_id.is_none_or(|req_id| i.item_id == req_id)
                    && ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| t.group_id == GROUP_JUMP_FILAMENTS)
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
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
            });
        };
        ch.solar_system_id = dest_sys_id;
        ch.constellation_id = dest_sys.constellation_id;
        ch.region_id = dest_sys.region_id;
        ch.station_id = None;
        ch.structure_id = None;
        let updated = ch.clone();
        if let Some(item) = snap.items.get_mut(&iid) {
            if item.quantity > 1 {
                item.quantity -= 1;
                item.version += 1;
            } else {
                snap.items.remove(&iid);
            }
        }
        Ok(updated)
    })?;
    let existing_ball = old_handle.remove_ball(ship_id).await;
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(current_ball);
    new_ball.position = DVec3::new(100_000_000.0, 0.0, 0.0);
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    let dest_tick = new_handle
        .spawn_ball_with_combat(new_ball, combat_state)
        .await
        .unwrap_or(0);
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys_id,
        dest_tick,
        "filamentJump",
    );

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

fn validate_structure_cyno_beacon(
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
        return Err(BridgeRpcError {
            code: "InvalidStructureBeacon".into(),
            message: format!("Structure {target_struct_id} (type {}) is not a Pharolux Cyno Beacon", st.type_id),
        });
    }
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
    if !st.quantum_core_installed
        || matches!(
            st.state.as_str(),
            "Unanchored" | "Anchored" | "AnchorVulnerable" | "Anchoring" | "NoCore" | "Onlining"
        )
    {
        return Err(BridgeRpcError {
            code: "StructureBeaconOffline".into(),
            message: format!("Beacon structure {target_struct_id} is offline (state '{}')", st.state),
        });
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
    let (_, current_ball) = cur_sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in space".into(),
        })?;
    if matches!(current_ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot jump while in warp".into(),
        });
    }
    if cur_sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot jump while warp scrambled".into(),
        });
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;

    let target_struct = ctx
        .db
        .with_snapshot(|snap| validate_structure_cyno_beacon(snap, char_id, target_struct_id, target_sys_hint))?;
    let target_sys_id = target_struct.solar_system_id;
    if (31_000_000..32_000_000).contains(&cur_sys_id)
        || (31_000_000..32_000_000).contains(&target_sys_id)
    {
        return Err(BridgeRpcError {
            code: "JumpDriveForbiddenInWormholeSpace".into(),
            message: "Jump drive and cynosural beacon operations are prohibited in wormhole space"
                .into(),
        });
    }
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

    let now_unix = ctx.now_unix();
    if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Err(BridgeRpcError {
            code: "SessionChangeTimerActive".into(),
            message: "Session change timer is active".into(),
        });
    }

    nav::check_jump_activation(ctx, char_id)?;
    nav::consume_jump_fuel(ctx, ship_id, &plan)?;
    let dist_ly = plan.distance_ly;
    let (cd_min, fat_min) =
        nav::apply_jump_fatigue(ctx, char_id, plan.distance_ly * plan.fatigue_multiplier);
    let (dest_const_id, dest_reg_id) = ctx
        .sde
        .solar_systems
        .get(&target_sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = target_sys_id;
            if dest_const_id != 0 {
                ch.constellation_id = dest_const_id;
                ch.region_id = dest_reg_id;
            }
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = target_sys_id as i64;
        }
    });

    let _ = cur_sys_handle.remove_ball(ship_id).await;
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
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
    session.solar_system_id = Some(target_sys_id);
    if dest_const_id != 0 {
        session.constellation_id = Some(dest_const_id);
        session.region_id = Some(dest_reg_id);
    }
    session.station_id = None;
    session.structure_id = None;
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
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship".into(),
    })?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot bridge or jump to cyno beacon while docked".into(),
        });
    }
    let cur_sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solarSystemID".into(),
    })?;
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;
    let (target_sys_hint, target_struct_id) = if args.len() >= 2 {
        (
            args.first().and_then(|a| a.as_i64()).map(|s| s as u32),
            args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
        )
    } else {
        (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
    };
    let target_struct = ctx
        .db
        .with_snapshot(|snap| validate_structure_cyno_beacon(snap, char_id, target_struct_id, target_sys_hint))?;
    if (31_000_000..32_000_000).contains(&cur_sys_id)
        || (31_000_000..32_000_000).contains(&target_struct.solar_system_id)
    {
        return Err(BridgeRpcError {
            code: "JumpDriveForbiddenInWormholeSpace".into(),
            message: "Jump drive and cynosural beacon operations are prohibited in wormhole space"
                .into(),
        });
    }
    let target_sec = ctx.sde.solar_systems.get(&target_struct.solar_system_id).map(|s| s.security_status).unwrap_or(0.0);
    if target_sec >= 0.45 {
        return Err(BridgeRpcError {
            code: "CannotJumpToHighSecBeacon".into(),
            message: format!("Pharolux Cyno Beacon cannot operate in high-security system {}", target_struct.solar_system_id),
        });
    }
    let dist_ly = super::space_nav_travel::distance_ly(ctx, cur_sys_id, target_struct.solar_system_id);
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
    let (target_struct, target_struct_id, dist_ly) = validate_beacon_and_distance(ctx, session, args)?;
    Ok(PyRep::keyval([
        ("bridgeActive", PyRep::Bool(true)),
        ("solarSystemID", PyRep::Integer(target_struct.solar_system_id as i64)),
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
    let (target_struct, target_struct_id, dist_ly) = validate_beacon_and_distance(ctx, session, args)?;
    Ok(PyRep::keyval([
        ("groupJumpActive", PyRep::Bool(true)),
        ("solarSystemID", PyRep::Integer(target_struct.solar_system_id as i64)),
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

async fn handle_cmd_beacon_jump_fleet(
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
            message: "Cannot jump while docked".into(),
        });
    }
    let cur_sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solarSystemID".into(),
    })?;

    let cur_sys_handle = ctx.systems.get_or_spawn(cur_sys_id);
    let (_, current_ball) = cur_sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in space".into(),
        })?;

    if matches!(current_ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot jump while in warp".into(),
        });
    }
    if cur_sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot jump while warp scrambled".into(),
        });
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;

    let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let _target_ship_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    let target_sys_id = args.get(2).and_then(|a| a.as_i64()).map(|s| s as u32).unwrap_or(0);

    let same_fleet = ctx.db.with_snapshot(|snap| {
        let f_a = snap.fleets.values().find(|f| f.members.contains(&char_id)).map(|f| f.fleet_id);
        let f_b = snap.fleets.values().find(|f| f.members.contains(&target_char_id)).map(|f| f.fleet_id);
        f_a.is_some() && f_a == f_b
    });
    if !same_fleet {
        return Err(BridgeRpcError {
            code: "FleetNotInFleet".into(),
            message: format!("Target character {target_char_id} is not in caller's fleet"),
        });
    }

    if (31_000_000..32_000_000).contains(&cur_sys_id)
        || (31_000_000..32_000_000).contains(&target_sys_id)
    {
        return Err(BridgeRpcError {
            code: "JumpDriveForbiddenInWormholeSpace".into(),
            message: "Jump drive operations are prohibited in wormhole space".into(),
        });
    }

    let target_sec = ctx.sde.solar_systems.get(&target_sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if target_sec >= 0.45 {
        return Err(BridgeRpcError {
            code: "CannotJumpToHighSecBeacon".into(),
            message: format!("Cynosural beacon cannot operate in high-security system {target_sys_id}"),
        });
    }

    let beacon_res = crate::services::inventory_fitting::fitting_cyno_portal::live_beacon(
        ctx,
        target_char_id,
        0,
        target_sys_id,
    );
    let beacon = beacon_res.map_err(|e| BridgeRpcError {
        code: "NoActiveBeacon".into(),
        message: e.message,
    })?;

    use super::space_nav_travel as nav;
    let db_type_id = ctx.db.get_item(ship_id).map(|i| i.type_id).unwrap_or(current_ball.type_id);
    let plan = nav::plan_jump_drive(ctx, char_id, db_type_id, cur_sys_id, target_sys_id)?;

    let now_unix = ctx.now_unix();
    if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Err(BridgeRpcError {
            code: "SessionChangeTimerActive".into(),
            message: "Session change timer is active".into(),
        });
    }

    nav::check_jump_activation(ctx, char_id)?;
    nav::consume_jump_fuel(ctx, ship_id, &plan)?;
    let dist_ly = plan.distance_ly;
    let (cd_min, fat_min) = nav::apply_jump_fatigue(ctx, char_id, plan.distance_ly * plan.fatigue_multiplier);
    let (dest_const_id, dest_reg_id) = ctx
        .sde
        .solar_systems
        .get(&target_sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((0, 0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = target_sys_id;
            if dest_const_id != 0 {
                ch.constellation_id = dest_const_id;
                ch.region_id = dest_reg_id;
            }
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = target_sys_id as i64;
        }
    });

    let _ = cur_sys_handle.remove_ball(ship_id).await;
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let target_pos = beacon.position + DVec3::new(2_500.0, 0.0, 0.0);
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
    session.solar_system_id = Some(target_sys_id);
    if dest_const_id != 0 {
        session.constellation_id = Some(dest_const_id);
        session.region_id = Some(dest_reg_id);
    }
    session.station_id = None;
    session.structure_id = None;
    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, target_sys_id as i64)?;

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(target_sys_id as i64)),
        ("beaconID", PyRep::Integer(beacon.beacon_id)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
        ("jumpFatigueMinutes", PyRep::Real(fat_min)),
        ("jumpCooldownMinutes", PyRep::Real(cd_min)),
        ("distanceLY", PyRep::Real(dist_ly)),
        ("fuelConsumed", PyRep::Integer(plan.fuel_units as i64)),
        ("fuelTypeID", PyRep::Integer(plan.fuel_type_id as i64)),
    ]))
}
