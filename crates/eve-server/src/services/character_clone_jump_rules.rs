//! `character_clone_jump_rules` — V24 Jump clone rules, preconditions, fees, and space restrictions.
//!
//! Implements:
//! - `feat:clones.alpha-jump-clone-access`
//! - `feat:clones.clone-jump-booster-and-implant-handling`
//! - `feat:clones.clone-jump-preconditions-and-fees`
//! - `feat:clones.pochven-clone-restrictions`
//! - `feat:clones.same-location-clone-swap`
//! - `feat:clones.wormhole-clone-jump-ban`
//! - `feat:clones.jump-clone-structure-dock`

#![forbid(unsafe_code)]

use crate::formulas::clone_jump_cooldown_seconds;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::station_structure::structure_upwell_ops;
use eve_db::{InventoryFlag, IskCents, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

/// Pochven region ID (10000070).
pub const POCHVEN_REGION_ID: u32 = 10_000_070;
/// Thera solar system ID (31000005).
pub const THERA_SYSTEM_ID: u32 = 31_000_005;
/// Canonical NPC station clone jump leave-behind fee: 900,000 ISK.
pub const NPC_LEAVE_BEHIND_FEE_CENTS: i64 = 900_000_00;

/// Checks if an Alpha clone has capacity to install a new jump clone.
/// Per V24 rules, Alpha characters cannot install new jump clones (Omega capacity required).
pub fn check_alpha_clone_install_capacity(
    snap: &WorldStateSnapshot,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    if let Some(ch) = snap.characters.get(&char_id) {
        if ch.clone_grade <= 1 {
            return Err(BridgeRpcError {
                code: "OmegaCapacityRequired".into(),
                message: "Installing jump clones requires an active Omega clone account".into(),
            });
        }
    }
    Ok(())
}

/// Evaluates structure clone bay access against Upwell structure access lists.
pub fn check_structure_clone_bay_access(
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    if !structure_upwell_ops::evaluate_character_structure_access_in_snap(
        snap,
        structure_id,
        char_id,
    ) {
        return Err(BridgeRpcError {
            code: "StructureAccessDenied".into(),
            message: format!("Access to structure {structure_id} clone bay is denied by access list"),
        });
    }
    let ext = structure_upwell_ops::load_ext_state(snap);
    if let Some(prof) = ext.access_profiles.get(&structure_id) {
        if prof.general_settings.get(&2) == Some(&false)
            || prof.general_settings.get(&35894) == Some(&false)
        {
            return Err(BridgeRpcError {
                code: "StructureAccessDenied".into(),
                message: format!("Clone bay service on structure {structure_id} is denied by profile settings"),
            });
        }
    }
    Ok(())
}

/// Computes clone jump fee at an Upwell structure, owner-set per standing (minimum 1 ISK).
pub fn compute_structure_clone_fee(
    snap: &WorldStateSnapshot,
    structure_id: i64,
    standing: f64,
) -> IskCents {
    let ext = structure_upwell_ops::load_ext_state(snap);
    let configured_fee = ext
        .access_profiles
        .get(&structure_id)
        .and_then(|p| p.service_settings.get(&35894).copied()); // 35894 = Standup Cloning Center
    if let Some(bps) = configured_fee {
        let val = (bps as i64) * 100;
        IskCents(val.max(100))
    } else if standing >= 5.0 {
        IskCents(100) // 1 ISK minimum
    } else if standing >= 0.0 {
        IskCents(500) // 5 ISK
    } else {
        IskCents(1000) // 10 ISK
    }
}

/// Handles `jumpCloneSvc.CloneJump` with comprehensive V24 precondition, fee, and space rules.
pub fn handle_clone_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if session.station_id.is_none() && session.structure_id.is_none() {
        return Some(Err(BridgeRpcError {
            code: "MustBeDocked".into(),
            message: "Clone jump requires being docked at a station or structure".into(),
        }));
    }
    let Some(departure_loc_id) = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .and_then(|c| c.station_id.or(c.structure_id))
        })
        .or_else(|| session.station_id.or(session.structure_id))
    else {
        return Some(Err(BridgeRpcError {
            code: "MustBeDocked".into(),
            message: "Clone jump requires being docked at a station or structure".into(),
        }));
    };

    let target_jc_id = if args.len() >= 2 {
        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
    } else {
        args.first().and_then(|a| a.as_i64()).unwrap_or(0)
    };
    if target_jc_id <= 0 {
        return Some(Err(BridgeRpcError {
            code: "InvalidJumpCloneID".into(),
            message: "CloneJump requires a positive jumpCloneID".into(),
        }));
    }

    let departure_has_clone_bay = if departure_loc_id >= 1_000_000_000_000 {
        ctx.db.with_snapshot(|snap| {
            let has_service_items = snap
                .items
                .values()
                .any(|i| i.location_id == departure_loc_id && i.flag.is_service_slot());
            let online_clone_bay = snap.items.values().any(|i| {
                i.location_id == departure_loc_id
                    && i.type_id == 35894
                    && i.flag.is_service_slot()
                    && snap.module_online_states.get(&i.item_id).copied().unwrap_or(false)
            });
            snap.structures
                .get(&departure_loc_id)
                .map(|st| {
                    st.quantum_core_installed
                        && st.fuel_blocks > 0
                        && (!has_service_items || online_clone_bay)
                })
                .unwrap_or(false)
        })
    } else {
        ctx.sde
            .solar_systems
            .values()
            .flat_map(|s| s.stations.iter())
            .find(|st| st.station_id == departure_loc_id)
            .map(|st| st.has_clone_bay)
            .unwrap_or(false)
    };
    let now_unix = ctx.now_unix();
    let pre_ship_host: Option<(i64, u32)> = ctx.db.with_snapshot(|snap| {
        let loc_id = snap
            .jump_clones
            .get(&char_id)?
            .iter()
            .find(|c| c.jump_clone_id == target_jc_id)?
            .location_id;
        let ship_it = snap.items.get(&loc_id)?;
        if !ctx.sde.types.get(&ship_it.type_id).is_some_and(|t| matches!(t.group_id, 30 | 883)) {
            return None;
        }
        let host_sys = snap
            .characters
            .get(&ship_it.owner_id)
            .map(|c| c.solar_system_id)
            .unwrap_or(0);
        Some((loc_id, host_sys))
    });
    let live_host_ball_state: Option<(u32, [f64; 3], bool)> = pre_ship_host.and_then(|(ship_id, sys_id)| {
        if sys_id == 0 {
            return None;
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let b = bp.balls.get(&ship_id)?;
        let cloaked = bp.combat_states.get(&ship_id).is_some_and(|cs| cs.is_cloaked)
            || bp.ball_slim_meta.get(&ship_id).is_some_and(|m| m.is_cloaked);
        let adj_x = b.position.x + b.radius.max(250.0) + 150.0;
        Some((sys_id, [adj_x, b.position.y, b.position.z], cloaked))
    });

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let sync_level = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&33399))
            .map(|s| s.level)
            .unwrap_or(0);
        let full_cooldown_sec = clone_jump_cooldown_seconds(sync_level);

        let Some(ch_check) = snap.characters.get(&char_id) else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
            });
        };

        let dest_loc_id = {
            let clones = snap.jump_clones.get(&char_id).ok_or_else(|| BridgeRpcError {
                code: "JumpCloneNotFound".into(),
                message: format!("Jump clone {target_jc_id} not found"),
            })?;
            let c = clones
                .iter()
                .find(|c| c.jump_clone_id == target_jc_id)
                .ok_or_else(|| BridgeRpcError {
                    code: "JumpCloneNotFound".into(),
                    message: format!("Jump clone {target_jc_id} not found"),
                })?;
            c.location_id
        };
        let same_station = dest_loc_id == departure_loc_id;
        let same_structure_swap =
            same_station && dest_loc_id >= 1_000_000_000_000 && departure_has_clone_bay;
        if !departure_has_clone_bay {
            return Err(BridgeRpcError {
                code: "StationHasNoCloneBay".into(),
                message: format!(
                    "Clone jump from {departure_loc_id} requires an active clone bay to store your left-behind clone"
                ),
            });
        }
        let effective_cooldown = if same_structure_swap { 0 } else { full_cooldown_sec };

        // feat:clones.clone-jump-preconditions-and-fees: no session-change timer running
        if crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None).is_some() {
            return Err(BridgeRpcError {
                code: "SessionChangeTimerActive".into(),
                message: "Session change timer is active".into(),
            });
        }

        // feat:clones.same-location-clone-swap: jumping to clone stored in the same Upwell structure
        // with an online Standup Cloning Center I incurs no cooldown and is allowed while cooldown is active.
        if !same_structure_swap
            && ch_check.last_clone_jump_unix > 0
            && now_unix < ch_check.last_clone_jump_unix.saturating_add(full_cooldown_sec)
        {
            return Err(BridgeRpcError {
                code: "CloneJumpCooldownActive".into(),
                message: format!(
                    "Clone jump is on cooldown for {} seconds",
                    ch_check
                        .last_clone_jump_unix
                        .saturating_add(full_cooldown_sec)
                        .saturating_sub(now_unix)
                ),
            });
        }

        // Resolve departure solar_system_id and region_id
        let (dep_sys_id, dep_reg_id) = if departure_loc_id >= 1_000_000_000_000 {
            let st = snap.structures.get(&departure_loc_id).ok_or_else(|| BridgeRpcError {
                code: "DepartureStructureNotFound".into(),
                message: format!("Departure structure {departure_loc_id} not found"),
            })?;
            check_structure_clone_bay_access(snap, departure_loc_id, char_id)?;
            let sys = ctx.sde.solar_systems.get(&st.solar_system_id).ok_or_else(|| BridgeRpcError {
                code: "DepartureSystemNotFound".into(),
                message: format!("Departure system {} not found in SDE", st.solar_system_id),
            })?;
            (st.solar_system_id, sys.region_id)
        } else {
            let (sys, _) = ctx
                .sde
                .solar_systems
                .values()
                .find_map(|sys| {
                    sys.stations
                        .iter()
                        .find(|st| st.station_id == departure_loc_id)
                        .map(|st| (sys, st))
                })
                .ok_or_else(|| BridgeRpcError {
                    code: "DepartureStationNotFound".into(),
                    message: format!("Departure station {departure_loc_id} not found in SDE"),
                })?;
            (sys.solar_system_id, sys.region_id)
        };

        // Resolve destination solar_system_id, constellation_id, and region_id
        let mut ship_clone_spawn_pos: Option<[f64; 3]> = None;
        let (dest_sys_id, dest_const_id, dest_reg_id) = if dest_loc_id >= 1_000_000_000_000 {
            let Some(st) = snap.structures.get(&dest_loc_id) else {
                return Err(BridgeRpcError {
                    code: "CloneDestinationNotFound".into(),
                    message: format!("Destination structure {dest_loc_id} no longer exists"),
                });
            };
            let has_service_items = snap
                .items
                .values()
                .any(|i| i.location_id == dest_loc_id && i.flag.is_service_slot());
            let online_clone_bay = snap.items.values().any(|i| {
                i.location_id == dest_loc_id
                    && i.type_id == 35894
                    && i.flag.is_service_slot()
                    && snap.module_online_states.get(&i.item_id).copied().unwrap_or(false)
            });
            if !st.quantum_core_installed || st.fuel_blocks == 0 || (has_service_items && !online_clone_bay) {
                return Err(BridgeRpcError {
                    code: "StructureHasNoCloneBay".into(),
                    message: format!(
                        "Destination structure {dest_loc_id} does not have an online Standup Cloning Center service"
                    ),
                });
            }
            // feat:clones.jump-clone-structure-dock: clone bay on Upwell structures is access-list gated
            check_structure_clone_bay_access(snap, dest_loc_id, char_id)?;

            let sys_id = st.solar_system_id;
            let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
                return Err(BridgeRpcError {
                    code: "CloneDestinationNotFound".into(),
                    message: format!("Destination solar system {sys_id} not found in SDE"),
                });
            };
            (sys_id, sys.constellation_id, sys.region_id)
        } else if let Some(host_ship) = snap.items.get(&dest_loc_id).filter(|it| {
            ctx.sde.types.get(&it.type_id).is_some_and(|t| matches!(t.group_id, 30 | 883))
        }) {
            // feat:modules.clone-vat-bay-fuel: jumping to a clone installed in a Titan or Rorqual Clone Vat Bay
            let host_owner = host_ship.owner_id;
            let Some(host_ch) = snap.characters.get(&host_owner) else {
                return Err(BridgeRpcError {
                    code: "NoActiveCloneVatBay".into(),
                    message: "Host ship pilot not found".into(),
                });
            };
            if host_ch.active_ship_id != dest_loc_id || host_ch.station_id.is_some() || host_ch.structure_id.is_some() {
                return Err(BridgeRpcError {
                    code: "NoActiveCloneVatBay".into(),
                    message: "Host Titan or Rorqual must be piloted in space".into(),
                });
            }
            let in_fleet = char_id == host_owner
                || snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&host_owner));
            if !in_fleet {
                return Err(BridgeRpcError {
                    code: "NotInHostShipFleet".into(),
                    message: "Clone jump to a ship Clone Vat Bay requires being in the host pilot's fleet".into(),
                });
            }
            if !is_ship_clone_vat_bay_active(ctx, snap, dest_loc_id, now_unix) {
                return Err(BridgeRpcError {
                    code: "NoActiveCloneVatBay".into(),
                    message: "Host ship Clone Vat Bay is not currently active".into(),
                });
            }
            let sys_id = host_ch.solar_system_id;
            let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
                return Err(BridgeRpcError {
                    code: "CloneDestinationNotFound".into(),
                    message: format!("Host ship solar system {sys_id} not found in SDE"),
                });
            };
            let has_active_cloak = snap.items.values().any(|i| {
                i.location_id == dest_loc_id
                    && matches!(i.flag as u16, 11..=34)
                    && snap.module_active_states.contains(&i.item_id)
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 330)
            });
            let Some((_, live_spawn_pos, live_cloaked)) = live_host_ball_state.filter(|(ls, _, _)| *ls == sys_id) else {
                return Err(BridgeRpcError {
                    code: "NoActiveCloneVatBay".into(),
                    message: "Host Titan or Rorqual is not present in live space".into(),
                });
            };
            if live_cloaked || has_active_cloak {
                return Err(BridgeRpcError {
                    code: "ShipCloaked".into(),
                    message: "Cannot clone jump to a cloaked Titan or Rorqual".into(),
                });
            }
            ship_clone_spawn_pos = Some(live_spawn_pos);
            (sys_id, sys.constellation_id, sys.region_id)
        } else {
            let Some((sys, dest_station)) = ctx.sde.solar_systems.values().find_map(|sys| {
                sys.stations
                    .iter()
                    .find(|st| st.station_id == dest_loc_id)
                    .map(|st| (sys, st))
            }) else {
                return Err(BridgeRpcError {
                    code: "CloneDestinationNotFound".into(),
                    message: format!("Destination station {dest_loc_id} not found in SDE"),
                });
            };
            if !dest_station.has_clone_bay {
                return Err(BridgeRpcError {
                    code: "StationHasNoCloneBay".into(),
                    message: format!("Destination station {dest_loc_id} does not have an active clone bay"),
                });
            }
            (sys.solar_system_id, sys.constellation_id, sys.region_id)
        };

        // feat:clones.clone-jump-preconditions-and-fees: no session-change timer running
        if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
            return Err(BridgeRpcError {
                code: "SessionChangeTimerActive".into(),
                message: "Session change timer is active".into(),
            });
        }

        if !same_structure_swap
            && ch_check.last_clone_jump_unix > 0
            && now_unix < ch_check.last_clone_jump_unix.saturating_add(full_cooldown_sec)
        {
            return Err(BridgeRpcError {
                code: "CloneJumpCooldownActive".into(),
                message: format!(
                    "Clone jump is on cooldown for {} seconds",
                    ch_check
                        .last_clone_jump_unix
                        .saturating_add(full_cooldown_sec)
                        .saturating_sub(now_unix)
                ),
            });
        }

        // feat:clones.clone-jump-preconditions-and-fees: in a capsule (not in any ship)
        let active_ship_opt = snap
            .characters
            .get(&char_id)
            .and_then(|c| (c.active_ship_id > 0).then_some(c.active_ship_id))
            .or(session.ship_id);
        if let Some(active_ship_id) = active_ship_opt {
            let is_capsule = snap
                .items
                .get(&active_ship_id)
                .is_some_and(|it| matches!(it.type_id, 670 | 33328));
            if !is_capsule {
                return Err(BridgeRpcError {
                    code: "MustBeInCapsule".into(),
                    message: "Clone jump requires being in a capsule (not in any ship)".into(),
                });
            }
        }

        // feat:clones.clone-jump-preconditions-and-fees: Infomorph Psychology I required for any clone action
        let psych_level = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&24242))
            .map(|s| s.level)
            .unwrap_or(0);
        if psych_level < 1 {
            return Err(BridgeRpcError {
                code: "InfomorphPsychologyRequired".into(),
                message: "Clone actions require Infomorph Psychology level 1 or higher".into(),
            });
        }

        // feat:clones.wormhole-clone-jump-ban:
        // Clone jumps into, out of or between wormhole systems are refused, except to and from Thera.
        // In-structure clone swaps inside a W-space Upwell structure are allowed.
        if !same_station {
            let dep_wh = crate::services::quasar_social_chat::is_wormhole_system(dep_sys_id) && dep_sys_id != THERA_SYSTEM_ID;
            let dest_wh = crate::services::quasar_social_chat::is_wormhole_system(dest_sys_id) && dest_sys_id != THERA_SYSTEM_ID;
            if dep_wh || dest_wh {
                return Err(BridgeRpcError {
                    code: "CannotCloneJumpInWormholeSpace".into(),
                    message: "Clone jumps into, out of or between wormhole systems are refused (except to/from Thera)".into(),
                });
            }
        }

        // feat:clones.pochven-clone-restrictions:
        // Clone jumps into Pochven are refused. Clone bays in Pochven allow local clone swaps only.
        if !same_station {
            if dest_reg_id == POCHVEN_REGION_ID {
                return Err(BridgeRpcError {
                    code: "CannotCloneJumpToPochven".into(),
                    message: "Clone jumps into Pochven are refused; clone bays in Pochven allow local clone swaps only".into(),
                });
            }
            if dep_reg_id == POCHVEN_REGION_ID {
                return Err(BridgeRpcError {
                    code: "CannotCloneJumpFromPochven".into(),
                    message: "Clone bays in Pochven allow local clone swaps only".into(),
                });
            }
            if dest_loc_id == 60_015_187 {
                let war_fac = snap.characters.get(&char_id).and_then(|c| c.war_faction_id);
                if !matches!(war_fac, Some(500_010 | 500_011)) {
                    return Err(BridgeRpcError {
                        code: "FulcrumCloneRequiresPirateEnlistment".into(),
                        message: "Clone jump to The Fulcrum requires pirate militia enlistment".into(),
                    });
                }
            }
        }

        // feat:clones.clone-jump-preconditions-and-fees:
        // Leaving current body behind at an NPC station costs 900,000 ISK, debited and journaled.
        // At an Upwell structure the owner sets the fee per standing, minimum 1 ISK (in-structure swap is free).
        if !same_structure_swap {
            let leave_behind_fee = if departure_loc_id >= 1_000_000_000_000 {
                let owner_corp = snap.structures.get(&departure_loc_id).map(|s| s.owner_corp_id).unwrap_or(0);
                let standing = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.standings.get(&owner_corp).copied())
                    .unwrap_or(0.0);
                compute_structure_clone_fee(snap, departure_loc_id, standing)
            } else {
                IskCents(NPC_LEAVE_BEHIND_FEE_CENTS)
            };

            let balance_after = {
                let ch = snap.characters.get_mut(&char_id).unwrap();
                let new_bal = ch.balance.checked_sub(leave_behind_fee).ok_or_else(|| {
                    BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!("Leaving your clone behind requires {} ISK", leave_behind_fee.to_wire_f64()),
                    }
                })?;
                ch.balance = new_bal;
                new_bal
            };

            snap.append_journal(
                char_id,
                "CloneJumpFee",
                IskCents(-leave_behind_fee.0),
                balance_after,
                if departure_loc_id >= 1_000_000_000_000 {
                    format!("Clone jump leave-behind fee at structure {departure_loc_id}")
                } else {
                    "Clone jump leave-behind body fee at NPC station".into()
                },
            );
            if departure_loc_id >= 1_000_000_000_000 {
                let owner_corp_opt = snap.structures.get(&departure_loc_id).map(|st| st.owner_corp_id);
                if let Some(owner_corp_id) = owner_corp_opt {
                    if let Some(corp) = snap.corporations.get_mut(&owner_corp_id) {
                        let new_corp_bal = corp
                            .master_wallet
                            .checked_add(leave_behind_fee)
                            .unwrap_or(corp.master_wallet);
                        corp.master_wallet = new_corp_bal;
                        corp.set_division_balance(1, new_corp_bal);
                    }
                }
            }
        }

        // Swap clone record to departure station (old body becomes jump clone at departure_loc_id)
        let jc_idx = {
            let clones = snap.jump_clones.entry(char_id).or_default();
            let idx = clones
                .iter()
                .position(|c| c.jump_clone_id == target_jc_id)
                .unwrap();
            clones[idx].location_id = departure_loc_id;
            idx
        };

        // Ensure character is placed in a Capsule (type_id 670) at dest_loc_id (or in space near host ship)
        let cap_loc = if ship_clone_spawn_pos.is_some() { dest_sys_id as i64 } else { dest_loc_id };
        let cap_flag = if ship_clone_spawn_pos.is_some() { InventoryFlag::None } else { InventoryFlag::Hangar };
        let existing_capsule_id = snap
            .items
            .values()
            .find(|i| i.owner_id == char_id && i.type_id == 670 && i.location_id == cap_loc)
            .map(|i| i.item_id);
        let capsule_id = if let Some(cid) = existing_capsule_id {
            cid
        } else {
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            snap.items.insert(
                cid,
                eve_db::InventoryItemRecord {
                    item_id: cid,
                    type_id: 670, // Capsule
                    owner_id: char_id,
                    location_id: cap_loc,
                    flag: cap_flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            cid
        };
        if let Some(pos) = ship_clone_spawn_pos {
            snap.item_positions.insert(capsule_id, pos);
        }

        // feat:clones.clone-jump-booster-and-implant-handling:
        // Skills, standings, corporation membership and active cerebral accelerators transfer.
        // Implants stay in old body (which becomes jump clone). Active boosters and side effects removed.
        let active_implants = {
            let char_implants = snap.active_implants.entry(char_id).or_default();
            if let Some(clones_mut) = snap.jump_clones.get_mut(&char_id) {
                std::mem::swap(char_implants, &mut clones_mut[jc_idx].implants);
            }
            char_implants.clone()
        };
        crate::services::implant_plugging::sync_clone_jump_implants_and_boosters(&ctx.sde, snap, char_id);

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            if ship_clone_spawn_pos.is_some() {
                ch.station_id = None;
                ch.structure_id = None;
            } else if dest_loc_id >= 1_000_000_000_000 {
                ch.structure_id = Some(dest_loc_id);
                ch.station_id = None;
            } else {
                ch.station_id = Some(dest_loc_id);
                ch.structure_id = None;
            }
            ch.solar_system_id = dest_sys_id;
            ch.constellation_id = dest_const_id;
            ch.region_id = dest_reg_id;
            ch.active_ship_id = capsule_id;
            if !same_structure_swap {
                ch.last_clone_jump_unix = now_unix;
            }
            ch.version += 1;
            ch.system_epoch += 1;
        }
        let cleared_zarzakh_gate = if !same_structure_swap {
            snap.zarzakh_gate_locks.remove(&char_id).map(|(gid, _)| gid)
        } else {
            None
        };
        Ok((
            target_jc_id,
            dest_loc_id,
            dest_sys_id,
            dest_const_id,
            dest_reg_id,
            capsule_id,
            effective_cooldown,
            active_implants,
            cleared_zarzakh_gate,
            ship_clone_spawn_pos,
        ))
    });

    match res {
        Ok((
            jc_id,
            dest_loc_id,
            dest_sys_id,
            dest_const_id,
            dest_reg_id,
            capsule_id,
            cooldown_sec,
            active_implants,
            cleared_zarzakh_gate,
            ship_clone_spawn_pos,
        )) => {
            if let Some(locked_gate_id) = cleared_zarzakh_gate {
                crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
                    &ctx.quasar_gateway,
                    char_id,
                    30_100_000,
                    locked_gate_id,
                    now_unix,
                );
            }
            if let Some(spawn_pos) = ship_clone_spawn_pos {
                session.station_id = None;
                session.structure_id = None;
                let (ch_corp_id, ch_alliance_id, ch_sec_status) = ctx.db.with_snapshot(|s| {
                    if let Some(ch) = s.characters.get(&char_id) {
                        let all_id = ch
                            .alliance_id
                            .or_else(|| s.corporations.get(&ch.corporation_id).and_then(|co| co.alliance_id))
                            .unwrap_or(-1);
                        (ch.corporation_id, all_id, ch.security_status)
                    } else {
                        (-1, -1, 0.0)
                    }
                });
                let sys_handle = ctx.systems.get_or_spawn(dest_sys_id);
                let cap_sde = ctx.sde.types.get(&670);
                sys_handle.try_spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: capsule_id,
                        type_id: 670,
                        position: glam::DVec3::from_array(spawn_pos),
                        velocity: glam::DVec3::ZERO,
                        max_velocity: cap_sde.and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(335.0),
                        inertia: cap_sde.and_then(|t| t.default_attributes.get(&70).copied()).unwrap_or(0.25),
                        mass: cap_sde.map(|t| t.mass.max(1_000.0)).unwrap_or(16_000.0),
                        radius: cap_sde.map(|t| t.radius.max(15.0)).unwrap_or(15.0),
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_ship_combat_state(char_id, capsule_id, 670),
                    eve_destiny::SlimItemMeta {
                        group_id: cap_sde.map(|t| t.group_id).unwrap_or(29),
                        category_id: 6,
                        owner_id: char_id,
                        char_id: Some(char_id),
                        corp_id: ch_corp_id,
                        alliance_id: ch_alliance_id,
                        security_status: ch_sec_status,
                        is_cloaked: false,
                    },
                );
            } else if dest_loc_id >= 1_000_000_000_000 {
                session.structure_id = Some(dest_loc_id);
                session.station_id = None;
            } else {
                session.station_id = Some(dest_loc_id);
                session.structure_id = None;
            }
            session.solar_system_id = Some(dest_sys_id);
            session.constellation_id = Some(dest_const_id);
            session.region_id = Some(dest_reg_id);
            session.ship_id = Some(capsule_id);
            let timer_reason = if dest_loc_id == departure_loc_id {
                "clone_swap"
            } else {
                "clone_jump"
            };
            crate::services::space_destiny::record_session_change_timer(
                ctx,
                char_id,
                now_unix,
                dest_sys_id,
                0,
                timer_reason,
            );
            if timer_reason == "clone_jump" {
                crate::services::map_service::record_solar_system_visit(ctx, char_id, dest_sys_id);
            }
            crate::services::jump_clone_names::emit_station_jump_clone_invalidated(
                ctx,
                char_id,
                dest_loc_id,
                true,
            );
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnCloneJumpUpdate",
                "charid",
                vec![PyRep::Integer(jc_id), PyRep::Integer(dest_loc_id)],
            );
            let implant_reps: Vec<PyRep> = active_implants
                .into_iter()
                .map(|t| PyRep::Integer(t as i64))
                .collect();
            Some(Ok(PyRep::keyval([
                ("jumpCloneID", PyRep::Integer(jc_id)),
                ("locationID", PyRep::Integer(dest_loc_id)),
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("shipID", PyRep::Integer(capsule_id)),
                ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ("activeImplants", PyRep::List(implant_reps)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

const CLONE_VAT_BAY_STATE_KEY: &str = "clone_vat_bay_state";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CloneVatBayState {
    pub active_by_ship: std::collections::HashMap<i64, (i64, u64)>,
}

pub fn load_clone_vat_bay_state(snap: &eve_db::WorldStateSnapshot) -> CloneVatBayState {
    snap.service_state_blobs
        .get(CLONE_VAT_BAY_STATE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn save_clone_vat_bay_state(snap: &mut eve_db::WorldStateSnapshot, st: &CloneVatBayState) {
    if let Ok(raw) = serde_json::to_string(st) {
        snap.service_state_blobs.insert(CLONE_VAT_BAY_STATE_KEY.to_string(), raw);
    }
}

/// Returns `true` iff `host_ship_id` is a Titan (`30`) or Rorqual (`883`) with a currently active Clone Vat Bay cycle.
pub fn is_ship_clone_vat_bay_active(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    host_ship_id: i64,
    now_unix: u64,
) -> bool {
    let valid_hull = snap
        .items
        .get(&host_ship_id)
        .and_then(|s| ctx.sde.types.get(&s.type_id))
        .is_some_and(|t| matches!(t.group_id, 30 | 883));
    if !valid_hull {
        return false;
    }
    let st = load_clone_vat_bay_state(snap);
    if let Some(&(mod_id, until_unix)) = st.active_by_ship.get(&host_ship_id) {
        return now_unix < until_unix
            && snap.module_online_states.get(&mod_id).copied().unwrap_or(true)
            && snap.module_active_states.contains(&mod_id);
    }
    false
}

// parity: feat:modules.clone-vat-bay-fuel
/// Activates a Clone Vat Bay (`group_id == 815`) on a Titan (`30`) or Rorqual (`883`),
/// consuming Liquid Ozone once from fuel bay/cargo and immobilizing the ship (`feat:modules.clone-vat-bay-fuel`).
pub async fn activate_clone_vat_bay(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let (ship_type_id, mod_type_id) = ctx.db.with_snapshot(|s| {
        (
            s.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0),
            s.items.get(&module_id).map(|i| i.type_id).unwrap_or(0),
        )
    });
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| BridgeRpcError {
        code: "ShipNotFound".into(),
        message: "Ship type unknown".into(),
    })?;
    if !matches!(ship_type.group_id, 30 | 883) {
        return Err(BridgeRpcError {
            code: "ShipCannotFitModule".into(),
            message: "Clone Vat Bay requires a Titan or Rorqual".into(),
        });
    }
    if sys.get_combat_state(ship_id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
        return Err(BridgeRpcError {
            code: "ShipCloaked".into(),
            message: "Cannot activate Clone Vat Bay while cloaked".into(),
        });
    }
    let mod_type = ctx.sde.types.get(&mod_type_id).ok_or_else(|| BridgeRpcError {
        code: "ModuleNotFound".into(),
        message: "Module type unknown".into(),
    })?;
    let (duration_ms, fuel_qty) = if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let ev = fit.evaluated_context(&ctx.sde);
        let m = ev.ship_located_items.iter().find(|m| m.item_id == module_id);
        let d = m.map(|m| m.get_attribute(eve_dogma::attr_ids::DURATION, &ctx.sde)).unwrap_or(0.0);
        let q = m.map(|m| m.get_attribute(714, &ctx.sde)).unwrap_or(0.0);
        (
            if d > 0.0 { d } else { mod_type.default_attributes.get(&73).copied().unwrap_or(300_000.0) },
            if q > 0.0 { q } else { mod_type.default_attributes.get(&714).copied().unwrap_or(200.0) },
        )
    } else {
        (
            mod_type.default_attributes.get(&73).copied().unwrap_or(300_000.0),
            mod_type.default_attributes.get(&714).copied().unwrap_or(200.0),
        )
    };
    let fuel_type = mod_type
        .default_attributes
        .get(&713)
        .copied()
        .map(|v| v as u32)
        .filter(|&v| v > 0)
        .unwrap_or(16_273);
    let needed = fuel_qty.max(1.0).ceil() as i32;

    let consumed = ctx.db.try_with_mut_snapshot(|s| -> Result<(), BridgeRpcError> {
        let mut stacks: Vec<(i64, InventoryFlag, i32)> = s
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && i.type_id == fuel_type
                    && i.quantity > 0
                    && matches!(i.flag, InventoryFlag::SpecializedFuelBay | InventoryFlag::Cargo)
            })
            .map(|i| (i.item_id, i.flag, i.quantity))
            .collect();
        stacks.sort_by_key(|&(id, flg, _)| (if flg == InventoryFlag::SpecializedFuelBay { 0 } else { 1 }, id));
        let have: i32 = stacks.iter().map(|x| x.2).sum();
        if have < needed {
            return Err(BridgeRpcError {
                code: "NotEnoughFuel".into(),
                message: format!("Clone Vat Bay requires {needed} x type {fuel_type}, ship holds {have}"),
            });
        }
        let mut rem = needed;
        for (iid, _, qty) in stacks {
            if rem <= 0 {
                break;
            }
            let take = rem.min(qty);
            rem -= take;
            if take >= qty {
                s.items.remove(&iid);
            } else if let Some(it) = s.items.get_mut(&iid) {
                it.quantity -= take;
            }
        }
        Ok(())
    });
    consumed?;

    let duration_s = (duration_ms / 1000.0).max(1.0);
    let now_unix = ctx.now_unix();
    let until_unix = now_unix.saturating_add(duration_s.ceil() as u64);
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id,
        module_type_id: mod_type.type_id,
        owner_char_id: Some(char_id),
        effect_name: "effects.CloneVatBay".to_string(),
        repeat: 1,
        duration_s,
        immobilize_self: true,
        ..Default::default()
    })
    .await
    .map_err(|e| BridgeRpcError { code: "ModuleActivationFailed".into(), message: e })?;

    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(module_id, true);
        s.module_active_states.insert(module_id);
        let mut st = load_clone_vat_bay_state(s);
        st.active_by_ship.insert(ship_id, (module_id, until_unix));
        save_clone_vat_bay_state(s, &st);
    });

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("active", PyRep::Bool(true)),
        ("cloneVatBayActive", PyRep::Bool(true)),
        ("fuelTypeID", PyRep::Integer(fuel_type as i64)),
        ("fuelConsumed", PyRep::Integer(needed as i64)),
        ("durationSeconds", PyRep::Real(duration_s)),
        ("activeUntilUnix", PyRep::Integer(until_unix as i64)),
    ]))
}
