//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

// parity: plan:E-5
// parity: feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept
// parity: feat:pve.mission-complete, feat:pve.mission-decline, feat:pve.mission-quit
// parity: feat:pve.mission-expire, feat:pve.mission-journal, feat:pve.mission-tracker
// parity: feat:pve.mission-standing-gate, feat:pve.mission-faction-standing-loss
// parity: feat:pve.mission-lp-isk-reward-formula, feat:pve.mission-bonus-timer
// parity: feat:pve.mission-offer-expiry-no-penalty, feat:pve.mission-level-clone-and-space-restrictions
// parity: feat:pve.research-agent-daily-mission

#![forbid(unsafe_code)]

#[path = "agents_standings_arcs.rs"]
pub mod agents_standings_arcs;
pub use agents_standings_arcs::*;

#[path = "agents_missions_helpers.rs"]
pub mod agents_missions_helpers;
pub use agents_missions_helpers::*;

#[path = "agents_finder_query.rs"]
mod agents_finder_query;

#[path = "agents_missions_warp.rs"]
pub mod agents_missions_warp;
pub(crate) use agents_missions_warp::*;

#[path = "agents_missions_offer.rs"]
mod agents_missions_offer;

#[path = "agents_missions_accept.rs"]
mod agents_missions_accept;

#[path = "agents_missions_complete.rs"]
mod agents_missions_complete;

use crate::router::notify_registry::{OnAgentMissionChange, OnAgentMissionObjectivesFailed};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::agents_mission_delivery;
use eve_db::{MissionInstance, MissionKind, MissionState};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_with_bound(ctx, session, None, service, method, args, _kwargs).await
}

async fn is_agent_colocated(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: u32,
    agent_info: &eve_sde::AgentInfo,
) -> bool {
    if agent_info.solar_system_id != Some(sys_id) {
        return false;
    }
    if let Some(ag_st) = agent_info.station_id {
        return session.station_id == Some(ag_st);
    }
    // In-space agent (`station_id == None`): pilot must be undocked in the same solar system
    // and within 2,500 m of the agent ball when the agent is spawned in space.
    if session.station_id.is_some() || session.structure_id.is_some() {
        return false;
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some((_, ag_ball))) = handle.get_ball_snapshot(agent_info.agent_id).await {
        let Some(ship_id) = session.ship_id else {
            return false;
        };
        let Ok(Some((_, ship_ball))) = handle.get_ball_snapshot(ship_id).await else {
            return false;
        };
        if ship_ball.position.distance(ag_ball.position) > 2_500.0 {
            return false;
        }
    }
    true
}


async fn handle_agent_warp_to_location(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let db_docked = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .is_some_and(|c| c.station_id.is_some() || c.structure_id.is_some())
    });
    if session.station_id.is_some() || session.structure_id.is_some() || db_docked {
        return Err(AgentWarpError::ShipDocked.into_rpc("Cannot initiate agent warp while docked",
        ));
    }
    let ship_id = session.ship_id.ok_or_else(|| {
        AgentWarpError::NoActiveShip.into_rpc("Session has no active ship in space")
    })?;
    let sys_id = session
        .solar_system_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.solar_system_id))
        })
        .ok_or_else(|| {
            AgentWarpError::NoSolarSystem.into_rpc("Session has no active solar system")
        })?;
    let parsed = parse_agent_warp_args(method, args, kwargs)?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, current_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| {
            AgentWarpError::DestinyQueryFailed.into_rpc(format!("Failed to query active ship ball {ship_id}: {e}"),
            )
        })?
        .ok_or_else(|| {
            AgentWarpError::ShipNotInSpace.into_rpc(format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
            )
        })?;

    let target = resolve_agent_warp_target(
        ctx,
        session,
        &sys_handle,
        char_id,
        sys_id,
        bound_entry,
        method,
        &parsed,
        kwargs,
    )
    .await?;
    let target_sys_id = target.solar_system_id();
    if target_sys_id != sys_id {
        return Err(AgentWarpError::CrossSystemWarp.into_rpc(format!("Target is in solar system {target_sys_id}, while pilot is in {sys_id}"),
        ));
    }

    let landing_pos = target.landing_position(ctx, current_ball.position);

    let delta = landing_pos - current_ball.position;
    let raw_dest_pos = if parsed.warp_range > 0.0 && delta.length_squared() > 1.0 {
        landing_pos - delta.normalize() * parsed.warp_range
    } else {
        landing_pos
    };

    let warp_dist_m = current_ball.position.distance(raw_dest_pos);
    if !warp_dist_m.is_finite() {
        return Err(AgentWarpError::InvalidArgument.into_rpc("Destination distance must be finite",
        ));
    }
    let is_subwarp_goto = parsed.is_goto && warp_dist_m < MIN_AGENT_WARP_DISTANCE_M;
    let (leave_entries, landing_positions): (Vec<(i64, [f64; 3], [f64; 3])>, Vec<(i64, glam::DVec3)>) =
        if is_subwarp_goto {
        crate::services::tourney_service::check_tourney_move_restricted(ctx, sys_id, char_id)?;
        if matches!(current_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
            return Err(AgentWarpError::ShipAlreadyInWarp.into_rpc("Ship is already in warp",
            ));
        }
        if crate::services::tactical_modules::has_lance_caster_lock(ctx, ship_id) {
            return Err(AgentWarpError::CantMoveWhileModuleActive.into_rpc("Ship is immobilized while superweapon warmup or 5-minute Lance caster lock is active",
            ));
        }
        if current_ball.max_velocity <= 1e-9
            && (crate::services::space_navigation_advanced::check_immobilizing_module_active(
                ctx, ship_id, "maneuver",
            )
            .is_err()
                || crate::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active(
                    ctx, ship_id,
                ))
        {
            return Err(AgentWarpError::CantMoveWhileModuleActive.into_rpc("Ship is immobilized while superweapon warmup, siege/bastion, or cynosural field is active",
            ));
        }
        crate::services::space_destiny::space_nav_travel::ensure_not_mjd_spooling(
            &sys_handle,
            ship_id,
        )
        .await?;
        crate::services::nav_warp_interdiction::execute_subwarp_goto_point(
            ctx,
            &sys_handle,
            char_id,
            ship_id,
            sys_id,
            &current_ball,
            raw_dest_pos,
        )
        .await?;
        (Vec::new(), vec![(char_id, raw_dest_pos)])
    } else if parsed.fleet {
        let mut fleet_kwargs = HashMap::new();
        for key in ["formationID", "formationSpacing", "formationSize"] {
            if let Some(v) = kwargs.get(key) {
                fleet_kwargs.insert(key.to_string(), v.clone());
            }
        }
        let formation_arg = match args {
            [_, _, _, _, _, a5, ..] => Some(a5),
            _ => None,
        };
        if let Some(PyRep::Dict(entries)) =
            formation_arg.or_else(|| kwargs.get("formationSettings"))
        {
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    if matches!(ks, "formationID" | "formationSpacing" | "formationSize") {
                        fleet_kwargs
                            .entry(ks.to_string())
                            .or_insert_with(|| v.clone());
                    }
                }
            }
        }
        let fleet_args = [
            PyRep::Real(landing_pos.x),
            PyRep::Real(landing_pos.y),
            PyRep::Real(landing_pos.z),
            PyRep::Real(parsed.warp_range),
        ];
        let fleet_reply = match crate::services::fleet_social::fleet_social_fleet_ops::handle_fleet_warp(
            ctx,
            session,
            &fleet_args,
            &fleet_kwargs,
        )
        .await
        {
            Some(res) => res?,
            None => return Ok(PyRep::Bool(false)),
        };
        let (warp_count, landing_positions) = extract_fleet_warp_outcome(&fleet_reply);
        if warp_count <= 0 {
            return Ok(PyRep::Bool(false));
        }
        // `handle_fleet_warp` already ran `on_warp_initiated_hooks` and
        // `on_warp_start_leave_dungeon_if_needed` for every warped fleet member.
        (Vec::new(), landing_positions)
    } else {
        if warp_dist_m < MIN_AGENT_WARP_DISTANCE_M {
            return Err(BridgeRpcError::warp_distance_too_short(format!(
                "Warp vector distance {warp_dist_m:.1} m is below minimum {MIN_AGENT_WARP_DISTANCE_M:.0} m"
            )));
        }
        crate::services::nav_warp_interdiction::validate_pre_warp_common(
            ctx,
            char_id,
            ship_id,
            sys_id,
            &sys_handle,
        )
        .await?;
        let cs_opt = sys_handle.get_combat_state(ship_id).await.ok().flatten();
        let cs_non_covert_cloak = cs_opt.as_ref().is_some_and(|cs| {
            !cs.is_covert_cloak
                && ((cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
                    || cs.cloak_velocity_multiplier.is_some())
        });
        let db_non_covert_cloak = ctx.db.with_snapshot(|s| {
            s.items.values().any(|it| {
                it.location_id == ship_id
                    && s.module_active_states.contains(&it.item_id)
                    && ctx.sde.types.get(&it.type_id).is_some_and(|t| {
                        t.group_id == 330
                            && !crate::services::inventory_fitting::fitting_module_cycles::is_warp_safe_cloak_module(ctx, t)
                    })
            })
        });
        if cs_non_covert_cloak || db_non_covert_cloak {
            return Err(AgentWarpError::CloakedCannotWarp.into_rpc("Cannot initiate agent warp while actively cloaked",
            ));
        }
        ctx.validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::AgencyWarp { sys_id },
        )
        .await?;

        crate::services::nav_warp_interdiction::execute_agency_content_warp(
            ctx,
            &sys_handle,
            char_id,
            ship_id,
            sys_id,
            &current_ball,
            raw_dest_pos,
        )
        .await?;
        let adjusted_dest = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .and_then(|(_, b)| match b.mode {
                MovementMode::Warp { destination, .. } => Some(destination),
                _ => None,
            })
            .unwrap_or(raw_dest_pos);
        (
            vec![(
                char_id,
                current_ball.position.to_array(),
                adjusted_dest.to_array(),
            )],
            vec![(char_id, adjusted_dest)],
        )
    };

    match target {
        WarpTarget::Dungeon {
            mission_id,
            anchor_pos,
            ..
        } => {
            if landing_positions
                .iter()
                .any(|&(_, pos)| pos.distance(anchor_pos) <= 200_000.0)
            {
                reconcile_stale_mission_npcs(ctx, &sys_handle, char_id, mission_id);
                let _ = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
                    ctx,
                    char_id,
                    sys_id,
                    anchor_pos,
                    Some(mission_id),
                );
                agents_missions_helpers::ensure_mission_asteroid_spawned_with_handle(
                    ctx,
                    &sys_handle,
                    char_id,
                    sys_id,
                    Some(mission_id),
                )
                .await;
            }
        }
        WarpTarget::Bookmark { bookmark_id, .. } => {
            agents_missions_warp::materialize_mission_pocket_on_fleet_warp(
                ctx,
                &sys_handle,
                char_id,
                sys_id,
                "bookmark",
                bookmark_id,
                &landing_positions,
            )
            .await;
        }
        WarpTarget::StationOrCelestial { .. }
        | WarpTarget::AgentInSpace { .. }
        | WarpTarget::SpacePoint { .. } => {}
    }

    for (warped_cid, start_pos, dest_pos) in leave_entries {
        crate::services::pve_dungeon_runtime::on_warp_start_leave_dungeon_if_needed(
            ctx,
            warped_cid,
            sys_id,
            start_pos,
            dest_pos,
        );
    }

    Ok(PyRep::Bool(true))
}

fn epic_arc_required_missions(ctx: &ServerContext, fac_id: i64) -> usize {
    ctx.sde
        .epic_arcs
        .values()
        .filter(|ea| ea.faction_id == Some(fac_id as u32))
        .min_by_key(|ea| ea.epic_arc_id)
        .and_then(|ea| (ea.missions_count > 0).then_some(ea.missions_count as usize))
        .unwrap_or(3)
        .max(2)
}

/// Clears stale `spawned_npc_ball_ids` when none of a mission's previously spawned NPCs
/// remain alive in the target system's ballpark (e.g., after system eviction/restart).
pub(super) fn reconcile_stale_mission_npcs(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    mission_id: u32,
) {
    let stale_ids = ctx.db.with_snapshot(|snap| {
        snap.missions
            .get_by_character_template(char_id, mission_id)
            .filter(|m| m.npcs_remaining() > 0 && !m.spawned_npc_ball_ids().is_empty())
            .map(|m| m.spawned_npc_ball_ids().to_vec())
    });
    if let Some(npc_ids) = stale_ids {
        let any_alive = npc_ids
            .iter()
            .any(|&npc_id| sys_handle.get_ball_position_sync(npc_id).is_some());
        if !any_alive {
            ctx.db.with_mut_snapshot(|snap| {
                let _ = snap
                    .missions
                    .update_by_character_template(char_id, mission_id, |m| {
                        if let Some(enc) = m.encounter_mut() {
                            enc.spawned_npc_ball_ids.clear();
                        }
                    });
            });
        }
    }
}

fn extract_fleet_warp_outcome(fleet_reply: &PyRep) -> (i64, Vec<(i64, glam::DVec3)>) {
    let PyRep::KeyVal(kv) = fleet_reply else {
        return (0, Vec::new());
    };
    let count = kv.get("warpCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let mut positions = Vec::new();
    if let Some(PyRep::List(entries)) = kv.get("landingPositions") {
        for entry in entries {
            if let PyRep::Tuple(elems) = entry {
                if let [cid, x, y, z] = elems.as_slice() {
                    if let (Some(member_char_id), Some(x), Some(y), Some(z)) =
                        (cid.as_i64(), x.as_f64(), y.as_f64(), z.as_f64())
                    {
                        positions.push((member_char_id, glam::DVec3::new(x, y, z)));
                    }
                }
            }
        }
    }
    (count, positions)
}

/// Reclaims mission-scoped courier cargo, removes any mission bookmark and persisted mining
/// asteroid, and archives the character's mission instance into persistent history with terminal
/// `new_state`, returning `(solar_system_id, asteroid_entity_id, spawned_npc_ball_ids)` for
/// optional post-commit live-system despawn.
fn finalize_mission_state_cleanup(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
    new_state: MissionState,
    zero_npcs: bool,
    ended_at_unix: u64,
) -> (u32, Option<i64>, Vec<i64>) {
    let mut bmid = None;
    let mut m_sys_id = 0_u32;
    let mut ast_id = None;
    let mut ball_ids = Vec::new();
    let _ = snap.missions
        .update_by_character_template(char_id, mission_id, |m| {
            m_sys_id = m.solar_system_id;
            ast_id = m.asteroid_entity_id.take();
            ball_ids = m.spawned_npc_ball_ids().to_vec();
            if zero_npcs {
                if let Some(enc) = m.encounter_mut() {
                    enc.npcs_remaining = 0;
                    enc.spawned_npc_ball_ids.clear();
                }
            }
            bmid = m.bookmark_id.take();
        });
    if let Some(aid) = ast_id {
        snap.asteroids.remove(&aid);
    }
    if let Some(bmid) = bmid {
        snap.bookmarks.remove(&bmid);
    }
    reclaim_courier_cargo(snap, char_id, mission_id);
    let _ = snap
        .missions
        .archive_by_character_template(char_id, mission_id, new_state, ended_at_unix);
    (m_sys_id, ast_id, ball_ids)
}

/// Formats concise mission objective text consistent with persisted `MissionInstance` fields (`PVE-012`).
pub(super) fn format_mission_objective_text(
    ctx: &ServerContext,
    m: &MissionInstance,
) -> String {
    let req_pair = m.required_delivery();
    let req_tid_opt = req_pair.map(|(tid, _)| tid);
    let req_qty = req_pair.map_or(0_i64, |(_, q)| i64::from(q));
    let agent_lvl = ctx.sde.agents.get(&m.agent_id).map_or(1, |a| a.level);
    let tmpl = ctx.sde.missions.get(&m.template_id);
    match m.kind {
        MissionKind::ResearchTurnIn => {
            let item_tid = req_tid_opt.unwrap_or(34);
            let item_name = ctx
                .sde
                .types
                .get(&item_tid)
                .map(|t| t.name.as_str())
                .unwrap_or("Tritanium");
            let qty = if req_qty > 0 {
                req_qty
            } else {
                i64::from(rnd_tritanium_required_qty(agent_lvl))
            };
            format!("Deliver {qty} units of {item_name} to agent station hangar.")
        }
        MissionKind::DataCenterTurnIn => {
            let tag_tid = req_tid_opt.unwrap_or(17202);
            let tag_name = ctx
                .sde
                .types
                .get(&tag_tid)
                .map(|t| t.name.as_str())
                .unwrap_or("Pirate Tag");
            let qty = if req_qty > 0 { req_qty } else { 1 };
            if qty == 1 {
                format!("Deliver 1 {tag_name} to agent station hangar.")
            } else {
                format!("Deliver {qty} units of {tag_name} to agent station hangar.")
            }
        }
        MissionKind::Heraldry => {
            let derived = (req_tid_opt.is_none() || req_qty <= 0)
                .then(|| tmpl.map(|t| resolve_heraldry_mission_details(ctx, t)))
                .flatten();
            let ship_tid = req_tid_opt
                .or_else(|| derived.map(|d| d.ship_type_id))
                .unwrap_or(587);
            let ship_name = ctx
                .sde
                .types
                .get(&ship_tid)
                .map(|t| t.name.as_str())
                .unwrap_or("ship hull");
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.map_or(1, |d| i64::from(d.quantity))
            };
            format!("Deliver {qty}x {ship_name} (packaged) to the Paragon station hangar.")
        }
        MissionKind::Mining => {
            let derived = (req_tid_opt.is_none() || req_qty <= 0)
                .then(|| tmpl.map(|t| resolve_mining_mission_details(ctx, t)))
                .flatten();
            let ore_tid = req_tid_opt
                .or_else(|| derived.as_ref().map(|d| d.ore_type_id))
                .unwrap_or(1230);
            let ore_name = ctx
                .sde
                .types
                .get(&ore_tid)
                .map(|t| t.name.as_str())
                .unwrap_or("ore");
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.map_or(500, |d| i64::from(d.required_units))
            };
            format!("Mine {qty} units of {ore_name} and deliver to the agent station hangar.")
        }
        MissionKind::Courier => {
            let inst_dest = m.destination_station_id();
            let derived = (req_tid_opt.is_none() || req_qty <= 0 || inst_dest.is_none())
                .then(|| {
                    tmpl.and_then(|t| {
                        ctx.sde
                            .agents
                            .get(&m.agent_id)
                            .and_then(|a| resolve_courier_mission_details(ctx, t, a).ok())
                    })
                })
                .flatten();
            let cargo_tid = req_tid_opt
                .or_else(|| derived.as_ref().map(|d| d.cargo_type_id))
                .unwrap_or(3683);
            let cargo_name = ctx
                .sde
                .types
                .get(&cargo_tid)
                .map(|t| t.name.as_str())
                .unwrap_or("cargo");
            let dest_name = inst_dest
                .or_else(|| derived.as_ref().map(|d| d.destination_station_id))
                .and_then(|sid| ctx.sde.stations.get(&sid))
                .map(|s| s.name.as_str())
                .unwrap_or("destination station");
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.as_ref().map_or(10, |d| i64::from(d.cargo_quantity))
            };
            format!("Transport {qty} units of {cargo_name} to {dest_name}.")
        }
        MissionKind::Trade => {
            let derived = (req_tid_opt.is_none() || req_qty <= 0)
                .then(|| tmpl.map(|t| resolve_trade_mission_details(ctx, t)))
                .flatten();
            let trade_tid = req_tid_opt
                .or_else(|| derived.as_ref().map(|d| d.trade_type_id))
                .unwrap_or(43);
            let trade_name = ctx
                .sde
                .types
                .get(&trade_tid)
                .map(|t| t.name.as_str())
                .unwrap_or("trade goods");
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.map_or(20, |d| i64::from(d.trade_quantity))
            };
            format!("Acquire and deliver {qty} units of {trade_name} to the agent station hangar.")
        }
        MissionKind::Encounter => {
            format!(
                "Eliminate hostile pirate vessels ({} targets remaining) and report back.",
                m.npcs_remaining()
            )
        }
    }
}

/// Resolves mission-specific briefing text from SDE templates, agent/corp/system/station
/// records, dungeon encounters, and active mission delivery metadata (never falling back to
/// the legacy generic `"Execute objectives in system"` sentence). Must remain DB-lock-free
/// because callers also invoke it inside `ctx.db.with_snapshot`.
fn build_mission_briefing_text(
    ctx: &ServerContext,
    agent_id: i64,
    mission_id: u32,
    title: &str,
    sys_id: u32,
    inst: Option<&MissionInstance>,
) -> String {
    let agent_opt = ctx.sde.agents.get(&agent_id);
    let agent_label = agent_opt
        .map(|a| a.name.as_str())
        .filter(|n| !n.is_empty())
        .map(String::from)
        .unwrap_or_else(|| format!("Agent {agent_id}"));
    let corp_name = agent_opt
        .and_then(|a| ctx.sde.npc_corporations.get(&a.corporation_id))
        .map(|c| c.name.as_str())
        .filter(|n| !n.is_empty());
    let issuer = match corp_name {
        Some(corp) => format!("{agent_label} ({corp})"),
        None => agent_label.clone(),
    };
    let sys_label = |sid: u32| -> String {
        ctx.sde
            .solar_systems
            .get(&sid)
            .map(|s| s.name.as_str())
            .filter(|n| !n.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("system {sid}"))
    };
    let sys_name = sys_label(sys_id);
    let origin_sid = inst
        .and_then(|i| i.origin_station_id)
        .or_else(|| agent_opt.and_then(|a| a.station_id));
    let origin_station = origin_sid.and_then(|sid| ctx.sde.stations.get(&sid));
    let origin_sys_id = origin_station.map_or(sys_id, |st| st.solar_system_id);
    let origin_sys_name = if origin_sys_id == sys_id {
        sys_name.clone()
    } else {
        sys_label(origin_sys_id)
    };
    let dropoff = origin_station
        .map(|st| st.name.as_str())
        .filter(|n| !n.is_empty())
        .map(String::from)
        .unwrap_or_else(|| origin_sys_name.clone());

    let tmpl_opt = ctx.sde.missions.get(&mission_id);
    let ct = tmpl_opt
        .and_then(|t| t.content_template.as_deref())
        .unwrap_or_default()
        .trim();

    if !ct.is_empty() && !ct.starts_with("agent.missionTemplatizedContent_") {
        return if title.is_empty() || ct.contains(title) {
            ct.to_string()
        } else {
            format!("{title}: {ct}")
        };
    }

    let is_epic_arc =
        agent_opt.is_some_and(|a| a.agent_type_id == 10) || ct.contains("EpicArc");
    let arc_name = is_epic_arc.then(|| {
        let (arc_id, resolved_name) =
            agents_standings_arcs::resolve_v24_epic_arc_for_mission(ctx, agent_id, mission_id);
        resolved_name.unwrap_or_else(|| format!("Epic Arc {arc_id}"))
    });
    let item_label = |tid: eve_db::TypeId, fallback: &str| -> String {
        ctx.sde
            .types
            .get(&tid)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| fallback.to_string())
    };
    let kind = inst
        .map(|i| i.kind)
        .unwrap_or_else(|| eve_db::decide_mission_kind(tmpl_opt, agent_opt));
    let req_pair = inst.and_then(|i| i.required_delivery());
    let req_tid = req_pair.map(|(tid, _)| tid);
    let req_qty = req_pair.map_or(0, |(_, q)| q);

    match kind {
        MissionKind::ResearchTurnIn => {
            let tid = req_tid.unwrap_or(34);
            let qty = if req_qty > 0 {
                req_qty
            } else {
                rnd_tritanium_required_qty(agent_opt.map_or(1, |a| a.level))
            };
            let item = item_label(tid, "research material");
            format!(
                "Research assignment \"{title}\" for {issuer}: supply {qty}x {item} to {dropoff} in {origin_sys_name} for active R&D work."
            )
        }
        MissionKind::DataCenterTurnIn => {
            let tid = req_tid.unwrap_or(17202);
            let qty = if req_qty > 0 { req_qty } else { 1 };
            let item = item_label(tid, "Pirate Tag");
            format!(
                "Data Center commission \"{title}\" for {issuer}: turn in {qty}x {item} at {dropoff} in {origin_sys_name}."
            )
        }
        MissionKind::Heraldry => {
            let derived = (req_tid.is_none() || req_qty <= 0)
                .then(|| tmpl_opt.map(|t| resolve_heraldry_mission_details(ctx, t)))
                .flatten();
            let tid = req_tid
                .or_else(|| derived.map(|d| d.ship_type_id))
                .unwrap_or(587);
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.map_or(1, |d| d.quantity)
            };
            let item = item_label(tid, "ship hull");
            format!(
                "Heraldry commission \"{title}\" for {issuer}: deliver {qty}x {item} (packaged) to the Paragon station hangar at {dropoff} in {origin_sys_name}."
            )
        }
        MissionKind::Trade => {
            let derived = (req_tid.is_none() || req_qty <= 0)
                .then(|| tmpl_opt.map(|t| resolve_trade_mission_details(ctx, t)))
                .flatten();
            let tid = req_tid
                .or_else(|| derived.as_ref().map(|d| d.trade_type_id))
                .unwrap_or(43);
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.map_or(20, |d| d.trade_quantity)
            };
            let item = item_label(tid, "trade goods");
            format!(
                "Trade order \"{title}\" for {issuer}: acquire {qty}x {item} and deliver the shipment to {dropoff} in {origin_sys_name}."
            )
        }
        MissionKind::Courier => {
            let inst_dest = inst.and_then(|i| i.destination_station_id());
            let derived = (req_tid.is_none() || req_qty <= 0 || inst_dest.is_none())
                .then(|| {
                    tmpl_opt.and_then(|t| {
                        agent_opt.and_then(|a| resolve_courier_mission_details(ctx, t, a).ok())
                    })
                })
                .flatten();
            let tid = req_tid
                .or_else(|| derived.as_ref().map(|d| d.cargo_type_id))
                .unwrap_or(3814);
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.as_ref().map_or(10, |d| d.cargo_quantity)
            };
            let cargo = item_label(tid, "diplomatic cargo");
            let dest_sid = inst_dest.or_else(|| derived.as_ref().map(|d| d.destination_station_id));
            let dest_station = dest_sid
                .and_then(|sid| ctx.sde.stations.get(&sid))
                .map(|st| {
                    let dsys = ctx
                        .sde
                        .solar_systems
                        .get(&st.solar_system_id)
                        .map(|s| s.name.as_str())
                        .unwrap_or("destination system");
                    format!("{} ({dsys})", st.name)
                })
                .unwrap_or_else(|| match dest_sid {
                    Some(sid) => format!("station {sid}"),
                    None => dropoff.clone(),
                });
            format!(
                "Courier contract \"{title}\" for {issuer}: transport {qty}x {cargo} from {dropoff} ({origin_sys_name}) to {dest_station}."
            )
        }
        MissionKind::Mining => {
            let derived = (req_tid.is_none() || req_qty <= 0)
                .then(|| tmpl_opt.map(|t| resolve_mining_mission_details(ctx, t)))
                .flatten();
            let ore_tid = req_tid
                .or_else(|| derived.as_ref().map(|d| d.ore_type_id))
                .unwrap_or(1230);
            let qty = if req_qty > 0 {
                req_qty
            } else {
                derived.map_or(500, |d| d.required_units)
            };
            let ore_name = item_label(ore_tid, "ore");
            format!(
                "Mining operation \"{title}\" for {issuer}: harvest {qty} units of {ore_name} in {sys_name} and haul the ore to {dropoff}."
            )
        }
        MissionKind::Encounter => {
            if ct.contains("TalkToAgent") || ct.contains("AgentInteraction") {
                let arc_prefix = match arc_name.as_deref() {
                    Some(n) => format!("Epic Arc ({n})"),
                    None => agent_opt
                        .map(|a| format!("Level {} mission", a.level.max(1)))
                        .unwrap_or_else(|| "Standard mission".into()),
                };
                return format!(
                    "{arc_prefix} liaison \"{title}\" for {issuer}: proceed to {dropoff} in {origin_sys_name} and confer with your designated contact."
                );
            }
            if let (Some(tmpl), Some(agent)) = (tmpl_opt, agent_opt) {
                let enc = resolve_security_mission_encounter(ctx, tmpl, agent, sys_id);
                let arc_prefix = arc_name
                    .as_deref()
                    .map(|n| format!("[{n}] "))
                    .unwrap_or_default();
                let burner_prefix = if inst.is_some_and(|i| i.is_anomic_burner) {
                    "[Anomic Burner] "
                } else {
                    ""
                };
                format!("{arc_prefix}{burner_prefix}{}", enc.briefing_text)
            } else {
                let dungeon_name = tmpl_opt
                    .and_then(|t| t.dungeon_id)
                    .or_else(|| {
                        ctx.sde
                            .dungeons
                            .contains_key(&mission_id)
                            .then_some(mission_id)
                    })
                    .and_then(|did| ctx.sde.dungeons.get(&did))
                    .map(|d| d.name.as_str());
                match dungeon_name {
                    Some(dname) => format!(
                        "Mission \"{title}\" for {issuer}: investigate {dname} in {sys_name}, neutralize hostile forces, and return to {dropoff}."
                    ),
                    None => format!(
                        "Mission \"{title}\" for {issuer} in {sys_name}: neutralize designated targets and report back to {dropoff}."
                    ),
                }
            }
        }
    }
}

pub async fn handle_with_bound(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = agents_standings_arcs::handle_extended_with_bound(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        _kwargs,
    )
    .await
    {
        return Some(res);
    }

    match (service, method) {
        ("agentMgr", "GetAgents") => {
            Some(agents_finder_query::handle_get_agents(ctx, session, args, _kwargs))
        }

        ("agentMgr", "GetInfoServiceDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, _) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            let mut services = vec![PyRep::keyval([("agentServiceType", PyRep::String("mission".into()))])];
            if a.is_locator {
                services.push(PyRep::keyval([("agentServiceType", PyRep::String("locator".into()))]));
            }
            if a.agent_type_id == 4 {
                services.push(PyRep::keyval([("agentServiceType", PyRep::String("research".into()))]));
            }
            Some(Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(a.agent_id)),
                ("agentTypeID", PyRep::Integer(a.agent_type_id as i64)),
                ("divisionID", PyRep::Integer(a.division_id as i64)),
                ("corporationID", PyRep::Integer(a.corporation_id)),
                ("solarSystemID", a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)),
                ("stationID", a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("level", PyRep::Integer(a.level as i64)),
                ("quality", PyRep::Integer(20)),
                ("isLocator", PyRep::Bool(a.is_locator)),
                ("incompatible", PyRep::None),
                ("services", PyRep::List(services)),
            ])))
        }

        ("agentMgr", "GetAgentLocationWrap") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, _) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            let loc_id = a
                .station_id
                .or_else(|| a.solar_system_id.map(|s| s as i64))
                .unwrap_or(0);
            let type_id = a
                .station_id
                .and_then(|sid| ctx.sde.stations.get(&sid))
                .map(|st| st.station_type_id as i64)
                .unwrap_or(5);
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("agentID".into()), PyRep::Integer(a.agent_id)),
                (
                    PyRep::String("solarsystemID".into()),
                    a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None),
                ),
                (PyRep::String("locationID".into()), PyRep::Integer(loc_id)),
                (PyRep::String("typeID".into()), PyRep::Integer(type_id)),
                (
                    PyRep::String("stationID".into()),
                    a.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
            ])))
        }

        ("agentMgr", "GetDungeonShipRestrictions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let restricted = ctx.db.with_snapshot(|snap| {
                let _ = snap.characters.get(&char_id);
                Vec::<PyRep>::new()
            });
            Some(Ok(PyRep::Dict(vec![(
                PyRep::String("restrictedShipTypes".into()),
                PyRep::List(restricted),
            )])))
        }

        ("agentMgr", "GetEntryPoint") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id.or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Character {char_id} has no active solar system"),
                    client_error: None,
                }));
            };
            let bm = ctx.db.with_snapshot(|snap| {
                snap.missions
                    .iter_by_character(char_id)
                    .next()
                    .map(|m| {
                        PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(m.solar_system_id as i64)),
                            ("locationID", PyRep::Integer(m.solar_system_id as i64)),
                            ("bookmarkID", PyRep::Integer(m.bookmark_id.unwrap_or(1))),
                        ])
                    })
                    .unwrap_or_else(|| {
                        PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(sys_id as i64)),
                            ("locationID", PyRep::Integer(sys_id as i64)),
                            ("bookmarkID", PyRep::Integer(1)),
                        ])
                    })
            });
            Some(Ok(bm))
        }

        ("agentMgr", "GetMissionKeywords") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let kw_entries = ctx.db.with_snapshot(|snap| {
                let _ = snap.characters.get(&char_id);
                Vec::<(PyRep, PyRep)>::new()
            });
            Some(Ok(PyRep::Dict(kw_entries)))
        }

        ("agentMgr", "GetStandingGainsForMission") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id.or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Character {char_id} has no active solar system"),
                    client_error: None,
                }));
            };
            let (agent_id, _) = resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let corp_id = ctx.sde.agents.get(&agent_id).map_or(1_000_035, |a| a.corporation_id);
            Some(Ok(PyRep::Dict(vec![
                (PyRep::Integer(agent_id), PyRep::Real(0.15)),
                (PyRep::Integer(corp_id), PyRep::Real(0.05)),
            ])))
        }

        ("agentMgr", "IsCheatingWithAgent") | ("agentMgr", "ShouldAlwaysAllowReplay") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let is_gm = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id)) && false;
            Some(Ok(PyRep::Bool(is_gm)))
        }

        ("agentMgr", "GotoLocation")
        | ("agentMgr", "WarpToAgentInSpace")
        | ("agentMgr", "WarpToLocation") => Some(
            handle_agent_warp_to_location(ctx, session, bound_entry, method, args, _kwargs).await,
        ),

        ("agentMgr", "GetSolarSystemOfAgent") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let agent_id = bound_entry
                .filter(|b| b.service == "agentMgr" && b.object_id > 0)
                .map(|b| b.object_id)
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or_else(|| resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs).0);
            let Some(a) = ctx.sde.agents.get(&agent_id) else {
                return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
            };
            Some(Ok(a.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)))
        }

        ("agentMgr", "GetMyJournalDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let bound_agent_id = bound_entry
                .filter(|b| b.service == "agentMgr" && b.object_id > 0)
                .map(|b| b.object_id);
            let now = ctx.now_unix();
            let (
                missions,
                research,
                expired_cleanups,
                expired_standing_changes,
                expired_failed_agents,
            ) = ctx.db.with_mut_snapshot(|snap| {
                // Sweep and transition any expired offers or accepted missions (feat:pve.mission-expiration)
                let expired_updates: Vec<(u32, i64, u32, Option<i64>, MissionState, bool)> = snap
                    .missions
                    .iter_by_character(char_id)
                    .filter_map(|m| {
                        if m.state == MissionState::Offered
                            && m.offer_expiry_unix > 0
                            && now > m.offer_expiry_unix
                        {
                            Some((
                                m.template_id,
                                m.agent_id,
                                m.solar_system_id,
                                m.origin_station_id,
                                MissionState::Expired,
                                false,
                            ))
                        } else if m.state == MissionState::Accepted
                            && m.accept_expiry_unix.is_some_and(|exp| exp > 0 && now > exp)
                        {
                            Some((
                                m.template_id,
                                m.agent_id,
                                m.solar_system_id,
                                m.origin_station_id,
                                MissionState::Failed,
                                true,
                            ))
                        } else {
                            None
                        }
                    })
                    .collect();
                let mut expired_cleanups = Vec::with_capacity(expired_updates.len());
                let mut expired_standing_changes = Vec::new();
                let mut expired_failed_agents = Vec::new();
                let char_station_id = snap.characters.get(&char_id).and_then(|c| c.station_id);
                for (mid, m_agent_id, m_sys_id, m_station_id, target_state, zero_npcs) in
                    expired_updates
                {
                    expired_cleanups.push(finalize_mission_state_cleanup(
                        snap,
                        char_id,
                        mid,
                        target_state,
                        zero_npcs,
                        now,
                    ));
                    if target_state == MissionState::Failed && m_agent_id > 0 {
                        let (corp_id, faction_id) = resolve_agent_corp_and_faction(
                            ctx,
                            char_id,
                            m_agent_id,
                            m_sys_id,
                            m_station_id.or(char_station_id),
                            false,
                        );
                        expired_standing_changes.extend(apply_mission_penalty_standings(
                            ctx,
                            snap,
                            char_id,
                            m_agent_id,
                            corp_id,
                            faction_id,
                            -0.10,
                            -0.10,
                            -0.5,
                            84,
                        ));
                        expired_failed_agents.push(m_agent_id);
                    }
                }

                let mut active: Vec<MissionInstance> = snap
                    .missions
                    .iter_by_character(char_id)
                    .filter(|m| {
                        m.state.is_active()
                            && bound_agent_id.is_none_or(|aid| m.agent_id == aid)
                    })
                    .cloned()
                    .collect();
                active.sort_by_key(|m| m.template_id);
                let mission_list = active
                    .into_iter().map(|m| {
                        let tmpl = ctx.sde.missions.get(&m.template_id);
                        let title = tmpl.map(|t| t.title.clone())
                            .unwrap_or_else(|| format!("Mission {}", m.template_id));
                        let important = tmpl.is_some_and(is_storyline_template);
                        let mission_state = m.state.client_state_num();
                        let exp_unix = if m.state == MissionState::Accepted {
                            m.accept_expiry_unix.unwrap_or(now + 86_400)
                        } else if m.offer_expiry_unix > 0 {
                            m.offer_expiry_unix
                        } else {
                            now + 86_400
                        };
                        let bm_hint = m.kind.journal_mission_type_hint();
                        let matching_bms = mission_journal_bookmarks(snap, char_id, &m);
                        let bm_loc_type = m.kind.journal_location_type();
                        let bm_created_unix = m
                            .accepted_at_unix
                            .or((m.offered_at_unix > 0).then_some(m.offered_at_unix))
                            .unwrap_or(now);
                        let bms: Vec<PyRep> = matching_bms
                            .into_iter()
                            .enumerate()
                            .map(|(idx, b)| {
                                let bm_type_id = b
                                    .item_id
                                    .and_then(|eid| {
                                        ctx.sde
                                            .stations
                                            .get(&eid)
                                            .map(|s| s.station_type_id as i64)
                                            .or_else(|| snap.items.get(&eid).map(|i| i.type_id as i64))
                                            .or_else(|| {
                                                snap.structures.get(&eid).map(|st| st.type_id as i64)
                                            })
                                    })
                                    .unwrap_or(5_i64);
                                let is_deadspace_bm =
                                    i64::from(bm_loc_type == "dungeon" && b.item_id.is_none());
                                let bm_sys = if b.solar_system_id > 0 {
                                    b.solar_system_id
                                } else {
                                    m.solar_system_id
                                };
                                let mut bm_fields = vec![
                                    ("bookmarkID", PyRep::Integer(b.bookmark_id)),
                                    ("agentID", PyRep::Integer(m.agent_id)),
                                    ("creatorID", PyRep::None),
                                    ("created", agents_standings_arcs::unix_to_filetime_rep(bm_created_unix)),
                                    ("expiry", PyRep::None),
                                    ("deadspace", PyRep::Integer(is_deadspace_bm)),
                                    ("note", PyRep::String(String::new())),
                                ];
                                if bm_sys > 0 {
                                    bm_fields.push(("solarSystemID", PyRep::Integer(bm_sys as i64)));
                                    bm_fields.push(("solarsystemID", PyRep::Integer(bm_sys as i64)));
                                    bm_fields.push(("locationID", PyRep::Integer(bm_sys as i64)));
                                }
                                bm_fields.extend([
                                    ("locationNumber", PyRep::Integer(idx as i64)),
                                    ("locationType", PyRep::String(bm_loc_type.into())),
                                    (
                                        "itemID",
                                        b.item_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                    ),
                                    ("typeID", PyRep::Integer(bm_type_id)),
                                    ("hint", PyRep::String(bm_hint.into())),
                                    ("label", PyRep::String(b.label.clone())),
                                    ("x", PyRep::Real(b.x)),
                                    ("y", PyRep::Real(b.y)),
                                    ("z", PyRep::Real(b.z)),
                                ]);
                                PyRep::keyval(bm_fields)
                            })
                            .collect();
                        let exp_rep = if exp_unix > 0 {
                            agents_standings_arcs::unix_to_filetime_rep(exp_unix)
                        } else {
                            PyRep::None
                        };
                        PyRep::Tuple(vec![
                            PyRep::Integer(mission_state),
                            PyRep::Bool(important),
                            PyRep::String(bm_hint.into()),
                            PyRep::String(title),
                            PyRep::Integer(m.agent_id),
                            exp_rep,
                            PyRep::List(bms),
                            PyRep::Bool(false),
                            PyRep::Bool(false),
                            PyRep::Integer(m.template_id as i64),
                        ])
                    })
                    .collect::<Vec<PyRep>>();

                let research_list = load_rnd_research_entries(snap, char_id)
                    .into_iter()
                    .filter(|r| bound_agent_id.is_none_or(|aid| r.agent_id == aid))
                    .map(|r| {
                        let elapsed_days =
                            (now.saturating_sub(r.started_unix) as f64) / 86_400.0;
                        let total_rp = r.initial_points + r.points_per_day * elapsed_days;
                        let agent_lvl = ctx.sde.agents.get(&r.agent_id).map_or(1, |a| a.level as i64);
                        PyRep::keyval([
                            ("agentID", PyRep::Integer(r.agent_id)),
                            ("typeID", PyRep::Integer(r.research_type_id as i64)),
                            ("ppd", PyRep::Real(r.points_per_day)),
                            ("points", PyRep::Real(total_rp)),
                            ("level", PyRep::Integer(agent_lvl)),
                            ("quality", PyRep::Integer(20)),
                        ])
                    })
                    .collect::<Vec<PyRep>>();
                (
                    mission_list,
                    research_list,
                    expired_cleanups,
                    expired_standing_changes,
                    expired_failed_agents,
                )
            });
            notify_standing_changes(ctx, char_id, &expired_standing_changes);
            for agent_id in expired_failed_agents {
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    OnAgentMissionObjectivesFailed { agent_id },
                );
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    OnAgentMissionChange {
                        action: "failed".into(),
                        agent_id,
                    },
                );
            }
            for (clean_sys, ast_id, ball_ids) in expired_cleanups {
                if clean_sys > 0 && (!ball_ids.is_empty() || ast_id.is_some()) {
                    if let Some(handle) = ctx.systems.get_handle(clean_sys) {
                        for bid in ball_ids {
                            let _ = handle.remove_ball(bid).await;
                        }
                        if let Some(aid) = ast_id {
                            let _ = handle.remove_ball(aid).await;
                        }
                    }
                }
            }
            Some(Ok(PyRep::Tuple(vec![PyRep::List(missions), PyRep::List(research)])))
        }

        ("agentMgr", "GetMissionBriefingInfo")
        | ("agentMgr", "GetMissionObjectiveInfo")
        | ("agentMgr", "GetMissionJournalInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, explicit_agent) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
            let mission_id =
                match resolve_target_mission_id(ctx, char_id, lookup_agent_id, None, args, _kwargs) {
                    Ok(v) => v,
                    Err(ref e) if e.code == "MissionNotActive" && method == "GetMissionObjectiveInfo" => {
                        return Some(Ok(PyRep::None));
                    }
                    Err(e) => return Some(Err(e)),
                };
            let not_active_reply = |msg: String| {
                if method == "GetMissionObjectiveInfo" {
                    Ok(PyRep::None)
                } else {
                    Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: msg,
                        client_error: None,
                    })
                }
            };
            let has_explicit_mission_id = args.get(1).and_then(|a| a.as_i64()).is_some()
                || args.first().and_then(|a| a.as_i64()).is_some_and(|v| {
                    v > 19
                        && !ctx.sde.agents.contains_key(&v)
                        && u32::try_from(v).is_ok_and(|mid| {
                            ctx.sde.missions.contains_key(&mid)
                                || ctx.db.with_snapshot(|snap| {
                                    snap.missions.contains_character_template(char_id, mid)
                                        || snap
                                            .missions
                                            .latest_history_for_character_template(char_id, mid)
                                            .is_some()
                                })
                        })
                });
            let allow_history_lookup =
                method == "GetMissionJournalInfo" || has_explicit_mission_id;
            let Some((m, bm_coords)) = ctx.db.with_snapshot(|snap| {
                snap.missions
                    .get_by_character_template(char_id, mission_id)
                    .cloned()
                    .map(|m| {
                        let bm_coords = m
                            .bookmark_id
                            .and_then(|bid| snap.bookmarks.get(&bid))
                            .filter(|b| b.item_id.is_none())
                            .map(|b| (b.solar_system_id, b.x, b.y, b.z));
                        (m, bm_coords)
                    })
                    .or_else(|| {
                        if !allow_history_lookup {
                            return None;
                        }
                        let entry = if explicit_agent {
                            snap.missions
                                .iter_history_by_character(char_id)
                                .filter(|e| e.template_id == mission_id && e.agent_id == agent_id)
                                .max_by_key(|e| (e.ended_at_unix, e.instance_id))
                                .or_else(|| {
                                    let arg_has_agent = args
                                        .iter()
                                        .take(2)
                                        .any(|a| a.as_i64().is_some_and(|v| ctx.sde.agents.contains_key(&v)));
                                    (bound_entry.is_none() && !arg_has_agent)
                                        .then(|| {
                                            snap.missions.latest_history_for_character_template(
                                                char_id, mission_id,
                                            )
                                        })
                                        .flatten()
                                })
                        } else {
                            snap.missions
                                .latest_history_for_character_template(char_id, mission_id)
                        };
                        entry.map(|entry| {
                            (
                                hydrate_mission_from_history_entry(
                                    ctx,
                                    entry,
                                    sys_id,
                                    session.station_id,
                                ),
                                None,
                            )
                        })
                    })
            }) else {
                return Some(not_active_reply(format!(
                    "Mission {mission_id} is not active for character {char_id}"
                )));
            };
            if !allow_history_lookup {
                if (explicit_agent && m.agent_id != agent_id) || !m.state.is_active() {
                    return Some(not_active_reply(format!(
                        "No active Offered/Accepted mission for character {char_id} with agent {agent_id}"
                    )));
                }
                if m.state == MissionState::Offered {
                    if let Some(ref agent_info) = ctx.sde.agents.get(&m.agent_id).cloned() {
                        if !is_agent_colocated(ctx, session, sys_id, agent_info).await {
                            return Some(not_active_reply(format!(
                                "Offered mission {mission_id} with agent {} is not accessible remotely",
                                m.agent_id
                            )));
                        }
                    }
                }
            }
            let tmpl = ctx.sde.missions.get(&mission_id);
            let title = tmpl.map(|t| t.title.clone())
                .unwrap_or_else(|| format!("Mission {mission_id}"));
            let briefing = build_mission_briefing_text(
                ctx,
                m.agent_id,
                mission_id,
                &title,
                m.solar_system_id,
                Some(&m),
            );
            let req_pair = m.required_delivery();
            let req_tid_opt = req_pair.map(|(tid, _)| tid);
            let req_type = req_tid_opt
                .map(|t| PyRep::Integer(t as i64))
                .unwrap_or(PyRep::None);
            let req_qty = req_pair.map_or(0_i64, |(_, q)| i64::from(q));
            let is_data_center = m.kind == MissionKind::DataCenterTurnIn;
            let is_rnd_turnin = m.kind == MissionKind::ResearchTurnIn;
            let is_trade = m.kind == MissionKind::Trade;
            let is_courier = m.kind == MissionKind::Courier;
            let is_mining = m.kind == MissionKind::Mining;
            let is_heraldry = m.kind == MissionKind::Heraldry;
            let agent_lvl = ctx.sde.agents.get(&m.agent_id).map_or(1, |a| a.level);
            let objectives = format_mission_objective_text(ctx, &m);
            let mission_state = m.state.client_state_num();
            let offer_lvl = m
                .offered_level
                .or_else(|| ctx.sde.agents.get(&m.agent_id).map(|a| a.level))
                .unwrap_or(1);
            let now = ctx.now_unix();
            let CharacterMissionRewards {
                isk_reward,
                lp_reward,
                bonus_reward: raw_bonus_reward,
            } = compute_character_mission_rewards(ctx, char_id, m.agent_id, mission_id, sys_id);
            let bonus_window_secs =
                mission_bonus_window_secs(ctx, char_id, m.agent_id, mission_id);
            let bonus_expiry_unix = m.bonus_expiry_unix;
            let bonus_reward = if bonus_expiry_unix.is_some_and(|bexp| now > bexp) {
                0
            } else {
                raw_bonus_reward
            };
            let origin_sid_opt = m
                .origin_station_id
                .or_else(|| ctx.sde.agents.get(&m.agent_id).and_then(|a| a.station_id))
                .or(session.station_id)
                .filter(|&id| id > 0);
            let Some(origin_loc_id) = origin_sid_opt
                .or_else(|| (m.solar_system_id > 0).then_some(i64::from(m.solar_system_id)))
                .or_else(|| (sys_id > 0).then_some(i64::from(sys_id)))
            else {
                return Some(Err(BridgeRpcError {
                    code: "MissionLocationMissing".into(),
                    message: format!("Mission {mission_id} has no resolvable origin location"),
                    client_error: None,
                }));
            };
            let dest_sid_opt = m.destination_station_id();
            let make_loc = |loc_id: i64, ltype: &str| {
                let st_opt = ctx.sde.stations.get(&loc_id);
                let is_station_loc = st_opt.is_some()
                    || origin_sid_opt.is_some_and(|sid| sid == loc_id)
                    || dest_sid_opt.is_some_and(|sid| sid == loc_id);
                let lsys = st_opt
                    .map(|st| st.solar_system_id as i64)
                    .unwrap_or_else(|| if m.solar_system_id > 0 { m.solar_system_id as i64 } else { sys_id as i64 });
                let (station_rep, ltid) = if is_station_loc {
                    (
                        PyRep::Integer(loc_id),
                        st_opt.map(|st| st.station_type_id as i64).unwrap_or(1529),
                    )
                } else {
                    (PyRep::None, 5_i64)
                };
                PyRep::Dict(vec![
                    (PyRep::String("locationID".into()), PyRep::Integer(loc_id)),
                    (PyRep::String("solarsystemID".into()), PyRep::Integer(lsys)),
                    (PyRep::String("stationID".into()), station_rep),
                    (PyRep::String("typeID".into()), PyRep::Integer(ltid)),
                    (PyRep::String("agentID".into()), PyRep::Integer(m.agent_id)),
                    (PyRep::String("shipTypeID".into()), PyRep::None),
                    (PyRep::String("locationType".into()), PyRep::String(ltype.into())),
                    (PyRep::String("BookmarkID".into()), m.bookmark_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ])
            };
            let has_dungeon_site =
                !is_courier && !is_trade && !is_data_center && !is_rnd_turnin && !is_heraldry;
            let dun_sys = bm_coords
                .map(|(sid, _, _, _)| sid)
                .filter(|&sid| sid > 0)
                .or_else(|| (m.solar_system_id > 0).then_some(m.solar_system_id))
                .unwrap_or(sys_id);
            let dungeon_loc = has_dungeon_site.then(|| {
                let (dx, dy, dz) = bm_coords
                    .map(|(_, x, y, z)| (x, y, z))
                    .or_else(|| m.site_origin_f64().map(|[x, y, z]| (x, y, z)))
                    .unwrap_or_else(|| {
                        let origin =
                            mission_site_origin_for_instance(ctx, dun_sys, m.instance_id);
                        (origin.x, origin.y, origin.z)
                    });
                let dun_bm_rep = m.bookmark_id.map(PyRep::Integer).unwrap_or(PyRep::None);
                let dun_coords_rep =
                    PyRep::Tuple(vec![PyRep::Real(dx), PyRep::Real(dy), PyRep::Real(dz)]);
                PyRep::Dict(vec![
                    (PyRep::String("locationID".into()), PyRep::Integer(i64::from(dun_sys))),
                    (PyRep::String("solarsystemID".into()), PyRep::Integer(i64::from(dun_sys))),
                    (PyRep::String("solarSystemID".into()), PyRep::Integer(i64::from(dun_sys))),
                    (PyRep::String("stationID".into()), PyRep::None),
                    (PyRep::String("typeID".into()), PyRep::Integer(5)),
                    (PyRep::String("agentID".into()), PyRep::Integer(m.agent_id)),
                    (PyRep::String("shipTypeID".into()), PyRep::None),
                    (PyRep::String("BookmarkID".into()), dun_bm_rep),
                    (PyRep::String("coords".into()), dun_coords_rep),
                    (PyRep::String("locationType".into()), PyRep::String("dungeon".into())),
                ])
            });
            let origin_loc = make_loc(origin_loc_id, "agenthomebase");
            let dest_loc_opt = dest_sid_opt.map(|dsid| make_loc(dsid, "objective.destination"));
            let mut locations = vec![origin_loc.clone()];
            if let Some(ref dloc) = dest_loc_opt {
                locations.push(dloc.clone());
            }
            if let Some(ref dloc) = dungeon_loc {
                locations.push(dloc.clone());
            }
            let completion_status = if mission_state == 4
                || (mission_state == 2
                    && !is_courier
                    && !is_trade
                    && !is_mining
                    && !is_data_center
                    && !is_rnd_turnin
                    && !is_heraldry
                    && m.npcs_remaining() == 0)
            {
                1_i64
            } else {
                0_i64
            };
            let agent_corp_id = ctx
                .sde
                .agents
                .get(&m.agent_id)
                .map(|a| a.corporation_id)
                .filter(|&v| v > 0);
            let agent_faction_id = agent_corp_id
                .and_then(|cid| ctx.sde.npc_corporations.get(&cid))
                .and_then(|c| c.faction_id)
                .map(i64::from)
                .filter(|&v| v > 0);
            let station_owner_id = |sid: i64| -> Option<i64> {
                ctx.sde.stations.get(&sid).and_then(|st| {
                    (st.owner_corporation_id > 0)
                        .then_some(st.owner_corporation_id)
                        .or_else(|| st.owner_faction_id.filter(|&f| f > 0))
                })
            };
            let Some(fallback_corp_or_faction_id) = agent_corp_id
                .or(agent_faction_id)
                .or_else(|| origin_sid_opt.and_then(station_owner_id))
                .or_else(|| dest_sid_opt.and_then(station_owner_id))
            else {
                return Some(Err(BridgeRpcError::custom_info(format!(
                    "Cannot resolve corporation or faction for agent {}",
                    m.agent_id
                ))));
            };
            let pickup_owner_id = origin_sid_opt
                .and_then(station_owner_id)
                .unwrap_or(fallback_corp_or_faction_id);
            let dropoff_owner_id = dest_sid_opt
                .and_then(station_owner_id)
                .unwrap_or(pickup_owner_id);
            let dungeons = if let Some(dloc) = dungeon_loc {
                let sde_dungeon_id_opt = ctx
                    .sde
                    .missions
                    .get(&mission_id)
                    .and_then(|mt| mt.dungeon_id)
                    .filter(|did| ctx.sde.dungeons.contains_key(did))
                    .or_else(|| {
                        ctx.sde
                            .dungeons
                            .contains_key(&mission_id)
                            .then_some(mission_id)
                    })
                    .or_else(|| {
                        let af = agent_faction_id.and_then(|v| u32::try_from(v).ok());
                        ctx.sde
                            .dungeons
                            .values()
                            .filter(|d| af.is_some() && d.faction_id == af)
                            .min_by_key(|d| d.dungeon_id)
                            .or_else(|| {
                                ctx.sde
                                    .dungeons
                                    .values()
                                    .filter(|d| d.faction_id.is_some())
                                    .min_by_key(|d| d.dungeon_id)
                            })
                            .or_else(|| ctx.sde.dungeons.values().min_by_key(|d| d.dungeon_id))
                            .map(|d| d.dungeon_id)
                    });
                if let Some(sde_dungeon_id) = sde_dungeon_id_opt.map(i64::from) {
                    let dungeon_owner_id = u32::try_from(sde_dungeon_id)
                        .ok()
                        .and_then(|did| ctx.sde.dungeons.get(&did))
                        .and_then(|d| d.faction_id)
                        .map(i64::from)
                        .filter(|&v| v > 0)
                        .unwrap_or(fallback_corp_or_faction_id);
                    let mut dun_entries = vec![
                        (PyRep::String("dungeonID".into()), PyRep::Integer(sde_dungeon_id)),
                        (PyRep::String("solarsystemID".into()), PyRep::Integer(i64::from(dun_sys))),
                        (PyRep::String("solarSystemID".into()), PyRep::Integer(i64::from(dun_sys))),
                        (PyRep::String("locationID".into()), PyRep::Integer(i64::from(dun_sys))),
                        (PyRep::String("stationID".into()), PyRep::None),
                        (PyRep::String("typeID".into()), PyRep::Integer(5)),
                        (PyRep::String("location".into()), dloc),
                        (PyRep::String("ownerID".into()), PyRep::Integer(dungeon_owner_id)),
                        (PyRep::String("optional".into()), PyRep::Bool(false)),
                        (
                            PyRep::String("objectiveCompleted".into()),
                            PyRep::Bool(completion_status > 0),
                        ),
                        (PyRep::String("shipRestrictions".into()), PyRep::Integer(0)),
                        (PyRep::String("completed".into()), PyRep::Bool(completion_status > 0)),
                    ];
                    if completion_status > 0 {
                        dun_entries.push((
                            PyRep::String("completionStatus".into()),
                            PyRep::Integer(completion_status),
                        ));
                    }
                    vec![PyRep::Dict(dun_entries)]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let bonus_rewards_rep = {
                let rem_secs = bonus_expiry_unix
                    .map(|bexp| bexp.saturating_sub(now))
                    .unwrap_or(bonus_window_secs);
                let window_mins = ((bonus_window_secs + 59) / 60).max(1) as i64;
                if bonus_reward > 0 {
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(rem_secs.saturating_mul(10_000_000) as i64),
                        PyRep::Integer(29), // const.typeCredits
                        PyRep::Integer(bonus_reward),
                        PyRep::None,
                        PyRep::Integer(window_mins),
                    ])])
                } else {
                    PyRep::List(Vec::new())
                }
            };
            if method == "GetMissionObjectiveInfo" {
                let mut v24_objectives = vec![PyRep::Tuple(vec![
                    PyRep::String("agent".into()),
                    PyRep::Tuple(vec![PyRep::Integer(m.agent_id), origin_loc.clone()]),
                ])];
                let make_cargo_dict = |tid: i64, qty: i64, has_cargo: bool| {
                    let unit_vol = u32::try_from(tid)
                        .ok()
                        .and_then(|u_tid| ctx.sde.types.get(&u_tid))
                        .map_or(1.0, |t| t.volume);
                    PyRep::Dict(vec![
                        (PyRep::String("typeID".into()), PyRep::Integer(tid)),
                        (PyRep::String("quantity".into()), PyRep::Integer(qty)),
                        (
                            PyRep::String("volume".into()),
                            PyRep::Real((qty as f64) * unit_vol),
                        ),
                        (PyRep::String("hasCargo".into()), PyRep::Bool(has_cargo)),
                    ])
                };
                if let Some(dest_loc) = dest_loc_opt {
                    let cargo_tid = req_tid_opt.map(i64::from).unwrap_or(34);
                    let cargo_qty = if req_qty > 0 { req_qty } else { 1 };
                    let has_transport_cargo = completion_status > 0 || mission_state >= 2;
                    v24_objectives.push(PyRep::Tuple(vec![
                        PyRep::String("transport".into()),
                        PyRep::Tuple(vec![
                            PyRep::Integer(pickup_owner_id),
                            origin_loc,
                            PyRep::Integer(dropoff_owner_id),
                            dest_loc,
                            make_cargo_dict(cargo_tid, cargo_qty, has_transport_cargo),
                        ]),
                    ]));
                } else if is_rnd_turnin || is_data_center || is_mining || is_trade || is_heraldry {
                    let (fetch_tid, fetch_qty) = if is_rnd_turnin {
                        (
                            i64::from(req_tid_opt.unwrap_or(34)),
                            if req_qty > 0 {
                                req_qty
                            } else {
                                i64::from(rnd_tritanium_required_qty(agent_lvl))
                            },
                        )
                    } else if is_data_center {
                        (
                            i64::from(req_tid_opt.unwrap_or(17202)),
                            if req_qty > 0 { req_qty } else { 1 },
                        )
                    } else if is_heraldry {
                        let hd = (req_tid_opt.is_none() || req_qty <= 0)
                            .then(|| tmpl.map(|t| resolve_heraldry_mission_details(ctx, t)))
                            .flatten();
                        (
                            i64::from(
                                req_tid_opt
                                    .or_else(|| hd.map(|d| d.ship_type_id))
                                    .unwrap_or(587),
                            ),
                            if req_qty > 0 {
                                req_qty
                            } else {
                                hd.map_or(1, |d| i64::from(d.quantity))
                            },
                        )
                    } else if is_mining {
                        let md = (req_tid_opt.is_none() || req_qty <= 0)
                            .then(|| tmpl.map(|t| resolve_mining_mission_details(ctx, t)))
                            .flatten();
                        (
                            i64::from(
                                req_tid_opt
                                    .or_else(|| md.as_ref().map(|d| d.ore_type_id))
                                    .unwrap_or(1230),
                            ),
                            if req_qty > 0 {
                                req_qty
                            } else {
                                md.map_or(500, |d| i64::from(d.required_units))
                            },
                        )
                    } else {
                        let td = (req_tid_opt.is_none() || req_qty <= 0)
                            .then(|| tmpl.map(|t| resolve_trade_mission_details(ctx, t)))
                            .flatten();
                        (
                            i64::from(
                                req_tid_opt
                                    .or_else(|| td.as_ref().map(|d| d.trade_type_id))
                                    .unwrap_or(43),
                            ),
                            if req_qty > 0 {
                                req_qty
                            } else {
                                td.map_or(20, |d| i64::from(d.trade_quantity))
                            },
                        )
                    };
                    let fetch_req_tid = u32::try_from(fetch_tid).unwrap_or(0);
                    let agent_station_id = ctx
                        .sde
                        .agents
                        .get(&m.agent_id)
                        .and_then(|a| a.station_id)
                        .or(m.origin_station_id);
                    let eligible_qty = if completion_status > 0 || fetch_req_tid == 0 {
                        0
                    } else {
                        ctx.db.with_snapshot(|snap| {
                            let active_ship_id = snap
                                .characters
                                .get(&char_id)
                                .map(|c| c.active_ship_id)
                                .filter(|&id| id > 0)
                                .or(session.ship_id);
                            super::agents_mission_delivery::count_eligible_mission_delivery_items(
                                snap,
                                char_id,
                                fetch_req_tid,
                                is_mining,
                                agent_station_id,
                                active_ship_id,
                                is_heraldry,
                            )
                        })
                    };
                    let has_fetch_cargo = completion_status > 0 || eligible_qty >= fetch_qty;
                    v24_objectives.push(PyRep::Tuple(vec![
                        PyRep::String("fetch".into()),
                        PyRep::Tuple(vec![
                            PyRep::Integer(dropoff_owner_id),
                            origin_loc,
                            make_cargo_dict(fetch_tid, fetch_qty, has_fetch_cargo),
                        ]),
                    ]));
                }
                let normal_rewards = if isk_reward > 0 {
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(29), // const.typeCredits
                        PyRep::Integer(isk_reward),
                        PyRep::None,
                    ])])
                } else {
                    PyRep::List(Vec::new())
                };
                return Some(Ok(PyRep::Dict(vec![
                    (
                        PyRep::String("completionStatus".into()),
                        PyRep::Integer(completion_status),
                    ),
                    (
                        PyRep::String("missionTitleID".into()),
                        PyRep::String(title.clone()),
                    ),
                    (
                        PyRep::String("missionState".into()),
                        PyRep::Integer(mission_state),
                    ),
                    (PyRep::String("missionName".into()), PyRep::String(title)),
                    (
                        PyRep::String("objectives".into()),
                        PyRep::List(v24_objectives),
                    ),
                    (PyRep::String("dungeons".into()), PyRep::List(dungeons)),
                    (PyRep::String("locations".into()), PyRep::List(locations)),
                    (PyRep::String("agentGift".into()), PyRep::List(Vec::new())),
                    (PyRep::String("normalRewards".into()), normal_rewards),
                    (
                        PyRep::String("loyaltyPoints".into()),
                        PyRep::Integer(lp_reward),
                    ),
                    (PyRep::String("researchPoints".into()), PyRep::Integer(0)),
                    (PyRep::String("bonusRewards".into()), bonus_rewards_rep),
                    (PyRep::String("collateral".into()), PyRep::List(Vec::new())),
                    (
                        PyRep::String("contentID".into()),
                        PyRep::Integer(mission_id as i64),
                    ),
                ])));
            }
            let mut info_fields = vec![
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("contentID", PyRep::Integer(mission_id as i64)),
                ("agentID", PyRep::Integer(m.agent_id)),
                ("corporationID", PyRep::Integer(fallback_corp_or_faction_id)),
                ("pickupOwnerID", PyRep::Integer(pickup_owner_id)),
                ("dropoffOwnerID", PyRep::Integer(dropoff_owner_id)),
                ("level", PyRep::Integer(offer_lvl as i64)),
                ("title", PyRep::String(title)),
                ("state", PyRep::String(m.state.as_str().into())),
                ("missionState", PyRep::Integer(mission_state)),
                ("completionStatus", PyRep::Integer(completion_status)),
                ("briefing", PyRep::String(briefing)),
                ("objectives", PyRep::String(objectives.into())),
                ("locations", PyRep::List(locations)),
                ("dungeons", PyRep::List(dungeons)),
                ("originStationID", origin_sid_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("destinationStationID", dest_sid_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("solarsystemID", PyRep::Integer(m.solar_system_id as i64)),
                ("currentRoomID", PyRep::Integer(m.current_room_id as i64)),
                ("npcsRemaining", PyRep::Integer(m.npcs_remaining() as i64)),
                ("requiredTypeID", req_type),
                ("requiredQuantity", PyRep::Integer(req_qty)),
                ("iskReward", PyRep::Integer(isk_reward)),
                ("lpReward", PyRep::Integer(lp_reward)),
                ("bonusReward", PyRep::Integer(bonus_reward)),
                ("bonusWindowSecs", PyRep::Integer(bonus_window_secs as i64)),
                ("bonusRewards", bonus_rewards_rep),
            ];
            if let Some(bexp) = bonus_expiry_unix {
                info_fields.push(("bonusExpiryUnix", PyRep::Integer(bexp as i64)));
            }
            if let Some(fid) = agent_faction_id {
                info_fields.push(("factionID", PyRep::Integer(fid)));
            }
            Some(Ok(PyRep::keyval(info_fields)))
        }

        ("agentMgr", "DoAction") | ("agentMgr", "GetOffer") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let explicit_action_opt = if method == "GetOffer" {
                Some(2_i64)
            } else {
                args.first()
                    .and_then(|a| a.as_i64())
                    .filter(|v| (1..=19).contains(v))
            };
            let is_implicit_greeting = explicit_action_opt.is_none();
            let action_id = explicit_action_opt.unwrap_or(2_i64);

            let bound_agent_id = bound_entry
                .filter(|b| b.service == "agentMgr" && b.object_id > 0)
                .map(|b| b.object_id);
            let (mut agent_id, explicit_agent) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);

            if (12..=19).contains(&action_id) {
                if bound_agent_id.is_none() {
                    if let Some(sid) = args.get(1).and_then(|a| a.as_i64()) {
                        if ctx.sde.agents.contains_key(&sid) {
                            agent_id = sid;
                        }
                    }
                }
                return agents_standings_arcs::handle_special_dialogue_action(
                    ctx, session, char_id, sys_id, action_id, agent_id, args, _kwargs,
                );
            }

            match action_id {
                // 1 = ViewMission, 2 = RequestMission, 8 = Continue
                1 | 2 | 8 => {
                    agents_missions_offer::handle_mission_offer_action(
                        ctx,
                        session,
                        char_id,
                        sys_id,
                        agent_id,
                        explicit_agent,
                        bound_agent_id,
                        is_implicit_greeting,
                        args,
                        _kwargs,
                    )
                    .await
                }

                // 3 = Accept, 4 = AcceptChoice, 5 = AcceptRemotely
                3..=5 => {
                    agents_missions_accept::handle_mission_accept_action(
                        ctx,
                        session,
                        char_id,
                        sys_id,
                        action_id,
                        agent_id,
                        explicit_agent,
                        args,
                        _kwargs,
                    )
                    .await
                }

                // 6 = Complete, 7 = CompleteRemotely
                6 | 7 => {
                    agents_missions_complete::handle_mission_complete_action(
                        ctx,
                        session,
                        char_id,
                        sys_id,
                        action_id,
                        agent_id,
                        explicit_agent,
                        args,
                        _kwargs,
                    )
                    .await
                }

                // 9 = Decline, 10 = Defer, 11 = Quit
                9..=11 => {
                    let expected_state = if action_id == 11 {
                        Some(MissionState::Accepted)
                    } else {
                        Some(MissionState::Offered)
                    };
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, expected_state, args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.missions
                            .get_by_character_template(char_id, mission_id)
                            .map_or(agent_id, |m| m.agent_id)
                    });
                    match abandon_or_decline_mission(ctx, session, char_id, mission_id).await {
                        Ok((new_state, penalty_applied, was_accepted)) => {
                            let mission_state = if was_accepted { 6_i64 } else { 5_i64 };
                            if was_accepted {
                                ctx.emit_typed(
                                    NotificationTarget::Character(char_id),
                                    OnAgentMissionObjectivesFailed {
                                        agent_id: active_agent_id,
                                    },
                                );
                            }
                            let action_str = if was_accepted { "quit" } else { "offer_declined" };
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnAgentMissionChange {
                                    action: action_str.into(),
                                    agent_id: active_agent_id,
                                },
                            );
                            let quit_msg = if was_accepted {
                                "Very well. I have cancelled your active assignment.".to_string()
                            } else {
                                "Very well. Let me know when you are ready for another assignment.".to_string()
                            };
                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(active_agent_id)),
                                ("state", PyRep::String(new_state)),
                                ("missionState", PyRep::Integer(mission_state)),
                                ("agentSays", PyRep::String(quit_msg.clone())),
                                ("briefing", PyRep::String(quit_msg)),
                                (
                                    "dialogue",
                                    PyRep::List(vec![PyRep::Tuple(vec![
                                        PyRep::Integer(2),
                                        PyRep::String("Request Mission".into()),
                                    ])]),
                                ),
                                ("standingPenaltyApplied", PyRep::Bool(penalty_applied)),
                                ("dungeonCleaned", PyRep::Bool(was_accepted)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                _ => Some(Err(BridgeRpcError { code: "InvalidDialogueAction".into(), message: format!("Unknown agent dialogue action {action_id}"), client_error: None })),
            }
        }

        ("agentMgr", "RemoveOfferFromJournal") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            let (agent_id, explicit_agent) =
                resolve_target_agent_id_with_bound(ctx, session, bound_entry, char_id, sys_id, args, _kwargs);
            let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
            let mission_id = match parse_mission_id(args).or_else(|_| {
                resolve_target_mission_id(ctx, char_id, lookup_agent_id, None, args, _kwargs)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            match abandon_or_decline_mission(ctx, session, char_id, mission_id).await {
                Ok((new_state, penalty_applied, was_accepted)) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("state", PyRep::String(new_state)),
                    ("standingPenaltyApplied", PyRep::Bool(penalty_applied)),
                    ("dungeonCleaned", PyRep::Bool(was_accepted)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a crate::router::BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_with_bound(ctx, session, bound_entry, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("agentMgr", "GetAgents", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetInfoServiceDetails", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetAgentLocationWrap", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetDungeonShipRestrictions", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetEntryPoint", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMissionKeywords", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetStandingGainsForMission", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "IsCheatingWithAgent", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "ShouldAlwaysAllowReplay", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "WarpToAgentInSpace", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GotoLocation", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "WarpToLocation", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetSolarSystemOfAgent", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMyJournalDetails", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMissionBriefingInfo", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMissionObjectiveInfo", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetMissionJournalInfo", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "DoAction", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "GetOffer", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("agentMgr", "RemoveOfferFromJournal", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

#[cfg(test)] #[path = "agents_missions_unit_tests.rs"] mod agents_missions_unit_tests;
#[cfg(test)] #[path = "agents_missions_edges2_tests.rs"] mod agents_missions_edges2_tests;
#[cfg(test)] #[path = "agents_missions_tests.rs"] mod agents_missions_tests;
#[cfg(test)] #[path = "agents_missions_t5_unit_tests.rs"] mod agents_missions_t5_unit_tests;

