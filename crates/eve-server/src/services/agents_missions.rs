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

use crate::router::notify_registry::{
    OnAgentMissionChange, OnAgentMissionObjectivesCompleted, OnAgentMissionObjectivesFailed,
    OnIncomingTransmission,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_character_services::emit_agent_mission_dialogue;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, StandingTransactionRecord,
    WalletJournalEntry,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
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

fn deterministic_mission_site_origin(
    ctx: &ServerContext,
    mission_sys_id: u32,
    mission_id: u32,
) -> DVec3 {
    let base = ctx
        .sde
        .solar_systems
        .get(&mission_sys_id)
        .and_then(|s| s.stations.first().map(|st| st.position))
        .unwrap_or(DVec3::ZERO);
    // Place mission dungeons well outside station undock grids so warps always exceed 150 km.
    let offset = DVec3::new(
        5_000_000.0 + ((mission_id % 97) as f64) * 50_000.0,
        2_000_000.0 + ((mission_id % 13) as f64) * 25_000.0,
        ((mission_id % 7) as f64) * 10_000.0,
    );
    base + offset
}

const MIN_AGENT_WARP_DISTANCE_M: f64 = 150_000.0;
const MAX_AGENT_WARP_RANGE_M: f64 = 100_000.0;

const ERR_INVALID_ARGUMENT: &str = "InvalidArgument";
const ERR_INVALID_MISSION_LOCATION: &str = "InvalidMissionLocation";
const ERR_SHIP_DOCKED: &str = "ShipDocked";
const ERR_NO_ACTIVE_SHIP: &str = "NoActiveShip";
const ERR_NO_SOLAR_SYSTEM: &str = "NoSolarSystem";
const ERR_DESTINY_QUERY_FAILED: &str = "DestinyQueryFailed";
const ERR_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
const ERR_CROSS_SYSTEM_WARP: &str = "CrossSystemWarp";
const ERR_BOOKMARK_EXPIRED: &str = "BookmarkExpired";
const ERR_BOOKMARK_NOT_FOUND: &str = "BookmarkNotFound";

fn agent_warp_err(code: &'static str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_string(),
        message: message.into(),
        client_error: None,
    }
}

fn can_use_bookmark_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    bm: &eve_db::BookmarkRecord,
) -> bool {
    bm.owner_id == char_id
        || bm.folder_id.is_some_and(|fid| {
            snap.bookmark_folders.get(&fid).is_some_and(|f| {
                crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                    snap, session, char_id, f,
                ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
            })
        })
}

enum AgentWarpTarget {
    Dungeon {
        mission_id: u32,
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
    StationOrCelestial {
        location_id: i64,
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
    SpacePoint {
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
}

impl AgentWarpTarget {
    fn solar_system_id(&self) -> u32 {
        match *self {
            Self::Dungeon { solar_system_id, .. }
            | Self::StationOrCelestial { solar_system_id, .. }
            | Self::SpacePoint { solar_system_id, .. } => solar_system_id,
        }
    }
}

struct ParsedAgentWarpArgs<'a> {
    loc_type: Option<&'a str>,
    first_id: Option<i64>,
    loc_num: Option<i64>,
    referring_agent_id: Option<i64>,
    warp_range: f64,
    is_goto: bool,
}

fn parse_agent_warp_args<'a>(
    method: &str,
    args: &'a [PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<ParsedAgentWarpArgs<'a>, BridgeRpcError> {
    let (arg0, arg1, arg2) = match args {
        [a0, a1, a2, ..] => (Some(a0), Some(a1), Some(a2)),
        [a0, a1] => (Some(a0), Some(a1), None),
        [a0] => (Some(a0), None, None),
        [] => (None, None, None),
    };
    let is_goto = method == "GotoLocation";
    // V24 `ApproachLocation` sends `agentMgr.GotoLocation(locType, locNumber, referringAgentID)`:
    // `args[2]` is an integer agent ID, never a warp standoff range.
    let (referring_agent_id, warp_range) = if is_goto {
        let ref_agent = arg2
            .and_then(|v| v.as_i64())
            .filter(|&id| id > 0)
            .or_else(|| {
                kwargs
                    .get("referringAgentID")
                    .and_then(|v| v.as_i64())
                    .filter(|&id| id > 0)
            });
        (ref_agent, 0.0)
    } else {
        let raw_range = arg2
            .and_then(|v| v.as_f64())
            .or_else(|| kwargs.get("warpRange").and_then(|v| v.as_f64()))
            .or_else(|| kwargs.get("minRange").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        if !raw_range.is_finite() {
            return Err(agent_warp_err(
                ERR_INVALID_ARGUMENT,
                "warpRange must be a finite number",
            ));
        }
        (None, raw_range.clamp(0.0, MAX_AGENT_WARP_RANGE_M))
    };
    Ok(ParsedAgentWarpArgs {
        loc_type: arg0.and_then(|v| v.as_str()),
        first_id: arg0.and_then(|v| v.as_i64()).filter(|&id| id > 0),
        loc_num: arg1.and_then(|v| v.as_i64()).filter(|&n| n > 0),
        referring_agent_id,
        warp_range,
        is_goto,
    })
}

fn find_accepted_mission_for_warp(
    ctx: &ServerContext,
    char_id: i64,
    bound_agent_id: Option<i64>,
    location_number: Option<i64>,
    referring_agent_id: Option<i64>,
    kwargs: &HashMap<String, PyRep>,
) -> Option<eve_db::ActiveMissionRecord> {
    let kw_mission_id = kwargs
        .get("missionID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0)
        .map(|id| id as u32);
    let kw_agent_id = kwargs
        .get("agentID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0);
    ctx.db.with_snapshot(|snap| {
        let mut missions: Vec<&eve_db::ActiveMissionRecord> = snap
            .active_missions
            .iter()
            .filter(|((cid, _), m)| *cid == char_id && m.state == "Accepted")
            .map(|(_, m)| m)
            .collect();
        missions.sort_by_key(|m| m.mission_id);
        if missions.is_empty() {
            return None;
        }
        if let Some(mid) = kw_mission_id {
            return missions.iter().find(|m| m.mission_id == mid).map(|m| (*m).clone());
        }
        if let Some(loc) = location_number {
            return missions
                .iter()
                .find(|m| {
                    i64::from(m.mission_id) == loc
                        || m.agent_id == loc
                        || m.bookmark_id == Some(loc)
                })
                .map(|m| (*m).clone());
        }
        if let Some(ag_id) = bound_agent_id.or(kw_agent_id).or(referring_agent_id) {
            return missions.iter().find(|m| m.agent_id == ag_id).map(|m| (*m).clone());
        }
        missions.first().map(|m| (*m).clone())
    })
}

fn station_warp_target(
    ctx: &ServerContext,
    station_id: i64,
) -> Result<AgentWarpTarget, BridgeRpcError> {
    let st = ctx.sde.stations.get(&station_id).ok_or_else(|| {
        agent_warp_err(
            ERR_INVALID_MISSION_LOCATION,
            format!("Station {station_id} not found in SDE"),
        )
    })?;
    Ok(AgentWarpTarget::StationOrCelestial {
        location_id: station_id,
        solar_system_id: st.solar_system_id,
        anchor_pos: st.position,
    })
}

async fn agent_homebase_or_space_target(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    current_sys_id: u32,
    agent_id: i64,
) -> Result<AgentWarpTarget, BridgeRpcError> {
    if let Some(agent) = ctx.sde.agents.get(&agent_id) {
        if let Some(ag_sys) = agent.solar_system_id {
            if ag_sys != current_sys_id {
                return Ok(AgentWarpTarget::SpacePoint {
                    solar_system_id: ag_sys,
                    anchor_pos: DVec3::ZERO,
                });
            }
        }
        if let Some(st_id) = agent.station_id {
            return station_warp_target(ctx, st_id);
        }
        if let Some(pos) = sys_handle.get_ball_position_sync(agent_id) {
            return Ok(AgentWarpTarget::SpacePoint {
                solar_system_id: current_sys_id,
                anchor_pos: pos,
            });
        }
        return Err(agent_warp_err(
            ERR_INVALID_MISSION_LOCATION,
            format!("Agent {agent_id} has no warpable station or space ball"),
        ));
    }
    // Only treat an uncatalogued ballpark entity as an in-space agent when it is an Agent-in-Space ball (typeID 24).
    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(agent_id).await {
        if ball.type_id == 24 {
            return Ok(AgentWarpTarget::SpacePoint {
                solar_system_id: current_sys_id,
                anchor_pos: ball.position,
            });
        }
    }
    Err(agent_warp_err(
        ERR_INVALID_MISSION_LOCATION,
        format!("Unknown agent {agent_id} for warp target"),
    ))
}

fn mission_dungeon_warp_target(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    fallback_sys_id: u32,
    mission: &eve_db::ActiveMissionRecord,
) -> Result<AgentWarpTarget, BridgeRpcError> {
    let mission_sys_id = if mission.solar_system_id > 0 {
        mission.solar_system_id
    } else {
        ctx.sde
            .agents
            .get(&mission.agent_id)
            .and_then(|a| a.solar_system_id)
            .unwrap_or(fallback_sys_id)
    };
    if mission_sys_id == 0 {
        return Err(agent_warp_err(
            ERR_INVALID_MISSION_LOCATION,
            format!("Mission {} has no solar system", mission.mission_id),
        ));
    }
    let now_unix = ctx.now_unix();
    let bm_pos = ctx.db.with_snapshot(|snap| {
        mission.bookmark_id.and_then(|bmid| {
            if crate::services::fleet_social::is_bookmark_expired_in_snap(snap, bmid, now_unix) {
                return None;
            }
            snap.bookmarks.get(&bmid).and_then(|bm| {
                (can_use_bookmark_in_snap(snap, session, char_id, bm)
                    && bm.solar_system_id == mission_sys_id
                    && bm.item_id.is_none()
                    && bm.x.is_finite()
                    && bm.y.is_finite()
                    && bm.z.is_finite())
                .then(|| DVec3::new(bm.x, bm.y, bm.z))
            })
        })
    });
    let anchor_pos = bm_pos.unwrap_or_else(|| {
        deterministic_mission_site_origin(ctx, mission_sys_id, mission.mission_id)
    });
    Ok(AgentWarpTarget::Dungeon {
        mission_id: mission.mission_id,
        solar_system_id: mission_sys_id,
        anchor_pos,
    })
}

async fn resolve_agent_warp_target(
    ctx: &ServerContext,
    session: &SessionState,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    sys_id: u32,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    method: &str,
    parsed: &ParsedAgentWarpArgs<'_>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<AgentWarpTarget, BridgeRpcError> {
    let bound_agent_id = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
    let kw_agent_id = kwargs
        .get("agentID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0);
    let loc_num = parsed.loc_num;
    let ref_agent_id = parsed.referring_agent_id;

    if method == "WarpToAgentInSpace" {
        let target_agent_id = parsed
            .first_id
            .or(kw_agent_id)
            .or(bound_agent_id)
            .or(ref_agent_id)
            .or_else(|| {
                find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    loc_num,
                    ref_agent_id,
                    kwargs,
                )
                .map(|m| m.agent_id)
            })
            .ok_or_else(|| {
                agent_warp_err(
                    ERR_INVALID_MISSION_LOCATION,
                    "No agent specified for WarpToAgentInSpace",
                )
            })?;
        return agent_homebase_or_space_target(ctx, sys_handle, sys_id, target_agent_id).await;
    }

    if let Some(raw_type) = parsed.loc_type {
        let loc_type = raw_type.trim().to_ascii_lowercase();
        match loc_type.as_str() {
            "dungeon" | "mission" | "site" | "pocket" | "encounter" => {
                if let Some(m) = find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    loc_num,
                    ref_agent_id,
                    kwargs,
                ) {
                    return mission_dungeon_warp_target(ctx, session, char_id, sys_id, &m);
                }
                if let Some(pos) =
                    crate::services::pve_dungeon_runtime::resolve_deadspace_or_escalation_warp_target(
                        ctx,
                        char_id,
                        sys_id,
                        &loc_type,
                        loc_num.unwrap_or(0),
                        Some(&loc_type),
                    )?
                {
                    return Ok(AgentWarpTarget::SpacePoint {
                        solar_system_id: sys_id,
                        anchor_pos: pos,
                    });
                }
                return Err(agent_warp_err(
                    ERR_INVALID_MISSION_LOCATION,
                    "No active mission dungeon location found",
                ));
            }
            "objective.destination" | "destination" | "courier" | "dropoff" => {
                if let Some(m) = find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    loc_num,
                    ref_agent_id,
                    kwargs,
                ) {
                    let dest_sid = ctx.db.with_snapshot(|snap| {
                        load_mission_meta(snap, char_id, m.mission_id)
                            .and_then(|mt| mt.destination_station_id.or(mt.origin_station_id))
                    });
                    if let Some(st_id) = dest_sid {
                        return station_warp_target(ctx, st_id);
                    }
                    if m.bookmark_id.is_some() || m.npcs_remaining > 0 {
                        return mission_dungeon_warp_target(ctx, session, char_id, sys_id, &m);
                    }
                    return agent_homebase_or_space_target(ctx, sys_handle, sys_id, m.agent_id)
                        .await;
                }
                return Err(agent_warp_err(
                    ERR_INVALID_MISSION_LOCATION,
                    "No active mission destination found",
                ));
            }
            "agenthomebase" | "station" | "agent" => {
                let mission_opt = find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    loc_num,
                    ref_agent_id,
                    kwargs,
                );
                if let Some(ref m) = mission_opt {
                    if bound_agent_id.is_none() || bound_agent_id == Some(m.agent_id) {
                        let origin_sid = ctx.db.with_snapshot(|snap| {
                            load_mission_meta(snap, char_id, m.mission_id)
                                .and_then(|mt| mt.origin_station_id)
                        });
                        if let Some(st_id) = origin_sid.filter(|id| ctx.sde.stations.contains_key(id))
                        {
                            return station_warp_target(ctx, st_id);
                        }
                    }
                }
                let target_agent_id = bound_agent_id
                    .or_else(|| loc_num.filter(|id| ctx.sde.agents.contains_key(id)))
                    .or(kw_agent_id)
                    .or(ref_agent_id)
                    .or_else(|| mission_opt.map(|m| m.agent_id))
                    .ok_or_else(|| {
                        agent_warp_err(
                            ERR_INVALID_MISSION_LOCATION,
                            "No agent homebase location found",
                        )
                    })?;
                return agent_homebase_or_space_target(ctx, sys_handle, sys_id, target_agent_id)
                    .await;
            }
            "bookmark" => {
                let bmid = loc_num
                    .or(parsed.first_id)
                    .or_else(|| kwargs.get("bookmarkID").and_then(|v| v.as_i64()))
                    .ok_or_else(|| {
                        agent_warp_err(
                            ERR_INVALID_MISSION_LOCATION,
                            "Bookmark warp requires bookmarkID",
                        )
                    })?;
                if sys_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH {
                    return Err(BridgeRpcError::from_user_key(
                        eve_proto::UserErrorKey::DunBlacklistCannotWarp,
                    ));
                }
                let now_unix = ctx.now_unix();
                let bm = ctx.db.with_snapshot(|snap| {
                    let expired = crate::services::fleet_social::is_bookmark_expired_in_snap(
                        snap, bmid, now_unix,
                    );
                    let Some(b) = snap.bookmarks.get(&bmid) else {
                        return Err(if expired {
                            agent_warp_err(
                                ERR_BOOKMARK_EXPIRED,
                                format!("Bookmark {bmid} has expired"),
                            )
                        } else {
                            agent_warp_err(
                                ERR_BOOKMARK_NOT_FOUND,
                                format!("Bookmark {bmid} not found"),
                            )
                        });
                    };
                    if !can_use_bookmark_in_snap(snap, session, char_id, b) {
                        return Err(agent_warp_err(
                            ERR_BOOKMARK_NOT_FOUND,
                            format!("Bookmark {bmid} not found"),
                        ));
                    }
                    if expired {
                        return Err(agent_warp_err(
                            ERR_BOOKMARK_EXPIRED,
                            format!("Bookmark {bmid} has expired"),
                        ));
                    }
                    if b.solar_system_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH {
                        return Err(BridgeRpcError::from_user_key(
                            eve_proto::UserErrorKey::DunBlacklistCannotWarp,
                        ));
                    }
                    if !b.x.is_finite() || !b.y.is_finite() || !b.z.is_finite() {
                        return Err(agent_warp_err(
                            ERR_BOOKMARK_NOT_FOUND,
                            format!("Bookmark {bmid} has invalid coordinates"),
                        ));
                    }
                    Ok(b.clone())
                })?;
                let anchor_pos = DVec3::new(bm.x, bm.y, bm.z);
                if let Some(st_id) = bm.item_id.filter(|id| ctx.sde.stations.contains_key(id)) {
                    return Ok(AgentWarpTarget::StationOrCelestial {
                        location_id: st_id,
                        solar_system_id: bm.solar_system_id,
                        anchor_pos,
                    });
                }
                return Ok(AgentWarpTarget::SpacePoint {
                    solar_system_id: bm.solar_system_id,
                    anchor_pos,
                });
            }
            _ => {
                return Err(agent_warp_err(
                    ERR_INVALID_MISSION_LOCATION,
                    format!("Unsupported agent warp locationType '{raw_type}'"),
                ));
            }
        }
    }

    if let Some(id) = parsed.first_id.or(loc_num) {
        if ctx.sde.agents.contains_key(&id) {
            return agent_homebase_or_space_target(ctx, sys_handle, sys_id, id).await;
        }
        if let Some(m) = find_accepted_mission_for_warp(
            ctx,
            char_id,
            bound_agent_id,
            Some(id),
            ref_agent_id,
            kwargs,
        ) {
            if i64::from(m.mission_id) == id || m.bookmark_id == Some(id) {
                return mission_dungeon_warp_target(ctx, session, char_id, sys_id, &m);
            }
        }
        if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(id).await {
            if ball.type_id == 24 {
                return Ok(AgentWarpTarget::SpacePoint {
                    solar_system_id: sys_id,
                    anchor_pos: ball.position,
                });
            }
        }
        return Err(agent_warp_err(
            ERR_INVALID_MISSION_LOCATION,
            format!("Unknown agent warp location {id}"),
        ));
    }

    if let Some(m) = find_accepted_mission_for_warp(
        ctx,
        char_id,
        bound_agent_id,
        None,
        ref_agent_id,
        kwargs,
    ) {
        return mission_dungeon_warp_target(ctx, session, char_id, sys_id, &m);
    }
    if let Some(ag_id) = bound_agent_id.or(kw_agent_id).or(ref_agent_id) {
        return agent_homebase_or_space_target(ctx, sys_handle, sys_id, ag_id).await;
    }
    Err(agent_warp_err(
        ERR_INVALID_MISSION_LOCATION,
        "No active mission or warp target specified",
    ))
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
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(agent_warp_err(
            ERR_SHIP_DOCKED,
            "Cannot initiate agent warp while docked",
        ));
    }
    let ship_id = session.ship_id.ok_or_else(|| {
        agent_warp_err(ERR_NO_ACTIVE_SHIP, "Session has no active ship in space")
    })?;
    let sys_id = session
        .solar_system_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.solar_system_id))
        })
        .ok_or_else(|| {
            agent_warp_err(ERR_NO_SOLAR_SYSTEM, "Session has no active solar system")
        })?;
    let parsed = parse_agent_warp_args(method, args, kwargs)?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, current_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| {
            agent_warp_err(
                ERR_DESTINY_QUERY_FAILED,
                format!("Failed to query active ship ball {ship_id}: {e}"),
            )
        })?
        .ok_or_else(|| {
            agent_warp_err(
                ERR_SHIP_NOT_IN_SPACE,
                format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
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
        return Err(agent_warp_err(
            ERR_CROSS_SYSTEM_WARP,
            format!("Target is in solar system {target_sys_id}, while pilot is in {sys_id}"),
        ));
    }

    let landing_pos = match &target {
        AgentWarpTarget::StationOrCelestial {
            location_id,
            anchor_pos,
            ..
        } => crate::services::nav_warp_interdiction::resolve_warp_landing_point(
            ctx,
            sys_id,
            *location_id,
            current_ball.position,
            *anchor_pos,
        ),
        AgentWarpTarget::Dungeon { anchor_pos, .. }
        | AgentWarpTarget::SpacePoint { anchor_pos, .. } => *anchor_pos,
    };

    let delta = landing_pos - current_ball.position;
    let raw_dest_pos = if parsed.warp_range > 0.0 && delta.length_squared() > 1.0 {
        landing_pos - delta.normalize() * parsed.warp_range
    } else {
        landing_pos
    };

    let warp_dist_m = current_ball.position.distance(raw_dest_pos);
    if !warp_dist_m.is_finite() {
        return Err(agent_warp_err(
            ERR_INVALID_ARGUMENT,
            "Destination distance must be finite",
        ));
    }
    let is_subwarp_goto = parsed.is_goto && warp_dist_m < MIN_AGENT_WARP_DISTANCE_M;
    if is_subwarp_goto {
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
    } else {
        if warp_dist_m < MIN_AGENT_WARP_DISTANCE_M {
            return Err(BridgeRpcError::warp_distance_too_short(format!(
                "Warp vector distance {warp_dist_m:.1} m is below minimum {MIN_AGENT_WARP_DISTANCE_M:.0} m"
            )));
        }

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
    }

    if let AgentWarpTarget::Dungeon {
        mission_id,
        anchor_pos,
        ..
    } = target
    {
        // Reconcile stale mission NPC ball IDs if system ballpark restarted/evicted since accept.
        let stale_ids = ctx.db.with_snapshot(|snap| {
            snap.active_missions
                .get(&(char_id, mission_id))
                .filter(|m| m.npcs_remaining > 0 && !m.spawned_npc_ball_ids.is_empty())
                .map(|m| m.spawned_npc_ball_ids.clone())
        });
        if let Some(npc_ids) = stale_ids {
            let any_alive = npc_ids
                .iter()
                .any(|&npc_id| sys_handle.get_ball_position_sync(npc_id).is_some());
            if !any_alive {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                        m.spawned_npc_ball_ids.clear();
                    }
                });
            }
        }
        let _ = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
            ctx,
            char_id,
            sys_id,
            anchor_pos,
        );
    }

    if !is_subwarp_goto {
        crate::services::pve_dungeon_runtime::on_warp_start_leave_dungeon_if_needed(
            ctx,
            char_id,
            sys_id,
            current_ball.position.to_array(),
            raw_dest_pos.to_array(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MissionArchetype {
    is_data_center: bool,
    is_rnd_turnin: bool,
    is_heraldry: bool,
    is_trade: bool,
    is_courier: bool,
    is_kill: bool,
}

/// Transitions a character's active mission state, removes any mission bookmark, and reclaims
/// mission-scoped courier cargo.
fn finalize_mission_state_cleanup(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
    new_state: &str,
    zero_npcs: bool,
) {
    let bmid = snap
        .active_missions
        .get_mut(&(char_id, mission_id))
        .and_then(|m| {
            m.state = new_state.into();
            if zero_npcs {
                m.npcs_remaining = 0;
            }
            m.bookmark_id.take()
        });
    if let Some(bmid) = bmid {
        snap.bookmarks.remove(&bmid);
    }
    reclaim_courier_cargo(snap, char_id, mission_id);
}

/// Classifies an agent mission into server gameplay archetypes from its SDE template, optional
/// offer/delivery metadata, and optional agent record.
///
/// Several non-courier SDE templates (`*TradeMission` variants and Paragon `HeraldryMission`
/// templates) carry `is_courier = 1` in static SDE data, while Data Center (`agent_type_id == 5`)
/// and non-delivery R&D turn-ins (`agent_type_id == 4` with no `destination_station_id`) deliver
/// directly to the agent's station rather than using courier cargo grant or destination hand-in.
fn classify_mission_archetype(
    tmpl: Option<&eve_sde::MissionTemplate>,
    meta: Option<&MissionTimingAndCargoMeta>,
    agent: Option<&eve_sde::AgentInfo>,
) -> MissionArchetype {
    let ct = tmpl
        .and_then(|t| t.content_template.as_deref())
        .unwrap_or_default();
    let is_data_center = agent.is_some_and(|a| a.agent_type_id == 5);
    let is_rnd_turnin = agent.is_some_and(is_rnd_agent)
        && meta.is_none_or(|mt| mt.destination_station_id.is_none());
    let is_heraldry = ct.contains("Heraldry");
    let is_trade = !is_data_center && !is_rnd_turnin && ct.contains("Trade");
    let has_courier_signal = tmpl.is_some_and(|t| t.is_courier)
        || ct.contains("Courier")
        || meta.is_some_and(|mt| mt.destination_station_id.is_some());
    let is_courier =
        !is_trade && !is_heraldry && !is_data_center && !is_rnd_turnin && has_courier_signal;
    let is_kill = ct.contains("BasicKill") || ct.contains("Kill") || ct.contains("Encounter");
    MissionArchetype {
        is_data_center,
        is_rnd_turnin,
        is_heraldry,
        is_trade,
        is_courier,
        is_kill,
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
    meta: Option<&MissionTimingAndCargoMeta>,
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
    let origin_sid = meta
        .and_then(|mt| mt.origin_station_id)
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

    let mut text = if !ct.is_empty() && !ct.starts_with("agent.missionTemplatizedContent_") {
        if title.is_empty() || ct.contains(title) {
            ct.to_string()
        } else {
            format!("{title}: {ct}")
        }
    } else {
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

        let is_kill = ct.contains("BasicKill") || ct.contains("Kill") || ct.contains("Encounter");
        let is_mining_brief = !is_kill
            && (ct.contains("BasicMining")
                || ct.contains("Mining")
                || tmpl_opt.is_some_and(|t| t.title.to_lowercase().contains("mining"))
                || agent_opt.is_some_and(|a| a.division_id == 23));

        if agent_opt.is_some_and(|a| a.agent_type_id == 4)
            && meta.is_none_or(|mt| mt.destination_station_id.is_none())
        {
            let tid = meta.and_then(|mt| mt.required_delivery_type_id).unwrap_or(34);
            let qty = meta
                .map(|mt| mt.required_delivery_qty)
                .filter(|&q| q > 0)
                .unwrap_or_else(|| rnd_tritanium_required_qty(agent_opt.map_or(1, |a| a.level)));
            let item = item_label(tid, "research material");
            format!(
                "Research assignment \"{title}\" for {issuer}: supply {qty}x {item} to {dropoff} in {origin_sys_name} for active R&D work."
            )
        } else if agent_opt.is_some_and(|a| a.agent_type_id == 5) {
            let tid = meta
                .and_then(|mt| mt.required_delivery_type_id)
                .unwrap_or(17202);
            let qty = meta
                .map(|mt| mt.required_delivery_qty)
                .filter(|&q| q > 0)
                .unwrap_or(1);
            let item = item_label(tid, "Pirate Tag");
            format!(
                "Data Center commission \"{title}\" for {issuer}: turn in {qty}x {item} at {dropoff} in {origin_sys_name}."
            )
        } else if ct.contains("Heraldry") {
            format!(
                "Heraldry commission \"{title}\" for {issuer}: complete the Paragon emblem assignment at {dropoff} in {origin_sys_name}."
            )
        } else if ct.contains("TalkToAgent") || ct.contains("AgentInteraction") {
            let arc_prefix = match arc_name.as_deref() {
                Some(n) => format!("Epic Arc ({n})"),
                None => agent_opt
                    .map(|a| format!("Level {} mission", a.level.max(1)))
                    .unwrap_or_else(|| "Standard mission".into()),
            };
            format!(
                "{arc_prefix} liaison \"{title}\" for {issuer}: proceed to {dropoff} in {origin_sys_name} and confer with your designated contact."
            )
        } else if ct.contains("Trade") {
            let derived = tmpl_opt.map(|t| resolve_trade_mission_details(ctx, t));
            let tid = meta
                .and_then(|mt| mt.required_delivery_type_id)
                .or_else(|| derived.as_ref().map(|d| d.trade_type_id))
                .unwrap_or(43);
            let qty = meta
                .map(|mt| mt.required_delivery_qty)
                .filter(|&q| q > 0)
                .or_else(|| derived.map(|d| d.trade_quantity))
                .unwrap_or(20);
            let item = item_label(tid, "trade goods");
            format!(
                "Trade order \"{title}\" for {issuer}: acquire {qty}x {item} and deliver the shipment to {dropoff} in {origin_sys_name}."
            )
        } else if tmpl_opt.is_some_and(|t| t.is_courier || ct.contains("Courier"))
            || meta.is_some_and(|mt| mt.destination_station_id.is_some())
        {
            let derived = tmpl_opt.and_then(|t| {
                agent_opt.and_then(|a| resolve_courier_mission_details(ctx, t, a).ok())
            });
            let tid = meta
                .and_then(|mt| mt.required_delivery_type_id)
                .or_else(|| derived.as_ref().map(|d| d.cargo_type_id))
                .unwrap_or(3814);
            let qty = meta
                .map(|mt| mt.required_delivery_qty)
                .filter(|&q| q > 0)
                .or_else(|| derived.as_ref().map(|d| d.cargo_quantity))
                .unwrap_or(10);
            let cargo = item_label(tid, "diplomatic cargo");
            let dest_sid = meta
                .and_then(|mt| mt.destination_station_id)
                .or_else(|| derived.as_ref().map(|d| d.destination_station_id));
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
        } else if is_mining_brief {
            let derived = tmpl_opt.map(|t| resolve_mining_mission_details(ctx, t));
            let ore_tid = meta
                .and_then(|mt| mt.required_delivery_type_id)
                .or_else(|| derived.as_ref().map(|d| d.ore_type_id))
                .unwrap_or(1230);
            let qty = meta
                .map(|mt| mt.required_delivery_qty)
                .filter(|&q| q > 0)
                .or_else(|| derived.map(|d| d.required_units))
                .unwrap_or(500);
            let ore_name = item_label(ore_tid, "ore");
            format!(
                "Mining operation \"{title}\" for {issuer}: harvest {qty} units of {ore_name} in {sys_name} and haul the ore to {dropoff}."
            )
        } else if let (Some(tmpl), Some(agent)) = (tmpl_opt, agent_opt) {
            let enc = resolve_security_mission_encounter(ctx, tmpl, agent, sys_id);
            let arc_prefix = arc_name
                .as_deref()
                .map(|n| format!("[{n}] "))
                .unwrap_or_default();
            let burner_prefix = if meta.is_some_and(|mt| mt.is_anomic_burner) {
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
    };

    if let Some(branch) = meta
        .and_then(|mt| mt.chosen_branch.as_deref())
        .filter(|b| !b.is_empty())
    {
        text.push_str(&format!(" [Branch: {branch}]"));
    }
    text
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
                snap.active_missions
                    .values()
                    .find(|m| m.character_id == char_id)
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
            let (missions, research) = ctx.db.with_mut_snapshot(|snap| {
                // Sweep and transition any expired offers or accepted missions (feat:pve.mission-expiration)
                let char_mids: Vec<u32> = snap.active_missions.values().filter(|m| m.character_id == char_id && matches!(m.state.as_str(), "Offered" | "Accepted")).map(|m| m.mission_id).collect();
                for mid in char_mids {
                    if let Some(meta) = load_mission_meta(snap, char_id, mid) {
                        let st = snap.active_missions.get(&(char_id, mid)).map(|m| m.state.clone()).unwrap_or_default();
                        if st == "Offered" && meta.offer_expiry_unix > 0 && now > meta.offer_expiry_unix
                        {
                            finalize_mission_state_cleanup(snap, char_id, mid, "Expired", false);
                        } else if st == "Accepted"
                            && meta.accept_expiry_unix.is_some_and(|exp| exp > 0 && now > exp)
                        {
                            finalize_mission_state_cleanup(snap, char_id, mid, "Failed", true);
                        }
                    }
                }

                let active_mission_bms: Vec<(u32, i64)> = snap
                    .active_missions
                    .values()
                    .filter(|om| om.character_id == char_id)
                    .filter_map(|om| om.bookmark_id.map(|bid| (om.mission_id, bid)))
                    .collect();
                let mut active: Vec<ActiveMissionRecord> = snap.active_missions.values().filter(|m| {
                        m.character_id == char_id
                            && matches!(m.state.as_str(), "Offered" | "Accepted")
                            && bound_agent_id.is_none_or(|aid| m.agent_id == aid)
                    }).cloned()
                    .collect();
                active.sort_by_key(|m| m.mission_id);
                let mission_list = active
                    .into_iter().map(|m| {
                        let tmpl = ctx.sde.missions.get(&m.mission_id);
                        let title = tmpl.map(|t| t.title.clone())
                            .unwrap_or_else(|| format!("Mission {}", m.mission_id));
                        let important = tmpl.is_some_and(is_storyline_template);
                        let mission_state = if m.state == "Accepted" { 2_i64 } else { 1_i64 };
                        let meta = load_mission_meta(snap, char_id, m.mission_id);
                        let exp_unix = meta
                            .as_ref().and_then(|mt| {
                                if m.state == "Accepted" { mt.accept_expiry_unix } else { Some(mt.offer_expiry_unix) }
                            }).unwrap_or(now + 86_400);
                        let ct = tmpl.and_then(|t| t.content_template.as_deref()).unwrap_or_default();
                        let arch = classify_mission_archetype(
                            tmpl,
                            meta.as_ref(),
                            ctx.sde.agents.get(&m.agent_id),
                        );
                        let bm_hint = if arch.is_trade || arch.is_data_center || arch.is_rnd_turnin {
                            "UI/Agents/MissionTypes/Trade"
                        } else if arch.is_courier {
                            "UI/Agents/MissionTypes/Courier"
                        } else if !arch.is_heraldry
                            && (tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining"))
                                || ct.contains("Mining"))
                        {
                            "UI/Agents/MissionTypes/Mining"
                        } else {
                            "UI/Agents/MissionTypes/Encounter"
                        };
                        let mut matching_bms: Vec<&eve_db::BookmarkRecord> = snap
                            .bookmarks
                            .values()
                            .filter(|b| {
                                let owned_by_other_mission = active_mission_bms
                                    .iter()
                                    .any(|&(mid, bid)| mid != m.mission_id && bid == b.bookmark_id);
                                b.owner_id == char_id
                                    && (m.bookmark_id == Some(b.bookmark_id)
                                        || (!owned_by_other_mission
                                            && b.solar_system_id == m.solar_system_id
                                            && b.memo.contains("Mission Pocket")))
                            })
                            .collect();
                        matching_bms.sort_by_key(|b| b.bookmark_id);
                        let bm_loc_type = if matches!(
                            bm_hint,
                            "UI/Agents/MissionTypes/Courier" | "UI/Agents/MissionTypes/Trade"
                        ) {
                            "objective.destination"
                        } else {
                            "dungeon"
                        };
                        let bm_created_unix = meta
                            .as_ref()
                            .and_then(|mt| mt.accepted_at_unix.or(Some(mt.offered_at_unix)))
                            .filter(|&ts| ts > 0)
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
                            PyRep::Integer(m.mission_id as i64),
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
                (mission_list, research_list)
            });
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
            let Some((m, meta, bm_coords)) = ctx.db.with_snapshot(|snap| {
                snap.active_missions.get(&(char_id, mission_id)).cloned().map(|m| {
                    let meta = load_mission_meta(snap, char_id, mission_id);
                    let bm_coords = m
                        .bookmark_id
                        .and_then(|bid| snap.bookmarks.get(&bid))
                        .filter(|b| b.item_id.is_none())
                        .map(|b| (b.solar_system_id, b.x, b.y, b.z));
                    (m, meta, bm_coords)
                })
            }) else {
                return Some(not_active_reply(format!(
                    "Mission {mission_id} is not active for character {char_id}"
                )));
            };
            let has_explicit_mission_id = args.get(1).and_then(|a| a.as_i64()).is_some()
                || args.first().and_then(|a| a.as_i64()).is_some_and(|v| {
                    v > 19
                        && !ctx.sde.agents.contains_key(&v)
                        && u32::try_from(v).is_ok_and(|mid| {
                            ctx.sde.missions.contains_key(&mid)
                                || ctx.db.with_snapshot(|snap| {
                                    snap.active_missions.contains_key(&(char_id, mid))
                                })
                        })
                });
            if method != "GetMissionJournalInfo" && !has_explicit_mission_id {
                if (explicit_agent && m.agent_id != agent_id)
                    || !matches!(m.state.as_str(), "Offered" | "Accepted")
                {
                    return Some(not_active_reply(format!(
                        "No active Offered/Accepted mission for character {char_id} with agent {agent_id}"
                    )));
                }
                if m.state == "Offered" {
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
                meta.as_ref(),
            );
            let req_type = meta
                .as_ref().and_then(|mt| mt.required_delivery_type_id).map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None);
            let req_qty = meta.as_ref().map_or(0_i64, |mt| mt.required_delivery_qty as i64);
            let ct = tmpl.and_then(|t| t.content_template.as_deref()).unwrap_or_default();
            let arch = classify_mission_archetype(tmpl, meta.as_ref(), ctx.sde.agents.get(&m.agent_id));
            let is_data_center = arch.is_data_center;
            let is_rnd_turnin = arch.is_rnd_turnin;
            let is_heraldry = arch.is_heraldry;
            let is_trade = arch.is_trade;
            let is_courier = arch.is_courier;
            let is_kill = arch.is_kill;
            let is_mining = !is_kill
                && !is_courier
                && !is_trade
                && !is_heraldry
                && !is_data_center
                && !is_rnd_turnin
                && (tmpl.is_some_and(|t| t.title.to_lowercase().contains("mining"))
                    || ct.contains("BasicMining")
                    || ct.contains("Mining")
                    || ctx.sde.agents.get(&m.agent_id).is_some_and(|a| a.division_id == 23));
            let agent_lvl = ctx.sde.agents.get(&m.agent_id).map_or(1, |a| a.level);
            let objectives = if is_rnd_turnin {
                let item_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(34);
                let item_name = ctx.sde.types.get(&item_tid).map(|t| t.name.as_str()).unwrap_or("Tritanium");
                let qty = if req_qty > 0 { req_qty } else { i64::from(rnd_tritanium_required_qty(agent_lvl)) };
                format!("Deliver {qty} units of {item_name} to agent station hangar.")
            } else if is_data_center {
                let tag_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(17202);
                let tag_name = ctx.sde.types.get(&tag_tid).map(|t| t.name.as_str()).unwrap_or("Pirate Tag");
                let qty = if req_qty > 0 { req_qty } else { 1 };
                if qty == 1 {
                    format!("Deliver 1 {tag_name} to agent station hangar.")
                } else {
                    format!("Deliver {qty} units of {tag_name} to agent station hangar.")
                }
            } else if is_heraldry {
                "Complete heraldry assignment and report back.".to_string()
            } else if is_mining {
                let ore_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(1230);
                let ore_name = ctx.sde.types.get(&ore_tid).map(|t| t.name.as_str()).unwrap_or("ore");
                let qty = if req_qty > 0 { req_qty } else { 500 };
                format!("Mine {qty} units of {ore_name} and deliver to the agent station hangar.")
            } else if is_courier {
                let cargo_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(3683);
                let cargo_name = ctx.sde.types.get(&cargo_tid).map(|t| t.name.as_str()).unwrap_or("cargo");
                let dest_name = meta.as_ref().and_then(|mt| mt.destination_station_id)
                    .and_then(|sid| ctx.sde.stations.get(&sid))
                    .map(|s| s.name.as_str())
                    .unwrap_or("destination station");
                let qty = if req_qty > 0 { req_qty } else { 10 };
                format!("Transport {qty} units of {cargo_name} to {dest_name}.")
            } else if is_trade {
                let trade_tid = meta.as_ref().and_then(|mt| mt.required_delivery_type_id).unwrap_or(3683);
                let trade_name = ctx.sde.types.get(&trade_tid).map(|t| t.name.as_str()).unwrap_or("trade goods");
                let qty = if req_qty > 0 { req_qty } else { 10 };
                format!("Acquire and deliver {qty} units of {trade_name} to the agent station hangar.")
            } else {
                format!("Eliminate hostile pirate vessels ({} targets remaining) and report back.", m.npcs_remaining)
            };
            let mission_state = match m.state.as_str() {
                "Offered" => 1_i64,
                "Accepted" => 2_i64,
                "Failed" | "Expired" => 3_i64,
                "Completed" => 4_i64,
                "Declined" => 5_i64,
                "Quit" => 6_i64,
                _ => 0_i64,
            };
            let offer_lvl = meta
                .as_ref()
                .and_then(|mt| mt.offered_level)
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
            let bonus_expiry_unix = meta.as_ref().and_then(|mt| mt.bonus_expiry_unix);
            let bonus_reward = if bonus_expiry_unix.is_some_and(|bexp| now > bexp) {
                0
            } else {
                raw_bonus_reward
            };
            let origin_sid_opt = meta
                .as_ref()
                .and_then(|mt| mt.origin_station_id)
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
            let dest_sid_opt = meta.as_ref().and_then(|mt| mt.destination_station_id);
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
            let has_dungeon_site = !is_courier && !is_trade && !is_data_center && !is_rnd_turnin;
            let dun_sys = bm_coords
                .map(|(sid, _, _, _)| sid)
                .filter(|&sid| sid > 0)
                .or_else(|| (m.solar_system_id > 0).then_some(m.solar_system_id))
                .unwrap_or(sys_id);
            let dungeon_loc = has_dungeon_site.then(|| {
                let (dx, dy, dz) = bm_coords
                    .map(|(_, x, y, z)| (x, y, z))
                    .unwrap_or_else(|| {
                        let origin = deterministic_mission_site_origin(ctx, dun_sys, mission_id);
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
                || (!is_courier
                    && !is_trade
                    && !is_mining
                    && !is_data_center
                    && !is_rnd_turnin
                    && m.npcs_remaining == 0)
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
                let req_tid_opt = meta.as_ref().and_then(|mt| mt.required_delivery_type_id);
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
                } else if is_rnd_turnin || is_data_center || is_mining || is_trade {
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
                    } else if is_mining {
                        let md = tmpl.map(|t| resolve_mining_mission_details(ctx, t));
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
                        let td = tmpl.map(|t| resolve_trade_mission_details(ctx, t));
                        (
                            i64::from(
                                req_tid_opt
                                    .or_else(|| td.as_ref().map(|d| d.trade_type_id))
                                    .unwrap_or(3683),
                            ),
                            if req_qty > 0 {
                                req_qty
                            } else {
                                td.map_or(10, |d| i64::from(d.trade_quantity))
                            },
                        )
                    };
                    let fetch_req_tid = u32::try_from(fetch_tid).unwrap_or(0);
                    let agent_station_id = ctx
                        .sde
                        .agents
                        .get(&m.agent_id)
                        .and_then(|a| a.station_id)
                        .or_else(|| meta.as_ref().and_then(|mt| mt.origin_station_id));
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
                ("state", PyRep::String(m.state)),
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
                ("npcsRemaining", PyRep::Integer(m.npcs_remaining as i64)),
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
            let (mut agent_id, mut explicit_agent) =
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
                    let supplied_id = if args
                        .first().and_then(|a| a.as_i64())
                        .is_some_and(|v| (1..=19).contains(&v))
                    {
                        args.get(1).and_then(|a| a.as_i64())
                    } else {
                        args.first()
                            .and_then(|a| a.as_i64())
                            .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                            .or_else(|| _kwargs.get("agentID").and_then(|a| a.as_i64()))
                    };
                    if let Some(sid) = supplied_id.filter(|_| bound_agent_id.is_none()) {
                        agent_id = sid;
                        explicit_agent = true;
                    }
                    let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
                        return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
                    };
                    if is_implicit_greeting {
                        let existing_accepted = ctx.db.with_snapshot(|snap| {
                            snap.active_missions
                                .values()
                                .find(|m| {
                                    m.character_id == char_id
                                        && m.agent_id == agent_id
                                        && m.state == "Accepted"
                                })
                                .cloned()
                        });
                        if let Some(acc) = existing_accepted {
                            let mid = acc.mission_id;
                            let title = ctx
                                .sde
                                .missions
                                .get(&mid)
                                .map(|t| t.title.clone())
                                .unwrap_or_else(|| format!("Mission {mid}"));
                            let msg = format!(
                                "Your assignment '{title}' is currently in progress. Return here once your objectives are complete."
                            );
                            let mut dlg = vec![
                                PyRep::Tuple(vec![
                                    PyRep::Integer(6),
                                    PyRep::String("Complete Mission".into()),
                                ]),
                                PyRep::Tuple(vec![
                                    PyRep::Integer(11),
                                    PyRep::String("Quit Mission".into()),
                                ]),
                            ];
                            if agent_info.is_locator {
                                dlg.push(PyRep::Tuple(vec![
                                    PyRep::Integer(15),
                                    PyRep::String("Locate Character".into()),
                                ]));
                            }
                            return Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mid as i64)),
                                ("contentID", PyRep::Integer(mid as i64)),
                                ("agentID", PyRep::Integer(agent_id)),
                                ("title", PyRep::String(title)),
                                ("state", PyRep::String("Accepted".into())),
                                ("missionState", PyRep::Integer(2)),
                                ("briefing", PyRep::String(msg.clone())),
                                ("agentSays", PyRep::String(msg)),
                                ("dialogue", PyRep::List(dlg)),
                            ])));
                        }
                    }

                    let station_has_local_agents = session.station_id.is_some_and(|sid| {
                        ctx.sde.agents.values().any(|a| {
                            a.solar_system_id == Some(sys_id)
                                && a.station_id == Some(sid)
                                && !is_storyline_agent_type(a.agent_type_id)
                                && !is_unsupported_direct_agent_type(a.agent_type_id)
                        })
                    });
                    if agent_info.solar_system_id != Some(sys_id)
                        || (session.station_id.is_some()
                            && (explicit_agent || station_has_local_agents)
                            && agent_info.station_id.is_some()
                            && agent_info.station_id != session.station_id)
                    {
                        let sys_label = agent_info
                            .solar_system_id
                            .map(|sid| {
                                ctx.sde
                                    .solar_systems
                                    .get(&sid)
                                    .map(|s| s.name.clone())
                                    .unwrap_or_else(|| sid.to_string())
                            })
                            .unwrap_or_else(|| "unknown system".to_string());
                        let station_label = agent_info
                            .station_id
                            .map(|stid| {
                                ctx.sde
                                    .stations
                                    .get(&stid)
                                    .map(|s| s.name.clone())
                                    .unwrap_or_else(|| stid.to_string())
                            })
                            .unwrap_or_else(|| "space".to_string());
                        if is_implicit_greeting {
                            let msg = format!(
                                "Please dock at my station in {sys_label} ({station_label}) if you wish to request a new assignment."
                            );
                            return Some(Ok(PyRep::keyval([
                                ("agentID", PyRep::Integer(agent_id)),
                                ("state", PyRep::String("NotCoLocated".into())),
                                ("missionState", PyRep::Integer(0)),
                                ("briefing", PyRep::String(msg.clone())),
                                ("agentSays", PyRep::String(msg)),
                                ("dialogue", PyRep::List(Vec::new())),
                            ])));
                        }
                        return Some(Err(BridgeRpcError { code: "AgentNotCoLocated".into(), message: format!("Agent {agent_id} is in system {sys_label} / station {station_label}, not character's current location (system {sys_id}, station {:?})", session.station_id), client_error: None }));
                    }
                    if is_storyline_agent_type(agent_info.agent_type_id) {
                        return Some(Err(BridgeRpcError { code: "StorylineAgentRequiresReferral".into(), message: format!("Storyline agent {agent_id} (agentType {}) cannot be requested directly without a 16-mission referral", agent_info.agent_type_id), client_error: None }));
                    }
                    // V24 parity (`agents.pyj:OpenDialogueWindow`): `agentTypeAura = 11` rejects tutorial agent
                    // dialogue with `NewNPEClickOnTutorialLinkDisabled`; locked as `UnimplementedRPC` by
                    // `auth_persistence` and `agents_missions_extra_unit_tests` (#1204).
                    if agent_info.agent_type_id == 11 {
                        return Some(Err(BridgeRpcError { code: "UnimplementedRPC".into(), message: format!("Agent {agent_id} (agentType {}) uses Aura node-graph missions which are not yet implemented", agent_info.agent_type_id), client_error: None }));
                    }
                    let now = ctx.now_unix();
                    if agent_info.agent_type_id == 10 {
                        let (arc_id, _) = resolve_epic_arc_id_for_agent(ctx, agent_id);
                        let on_cd = ctx.db.with_snapshot(|snap| {
                            load_epic_arc_states(snap, char_id).iter().any(|a| {
                                a.epic_arc_id == arc_id
                                    && a.cooldown_until_unix.is_some_and(|until| now < until)
                            })
                        });
                        if on_cd {
                            return Some(Err(BridgeRpcError { code: "EpicArcOnCooldown".into(), message: format!("Epic Arc {arc_id} (agent {agent_id}) is on 90-day repeat cooldown"), client_error: None }));
                        }
                    }
                    if agent_info.agent_type_id == 9 {
                        let req_fac = ctx.sde.factions.values().find(|f| f.militia_corporation_id == Some(agent_info.corporation_id)).map(|f| f.faction_id as i64);
                        let ch_fac = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.war_faction_id));
                        if req_fac.is_some() && ch_fac != req_fac {
                            return Some(Err(BridgeRpcError { code: "NotEnlistedInMilitia".into(), message: format!("FW agent {agent_id} requires enlistment in militia faction {req_fac:?} (have {ch_fac:?})"), client_error: None }));
                        }
                    }
                    if matches!(agent_info.agent_type_id, 5 | 8 | 12) {
                        let already_done = ctx.db.with_snapshot(|snap| {
                            snap.active_missions.values().any(|m| {
                                m.character_id == char_id
                                    && m.agent_id == agent_id
                                    && m.state == "Completed"
                            })
                        });
                        if already_done {
                            return Some(Err(BridgeRpcError { code: "OneTimeAgentMissionCompleted".into(), message: format!("One-time COSMOS / Data Center / Career agent {agent_id} mission has already been completed"), client_error: None }));
                        }
                    }

                    if matches!(agent_info.agent_type_id, 5 | 8 | 12) {
                        let cosmos_expired = ctx.db.with_snapshot(|snap| {
                            snap.active_missions.values().any(|m| {
                                m.character_id == char_id && m.agent_id == agent_id && m.state == "Expired"
                            })
                        });
                        if cosmos_expired {
                            return Some(Err(BridgeRpcError {
                                code: "CosmosMissionExpired".into(),
                                message: format!("One-time COSMOS agent {agent_id} mission offer has expired and cannot be re-offered"),
                                client_error: None,
                            }));
                        }
                    }

                    let is_alpha = ctx.db.with_snapshot(|snap| {
                        snap.characters.get(&char_id).is_some_and(|c| c.clone_grade <= 1)
                    });
                    if is_alpha && agent_info.level >= 4 {
                        return Some(Err(BridgeRpcError {
                            code: "CantDoMissionAlphaClone".into(),
                            message: format!("Agent level {} requires Omega clone state", agent_info.level),
                            client_error: None,
                        }));
                    }

                    if agent_info.level == 5 {
                        let agent_sys = agent_info.solar_system_id.unwrap_or(sys_id);
                        let sec = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.security_status).unwrap_or(1.0);
                        if sec >= 0.45 {
                            return Some(Err(BridgeRpcError {
                                code: "CantDoMission".into(),
                                message: format!("Level 5 agent {agent_id} cannot operate in high-security space ({sec:.2})"),
                                client_error: None,
                            }));
                        }
                    }

                    let faction_id = ctx.sde.npc_corporations.get(&agent_info.corporation_id).and_then(|c| c.faction_id).map(|fid| fid as i64);
                    let effective_standing = ctx.db.with_snapshot(|snap| {
                        compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent_info.corporation_id, faction_id)
                    });
                    if agent_info.level > 1 {
                        let required_standing = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
                        if effective_standing < required_standing {
                            if is_implicit_greeting {
                                let refusal = format!(
                                    "I am sorry, capsuleer, but I only offer assignments to pilots with an effective standing of at least {required_standing:.1} (yours is {effective_standing:.2})."
                                );
                                return Some(Ok(PyRep::keyval([
                                    ("agentID", PyRep::Integer(agent_id)),
                                    ("state", PyRep::String("Refused".into())),
                                    ("missionState", PyRep::Integer(0)),
                                    ("briefing", PyRep::String(refusal.clone())),
                                    ("agentSays", PyRep::String(refusal)),
                                    ("dialogue", PyRep::List(Vec::new())),
                                ])));
                            }
                            return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level {}) requires agent/corporation/faction standing >= {required_standing:.1}, character has {effective_standing:.2}", agent_info.level), client_error: None }));
                        }
                    } else if !matches!(agent_info.agent_type_id, 7 | 12) && effective_standing <= -2.0 {
                        if is_implicit_greeting {
                            let refusal = format!(
                                "I do not work with pilots whose standing has fallen so low ({effective_standing:.2}). Improve your standing above -2.0 first."
                            );
                            return Some(Ok(PyRep::keyval([
                                ("agentID", PyRep::Integer(agent_id)),
                                ("state", PyRep::String("Refused".into())),
                                ("missionState", PyRep::Integer(0)),
                                ("briefing", PyRep::String(refusal.clone())),
                                ("agentSays", PyRep::String(refusal)),
                                ("dialogue", PyRep::List(Vec::new())),
                            ])));
                        }
                        return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level 1) requires standing > -2.0, character has {effective_standing:.2}"), client_error: None }));
                    }

                    let is_rnd = is_rnd_agent(agent_info);
                    if is_rnd {
                        let has_research = ctx.db.with_snapshot(|snap| {
                            load_rnd_research_entries(snap, char_id).iter().any(|r| r.agent_id == agent_id)
                        });
                        if !has_research {
                            if is_implicit_greeting {
                                let greeting = "Greetings, capsuleer. We do not have an active research project yet, but we can begin one whenever you are ready.".to_string();
                                return Some(Ok(PyRep::keyval([
                                    ("agentID", PyRep::Integer(agent_id)),
                                    ("state", PyRep::String("Greeting".into())),
                                    ("missionState", PyRep::Integer(0)),
                                    ("briefing", PyRep::String(greeting.clone())),
                                    ("agentSays", PyRep::String(greeting)),
                                    (
                                        "dialogue",
                                        PyRep::List(vec![PyRep::Tuple(vec![
                                            PyRep::Integer(12),
                                            PyRep::String("Start Research".into()),
                                        ])]),
                                    ),
                                ])));
                            }
                            return Some(Err(BridgeRpcError {
                                code: "ResearchNotActive".into(),
                                message: format!("Agent {agent_id} requires active research to offer daily missions"),
                                client_error: None,
                            }));
                        }
                    }

                    let offer_dur = 604_800_u64;

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        if let Some(existing_active) = snap.active_missions.values().find(|m| {
                            m.character_id == char_id
                                && m.agent_id == agent_id
                                && m.state == "Offered"
                        }) {
                            let mid = existing_active.mission_id;
                            let title = ctx.sde.missions.get(&mid).map(|t| t.title.clone()).unwrap_or_default();
                            let meta = load_mission_meta(snap, char_id, mid);
                            let dest_sid = meta.as_ref().and_then(|m| m.destination_station_id);
                            let briefing = build_mission_briefing_text(
                                ctx,
                                agent_id,
                                mid,
                                &title,
                                existing_active.solar_system_id,
                                meta.as_ref(),
                            );
                            let objectives = format!("Execute mission objectives for {} and report back.", title);
                            return Ok((mid, title, briefing, objectives, dest_sid));
                        }
                        let is_heraldry_agent = agent_info.agent_type_id == 13 || agent_info.division_id == 37;
                        let next_tmpl = if is_heraldry_agent && explicit_agent {
                            ctx.sde.missions
                                .values().filter(|m| {
                                     m.content_template.as_deref().unwrap_or_default().contains("Heraldry")
                                        && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                }).min_by_key(|m| m.mission_id)
                        } else {
                            ctx.sde.missions
                                .values().filter(|m| {
                                    mission_matches_agent_pool_with_sde(&ctx.sde, m, agent_info)
                                        && m.has_standing_rewards
                                        && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                }).min_by_key(|m| m.mission_id).or_else(|| {
                                    ctx.sde.missions
                                        .values().filter(|m| {
                                            mission_matches_agent_pool_with_sde(&ctx.sde, m, agent_info)
                                                && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                                        }).min_by_key(|m| m.mission_id)
                                })
                        };
                        let Some(tmpl) = next_tmpl else {
                            return Err(BridgeRpcError { code: "NoMissionsAvailable".into(), message: format!("No unoffered missions remaining in SDE pool for agent {agent_id} and character {char_id}"), client_error: None });
                        };
                        let mission_id = tmpl.mission_id;
                        let title = tmpl.title.clone();
                        let ct = tmpl.content_template.as_deref().unwrap_or_default();
                        let arch = classify_mission_archetype(Some(tmpl), None, Some(agent_info));
                        let is_heraldry = arch.is_heraldry;
                        let is_trade = arch.is_trade;
                        let is_courier = arch.is_courier;
                        let is_data_center = arch.is_data_center;
                        let is_kill = arch.is_kill;
                        let is_mining = !is_kill && !is_trade && !is_courier && !is_heraldry && (ct.contains("BasicMining")
                            || tmpl.title.to_lowercase().contains("mining")
                            || agent_info.division_id == 23);

                        let target_sys = if is_fw_agent(agent_info) {
                            resolve_fw_warzone_system(agent_info.corporation_id)
                        } else {
                            sys_id
                        };
                        let (req_tid, req_qty, origin_sid, dest_sid, npcs_remaining, objectives) = if is_rnd {
                            let qty = rnd_tritanium_required_qty(agent_info.level);
                            (Some(34), qty, agent_info.station_id, None, 0, format!("Deliver {qty} units of Tritanium to agent station hangar."))
                        } else if is_data_center {
                            (Some(17202), 1, agent_info.station_id, None, 0, "Deliver 1 Pirate Tag to agent station hangar.".to_string())
                        } else if is_courier {
                            let cd = resolve_courier_mission_details(ctx, tmpl, agent_info)?;
                            let cargo_name = ctx.sde.types.get(&cd.cargo_type_id).map(|t| t.name.as_str()).unwrap_or("cargo");
                            let dest_name = ctx.sde.stations.get(&cd.destination_station_id).map(|s| s.name.as_str()).unwrap_or("destination station");
                            (Some(cd.cargo_type_id), cd.cargo_quantity, Some(cd.origin_station_id), Some(cd.destination_station_id), 0, format!("Transport {} units of {} to {}.", cd.cargo_quantity, cargo_name, dest_name))
                        } else if is_trade {
                            let td = resolve_trade_mission_details(ctx, tmpl);
                            let trade_name = ctx.sde.types.get(&td.trade_type_id).map(|t| t.name.as_str()).unwrap_or("goods");
                            (Some(td.trade_type_id), td.trade_quantity, agent_info.station_id, None, 0, format!("Acquire and deliver {} units of {} to the agent station hangar.", td.trade_quantity, trade_name))
                        } else if is_mining {
                            let md = resolve_mining_mission_details(ctx, tmpl);
                            (Some(md.ore_type_id), md.required_units, agent_info.station_id, None, 0, md.objective_text)
                        } else if is_heraldry {
                            (None, 0, agent_info.station_id, None, 0, "Complete heraldry assignment and report back.".to_string())
                        } else {
                            let enc = resolve_security_mission_encounter(ctx, tmpl, agent_info, target_sys);
                            (None, 0, agent_info.station_id, None, enc.wave_npc_count, enc.objective_text)
                        };

                        let is_burner = title.contains("Anomic")
                            || (agent_info.level >= 4 && agent_info.division_id == 24);
                        let offer_meta = MissionTimingAndCargoMeta {
                            mission_id,
                            agent_id,
                            offered_at_unix: now,
                            offer_expiry_unix: now + offer_dur,
                            accepted_at_unix: None,
                            accept_expiry_unix: None,
                            bonus_expiry_unix: None,
                            completed_at_unix: None,
                            required_delivery_type_id: req_tid,
                            required_delivery_qty: req_qty,
                            granted_courier_item_id: None,
                            granted_courier_qty: 0,
                            is_anomic_burner: is_burner,
                            chosen_branch: None,
                            destination_station_id: dest_sid,
                            origin_station_id: origin_sid,
                            offered_level: Some(agent_info.level),
                        };
                        let briefing = build_mission_briefing_text(
                            ctx,
                            agent_id,
                            mission_id,
                            &title,
                            target_sys,
                            Some(&offer_meta),
                        );
                        save_mission_meta(snap, char_id, &offer_meta);
                        snap.active_missions.insert((char_id, mission_id), ActiveMissionRecord { mission_id, character_id: char_id, agent_id, solar_system_id: target_sys, state: "Offered".into(), current_room_id: 1, npcs_remaining, spawned_npc_ball_ids: Vec::new(), bookmark_id: None });
                        Ok((mission_id, title, briefing, objectives, dest_sid))
                    });

                    match res {
                        Ok((mission_id, title, briefing, objectives, dest_sid)) => {
                            let expiry_unix = now + offer_dur;
                            let dialogue = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]), PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())])]);
                            let CharacterMissionRewards {
                                isk_reward,
                                lp_reward,
                                bonus_reward,
                            } = compute_character_mission_rewards(ctx, char_id, agent_id, mission_id, sys_id);
                            let bonus_window_secs =
                                mission_bonus_window_secs(ctx, char_id, agent_id, mission_id);
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnAgentMissionChange {
                                    action: "offered".into(),
                                    agent_id,
                                },
                            );
                            let clean_header = title.trim_start_matches("[d]").trim();
                            let header_str = if clean_header.is_empty() { "Agent Transmission" } else { clean_header };
                            let trans_kv = PyRep::keyval([("header", PyRep::String(header_str.to_string())), ("text", PyRep::String(briefing.clone())), ("icon", PyRep::String("res:/UI/Texture/WindowIcons/agent.png".into())), ("agentID", PyRep::Integer(agent_id))]);
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnIncomingTransmission {
                                    transmission: trans_kv,
                                    is_agent_mission: true,
                                },
                            );
                            let mut offer_kvs = vec![
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(agent_id)),
                                ("title", PyRep::String(title)),
                                ("state", PyRep::String("Offered".into())),
                                ("missionState", PyRep::Integer(1)),
                                ("briefing", PyRep::String(briefing.clone())),
                                ("agentSays", PyRep::String(briefing)),
                                ("objectives", PyRep::String(objectives)),
                                ("dialogue", dialogue),
                                ("iskReward", PyRep::Integer(isk_reward)),
                                ("lpReward", PyRep::Integer(lp_reward)),
                                ("bonusReward", PyRep::Integer(bonus_reward)),
                                ("bonusWindowSecs", PyRep::Integer(bonus_window_secs as i64)),
                                ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                            ];
                            if let Some(dsid) = dest_sid {
                                offer_kvs.push(("destinationStationID", PyRep::Integer(dsid)));
                            }
                            Some(Ok(PyRep::keyval(offer_kvs)))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                // 3 = Accept, 4 = AcceptChoice, 5 = AcceptRemotely
                3..=5 => {
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some("Offered"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let (is_alpha, agent_lvl, actual_agent_id) = ctx.db.with_snapshot(|snap| {
                        let alpha = snap.characters.get(&char_id).is_some_and(|c| c.clone_grade <= 1);
                        let mid_agent = snap.active_missions.get(&(char_id, mission_id)).map(|m| m.agent_id).unwrap_or(agent_id);
                        let lvl = ctx.sde.agents.get(&mid_agent).map_or(1, |a| a.level);
                        (alpha, lvl, mid_agent)
                    });
                    if is_alpha && agent_lvl >= 4 {
                        return Some(Err(BridgeRpcError {
                            code: "CantDoMissionAlphaClone".into(),
                            message: format!("Agent level {agent_lvl} requires Omega clone state"),
                            client_error: None,
                        }));
                    }
                    let tmpl_opt = ctx.sde.missions.get(&mission_id).cloned();
                    let agent_info_opt = ctx.sde.agents.get(&actual_agent_id).cloned();
                    if let Some(ref agent_info) = agent_info_opt {
                        let colocated = is_agent_colocated(ctx, session, sys_id, agent_info).await;
                        if matches!(action_id, 3 | 4) && !colocated {
                            return Some(Err(BridgeRpcError {
                                code: "AgentNotCoLocated".into(),
                                message: format!(
                                    "Agent {actual_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                                    agent_info.solar_system_id, agent_info.station_id, session.station_id
                                ),
                                client_error: None,
                            }));
                        }
                        if action_id == 5 && !colocated {
                            let is_burner = ctx.db.with_snapshot(|snap| {
                                load_mission_meta(snap, char_id, mission_id)
                                    .is_some_and(|mt| mt.is_anomic_burner)
                            });
                            let remote_eligible = is_storyline_agent_type(agent_info.agent_type_id)
                                || tmpl_opt.as_ref().is_some_and(is_storyline_template)
                                || matches!(agent_info.agent_type_id, 5 | 8)
                                || is_burner;
                            if !remote_eligible {
                                return Some(Err(BridgeRpcError {
                                    code: "CannotAcceptRemotely".into(),
                                    message: format!(
                                        "Mission {mission_id} from agent {actual_agent_id} requires visiting the agent to accept"
                                    ),
                                    client_error: None,
                                }));
                            }
                        }
                    }
                    let mission_sys_id = ctx
                        .db
                        .with_snapshot(|snap| {
                            snap.active_missions
                                .get(&(char_id, mission_id))
                                .map(|m| m.solar_system_id)
                                .filter(|&s| s > 0)
                        })
                        .or_else(|| {
                            agent_info_opt
                                .as_ref()
                                .and_then(|a| a.solar_system_id)
                                .filter(|&s| s > 0)
                        })
                        .unwrap_or(sys_id);
                    let enc_opt = tmpl_opt.as_ref().and_then(|t| {
                        agent_info_opt
                            .as_ref()
                            .map(|a| resolve_security_mission_encounter(ctx, t, a, mission_sys_id))
                    });
                    let handle = ctx.systems.get_or_spawn(mission_sys_id);
                    let existing_accepted = ctx.db.with_snapshot(|snap| {
                        snap.active_missions
                            .get(&(char_id, mission_id))
                            .filter(|m| m.state == "Accepted" && m.npcs_remaining > 0)
                            .map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone()))
                    });
                    let can_respawn_missing = if let Some((m_sys_id, prev_balls)) = existing_accepted {
                        if m_sys_id == mission_sys_id && !prev_balls.is_empty() {
                            let m_handle = ctx.systems.get_or_spawn(m_sys_id);
                            let mut all_missing = true;
                            for bid in prev_balls {
                                if m_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                                    all_missing = false;
                                    break;
                                }
                            }
                            all_missing
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    let now = ctx.now_unix();
                    let accept_dur = 604_800_u64;
                    let bonus_dur =
                        mission_bonus_window_secs(ctx, char_id, actual_agent_id, mission_id);

                    let m_info = ctx.db.with_snapshot(|snap| {
                        let m = snap.active_missions.get(&(char_id, mission_id))?;
                        let meta = load_mission_meta(snap, char_id, mission_id)?;
                        let is_expired = meta.offer_expiry_unix > 0 && now > meta.offer_expiry_unix;
                        Some((m.state.clone(), m.solar_system_id, is_expired))
                    });
                    if let Some((state, sys, is_expired)) = m_info {
                        if state == "Offered" {
                            if is_expired {
                                ctx.db.with_mut_snapshot(|snap| {
                                    finalize_mission_state_cleanup(snap, char_id, mission_id, "Expired", false);
                                });
                                return Some(Err(BridgeRpcError { code: "MissionOfferExpired".into(), message: format!("Mission {mission_id} offer has expired"), client_error: None }));
                            }
                        } else if !(state == "Accepted" && sys == mission_sys_id && can_respawn_missing) {
                            return Some(Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{state}' (system {sys}), not Offered"), client_error: None }));
                        }
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let Some(m_check) = snap.active_missions.get(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotOffered".into(), message: format!("Mission {mission_id} has not been offered to character {char_id}"), client_error: None });
                        };
                        if m_check.state != "Offered" && !(m_check.state == "Accepted" && m_check.solar_system_id == mission_sys_id && can_respawn_missing) {
                            return Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{}' (system {}), not Offered", m_check.state, m_check.solar_system_id), client_error: None });
                        }
                        let mut spawned_npcs: Vec<(i64, eve_db::TypeId)> = Vec::new();
                        let mut need_npc = false;
                        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m.state = "Accepted".into();
                            if m.solar_system_id == 0 {
                                m.solar_system_id = mission_sys_id;
                            }
                            need_npc = m.npcs_remaining > 0;
                        }
                        if need_npc {
                            let count = enc_opt.as_ref().map(|e| e.wave_npc_count).unwrap_or(1) as usize;
                            let pool = enc_opt.as_ref().map(|e| e.npc_type_ids.as_slice()).unwrap_or(&[]);
                            let mut ball_ids = Vec::with_capacity(count);
                            for i in 0..count {
                                let npc_id = snap.alloc_entity_id();
                                let tid = if !pool.is_empty() { pool[i % pool.len()] } else { resolve_mission_npc_type(ctx) };
                                ball_ids.push(npc_id);
                                spawned_npcs.push((npc_id, tid));
                            }
                            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                                m.npcs_remaining = count as u32;
                                m.spawned_npc_ball_ids = ball_ids;
                            }
                            let wave_tids: Vec<u32> = spawned_npcs.iter().map(|&(_, tid)| tid).collect();
                            snap.sp6_lifecycle.mission_initial_npc_counts.insert(format!("{char_id}:{mission_id}"), count as u32);
                            snap.sp6_lifecycle.mission_initial_npc_types.insert(format!("{char_id}:{mission_id}"), wave_tids);
                        }
                        let mut granted_courier_item: Option<InventoryItemRecord> = None;
                        let mut final_accept_expiry = now + accept_dur;
                        let mut final_bonus_expiry = now + bonus_dur;
                        if let Some(mut meta) = load_mission_meta(snap, char_id, mission_id) {
                            meta.accepted_at_unix.get_or_insert(now);
                            final_accept_expiry = *meta.accept_expiry_unix.get_or_insert(now + accept_dur);
                            final_bonus_expiry = *meta.bonus_expiry_unix.get_or_insert(now + bonus_dur);
                            let agent_opt = ctx.sde.agents.get(&meta.agent_id);
                            let is_courier = classify_mission_archetype(
                                tmpl_opt.as_ref(),
                                Some(&meta),
                                agent_opt,
                            )
                            .is_courier;
                            if is_courier && meta.granted_courier_item_id.is_none() {
                                let cargo_tid = meta.required_delivery_type_id.ok_or_else(|| BridgeRpcError { code: "InvalidMissionLocation".into(), message: format!("Courier mission {mission_id} has no delivery item type specified"), client_error: None })?;
                                let cargo_qty = meta.required_delivery_qty.max(1);
                                let agent_station = meta.origin_station_id
                                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.station_id))
                                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.solar_system_id).and_then(|sys| ctx.sde.stations.values().filter(|st| st.solar_system_id == sys).map(|st| st.station_id).min()))
                                    .ok_or_else(|| BridgeRpcError { code: "InvalidMissionLocation".into(), message: "Agent has no valid station location".into(), client_error: None })?;
                                snap.next_entity_id += 1;
                                let cid = snap.next_entity_id;
                                let item = InventoryItemRecord { item_id: cid, type_id: cargo_tid, owner_id: char_id, location_id: agent_station, flag: InventoryFlag::Hangar, quantity: cargo_qty, singleton: false, version: 1, ..Default::default() };
                                snap.items.insert(cid, item.clone());
                                snap.item_custom_names.insert(cid, format!("courier_cargo:{char_id}:{mission_id}"));
                                meta.granted_courier_item_id = Some(cid);
                                meta.granted_courier_qty = cargo_qty;
                                granted_courier_item = Some(item);
                            }
                            save_mission_meta(snap, char_id, &meta);
                        }
                        Ok((spawned_npcs, granted_courier_item, final_accept_expiry, final_bonus_expiry))
                    });
                    let (spawned_npcs, granted_courier_item, expiry_unix, bonus_expiry_unix) = match res { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    if let Some(ref courier_item) = granted_courier_item {
                        ctx.notify_item_mutation(char_id, None, Some(courier_item));
                    }
                    let first_npc_id = spawned_npcs.first().map(|(id, _)| *id);

                    let site_origin = deterministic_mission_site_origin(ctx, mission_sys_id, mission_id);
                    if !spawned_npcs.is_empty() {
                        for (idx, (npc_id, tid)) in spawned_npcs.into_iter().enumerate() {
                            let pocket_offset = ((npc_id % 40) as f64) * 35.0 + (idx as f64) * 350.0;
                            let pos = site_origin + DVec3::new(800.0 + pocket_offset, (idx as f64) * 120.0, 0.0);
                            let (ball, cs, mut entity) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(ctx, mission_sys_id, npc_id, tid, "Pirate", 1, pos);
                            entity.bounty_isk = entity.bounty_isk.max(10_000.0);
                            let _ = handle.spawn_ball_with_combat(ball, cs).await;
                            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                                rt.npc_entities.insert(npc_id, entity);
                            });
                        }
                    }

                    let is_mining_mission = ctx.db.with_snapshot(|snap| {
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        let agent_opt = meta_opt.as_ref().and_then(|m| ctx.sde.agents.get(&m.agent_id));
                        !agent_opt.is_some_and(is_rnd_agent)
                            && ctx.sde.missions.get(&mission_id).is_some_and(|m| {
                                let arch = classify_mission_archetype(Some(m), meta_opt.as_ref(), agent_opt);
                                if arch.is_kill || arch.is_trade || arch.is_courier || arch.is_heraldry || arch.is_data_center {
                                    return false;
                                }
                                let agent_div = agent_opt.map(|a| a.division_id).unwrap_or(0);
                                let t = m.title.to_lowercase();
                                let c_lower = m.content_template.as_deref().unwrap_or_default().to_lowercase();
                                agent_div == 23 || c_lower.contains("mining") || t.contains("mining") || t.contains("mineral") || t.contains("ore")
                            })
                    });
                    if is_mining_mission {
                        let ore_tid = ctx.db.with_snapshot(|snap| load_mission_meta(snap, char_id, mission_id).and_then(|m| m.required_delivery_type_id).unwrap_or(1230));
                        let ore_vol = ctx.sde.types.get(&ore_tid).map(|t| t.volume.max(0.01)).unwrap_or(0.1);
                        let ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
                        let ast_pos = site_origin + DVec3::new(8_000.0, 1_000.0, -5_000.0);
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.asteroids.insert(ast_id, eve_db::AsteroidRecord { asteroid_id: ast_id, solar_system_id: mission_sys_id, belt_id: 0, type_id: ore_tid, quantity: 5_000, initial_quantity: 5_000, position: (ast_pos.x, ast_pos.y, ast_pos.z), radius: 50.0 });
                        });
                        let ast_ball = Ball { id: ast_id, type_id: ore_tid, position: ast_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
                        let ast_state = crate::system_actor::AsteroidState { asteroid_id: ast_id, belt_id: 0, type_id: ore_tid, initial_units: 5_000, remaining_units: 5_000, unit_volume: ore_vol, depleted: false, respawn_at_tick: None };
                        let _ = handle.spawn_asteroid(ast_state, ast_ball).await;
                    }

                    let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone()).unwrap_or_else(|| format!("Mission {mission_id}"));
                    let bookmark_id = ctx.db.with_mut_snapshot(|snap| {
                        snap.next_entity_id += 1;
                        let bmid = snap.next_entity_id;
                        let b_sys = snap.active_missions.get(&(char_id, mission_id)).map(|m| m.solar_system_id).filter(|&s| s > 0).unwrap_or(mission_sys_id);
                        snap.bookmarks.insert(bmid, eve_db::BookmarkRecord { bookmark_id: bmid, owner_id: char_id, solar_system_id: b_sys, label: format!("Mission: {mission_title}"), x: site_origin.x, y: site_origin.y, z: site_origin.z, item_id: None, memo: "Mission Pocket Destination".into(), folder_id: None, is_shared: false });
                        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                            m.bookmark_id = Some(bmid);
                        }
                        bmid
                    });
                    let dialogue = PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::String("Complete Mission".into())]),
                        PyRep::Tuple(vec![PyRep::Integer(11), PyRep::String("Quit Mission".into())]),
                    ]);
                    let (active_agent_id, active_sys_id, active_meta) = ctx.db.with_snapshot(|snap| {
                        let am = snap.active_missions.get(&(char_id, mission_id));
                        (
                            am.map_or(agent_id, |m| m.agent_id),
                            am.map(|m| m.solar_system_id)
                                .filter(|&s| s > 0)
                                .unwrap_or(mission_sys_id),
                            load_mission_meta(snap, char_id, mission_id),
                        )
                    });
                    let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
                    let mission_body = build_mission_briefing_text(
                        ctx,
                        active_agent_id,
                        mission_id,
                        &mission_title,
                        active_sys_id,
                        active_meta.as_ref(),
                    );
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnAgentMissionChange {
                            action: "offer_accepted".into(),
                            agent_id: active_agent_id,
                        },
                    );
                    emit_agent_mission_dialogue(
                        ctx,
                        char_id,
                        active_agent_id,
                        agent_type_id,
                        &mission_title,
                        &mission_body,
                        false,
                    );
                    let accept_msg = format!(
                        "Excellent. Your mission '{mission_title}' is now active in your journal. Return here once your objectives are complete."
                    );
                    Some(Ok(PyRep::keyval([
                        ("missionID", PyRep::Integer(mission_id as i64)),
                        ("contentID", PyRep::Integer(mission_id as i64)),
                        ("agentID", PyRep::Integer(active_agent_id)),
                        ("title", PyRep::String(mission_title)),
                        ("state", PyRep::String("Accepted".into())),
                        ("missionState", PyRep::Integer(2)),
                        ("agentSays", PyRep::String(accept_msg.clone())),
                        ("briefing", PyRep::String(accept_msg)),
                        ("dialogue", dialogue),
                        ("dungeonID", PyRep::None),
                        ("currentRoomID", PyRep::Integer(1)),
                        ("bookmarkID", PyRep::Integer(bookmark_id)),
                        ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                        ("bonusExpiryUnix", PyRep::Integer(bonus_expiry_unix as i64)),
                        ("bonusWindowSecs", PyRep::Integer(bonus_dur as i64)),
                        ("spawnedNpcBallID", first_npc_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ])))
                }

                // 6 = Complete, 7 = CompleteRemotely
                6 | 7 => {
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some("Accepted"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                        return Some(Err(BridgeRpcError { code: "UnknownMission".into(), message: format!("Mission {mission_id} does not exist in SDE"), client_error: None }));
                    };
                    let is_storyline = is_storyline_template(&tmpl);
                    let (active_agent_id, pre_dest_station, pre_is_dc, pre_is_trade, pre_is_courier) = ctx.db.with_snapshot(|snap| {
                        let m_agent = snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id);
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        let arch = classify_mission_archetype(
                            Some(&tmpl),
                            meta_opt.as_ref(),
                            ctx.sde.agents.get(&m_agent),
                        );
                        let dest_st = meta_opt.as_ref().and_then(|mt| mt.destination_station_id);
                        (m_agent, dest_st, arch.is_data_center, arch.is_trade, arch.is_courier)
                    });
                    if action_id == 6 {
                        if let Some(ref agent_info) = ctx.sde.agents.get(&active_agent_id).cloned() {
                            let at_courier_dest = pre_is_courier
                                && pre_dest_station.is_some()
                                && session.station_id == pre_dest_station;
                            if !at_courier_dest && !is_agent_colocated(ctx, session, sys_id, agent_info).await {
                                return Some(Err(BridgeRpcError {
                                    code: "AgentNotCoLocated".into(),
                                    message: format!(
                                        "Agent {active_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                                        agent_info.solar_system_id, agent_info.station_id, session.station_id
                                    ),
                                    client_error: None,
                                }));
                            }
                        } else if pre_is_dc || pre_is_trade {
                            return Some(Err(BridgeRpcError {
                                code: "UnknownAgent".into(),
                                message: format!("Agent {active_agent_id} does not exist in SDE"),
                                client_error: None,
                            }));
                        }
                    }
                    let agent_level = ctx.sde.agents.get(&active_agent_id).map_or(1.0, |a| a.level.max(1) as f64);
                    let storyline_mult = if is_storyline { 3.0 } else { 1.0 };
                    let standing_pct = if tmpl.has_standing_rewards { 1.5 * agent_level * storyline_mult } else { 0.0 };
                    let (storyline_agent_id, _) = resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, true);
                    let CharacterMissionRewards {
                        isk_reward,
                        lp_reward,
                        bonus_reward,
                    } = compute_character_mission_rewards(ctx, char_id, active_agent_id, mission_id, sys_id);
                    let corp_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.corporation_id)
                        .unwrap_or_else(|| {
                            resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, is_storyline).1
                        });
                    let faction_id = ctx.sde.npc_corporations.get(&corp_id).and_then(|c| c.faction_id).map(|fid| fid as i64);
                    let now = ctx.now_unix();

                    let is_expired = ctx.db.with_snapshot(|snap| {
                        load_mission_meta(snap, char_id, mission_id)
                            .and_then(|m| m.accept_expiry_unix)
                            .is_some_and(|exp| exp > 0 && now > exp)
                    });
                    if is_expired {
                        ctx.db.with_mut_snapshot(|snap| {
                            finalize_mission_state_cleanup(snap, char_id, mission_id, "Failed", true);
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                let cur_ag = ch.standings.get(&active_agent_id).copied().unwrap_or(0.0);
                                ch.standings.insert(active_agent_id, (cur_ag - 0.10).max(-10.0));
                                let cur_corp = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                                ch.standings.insert(corp_id, (cur_corp - 0.10).max(-10.0));
                                if let Some(fac_id) = faction_id {
                                    let cur_fac = ch.standings.get(&fac_id).copied().unwrap_or(0.0);
                                    ch.standings.insert(fac_id, (cur_fac - 0.05).max(-10.0));
                                }
                            }
                            snap.next_entity_id += 1;
                            let ev_id = snap.next_entity_id;
                            snap.standing_transactions.entry(char_id).or_default().push(
                                StandingTransactionRecord {
                                    event_id: ev_id,
                                    from_id: corp_id,
                                    to_id: char_id,
                                    modification: -0.10,
                                    event_type_id: 84,
                                },
                            );
                        });
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnAgentMissionObjectivesFailed {
                                agent_id: active_agent_id,
                            },
                        );
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnAgentMissionChange {
                                action: "failed".into(),
                                agent_id: active_agent_id,
                            },
                        );
                        return Some(Err(BridgeRpcError {
                            code: "MissionExpired".into(),
                            message: format!("Mission {mission_id} has expired and failed"),
                            client_error: None,
                        }));
                    }

                    let res = ctx.db.try_with_mut_snapshot(|snap| {
                        let meta_opt = load_mission_meta(snap, char_id, mission_id);
                        let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                            return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}"), client_error: None });
                        };
                        let agent_opt = ctx.sde.agents.get(&m.agent_id);
                        let arch = classify_mission_archetype(Some(&tmpl), meta_opt.as_ref(), agent_opt);
                        let is_rnd = arch.is_rnd_turnin;
                        let is_dc = arch.is_data_center;
                        let is_heraldry = arch.is_heraldry;
                        let is_trade = arch.is_trade;
                        let is_courier = arch.is_courier;
                        let is_kill = !is_dc && arch.is_kill;
                        let is_mining_mission = !is_rnd && !is_dc && !is_kill && !is_trade && !is_courier && !is_heraldry && {
                            let agent_div = ctx.sde.agents.get(&m.agent_id).map(|a| a.division_id).unwrap_or(0);
                            agent_div == 23 || {
                                let t = tmpl.title.to_lowercase();
                                let c = tmpl
                                    .content_template
                                    .as_deref().unwrap_or_default()
                                    .to_lowercase();
                                c.contains("mining")
                                    || t.split(|ch: char| !ch.is_alphanumeric()).any(|w| matches!(w, "mining" | "mineral" | "minerals" | "ore" | "ores"))
                            }
                        };

                        if m.state != "Accepted" || (m.npcs_remaining > 0 && !is_mining_mission && !is_rnd && !is_courier && !is_trade && !is_dc && !is_heraldry) {
                            return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Mission {mission_id} cannot be completed (state='{}', npcs_remaining={})", m.state, m.npcs_remaining), client_error: None });
                        }

                        let req_delivery = meta_opt
                            .as_ref()
                            .and_then(|mt| {
                                mt.required_delivery_type_id
                                    .map(|tid| (tid, mt.required_delivery_qty.max(1)))
                            })
                            .or_else(|| {
                                if is_mining_mission {
                                    let md = resolve_mining_mission_details(ctx, &tmpl);
                                    Some((md.ore_type_id, md.required_units))
                                } else if is_trade {
                                    let td = resolve_trade_mission_details(ctx, &tmpl);
                                    Some((td.trade_type_id, td.trade_quantity))
                                } else if is_rnd {
                                    Some((34, rnd_tritanium_required_qty(agent_opt.map_or(1, |a| a.level))))
                                } else if is_dc {
                                    Some((17202, 1))
                                } else {
                                    None
                                }
                            });
                        let m_agent_id = m.agent_id;
                        if action_id == 7 && req_delivery.is_some() {
                            return Err(BridgeRpcError {
                                code: "CannotCompleteRemotely".into(),
                                message: format!(
                                    "Mission {mission_id} requires physical item delivery and cannot be completed remotely"
                                ),
                                client_error: None,
                            });
                        }
                        if let Some((req_tid, req_units)) = req_delivery {
                            let current_station = snap.characters.get(&char_id).and_then(|c| c.station_id).or(session.station_id);
                            let agent_station_id = ctx
                                .sde
                                .agents
                                .get(&m_agent_id)
                                .and_then(|a| a.station_id)
                                .or_else(|| meta_opt.as_ref().and_then(|mt| mt.origin_station_id));
                            let active_ship_id = snap
                                .characters
                                .get(&char_id)
                                .map(|c| c.active_ship_id)
                                .filter(|&id| id > 0)
                                .or(session.ship_id);
                            let dest_station_id = meta_opt.as_ref().and_then(|mt| mt.destination_station_id);
                            let granted_cid = meta_opt.as_ref().and_then(|mt| mt.granted_courier_item_id);
                            super::agents_mission_delivery::verify_and_consume_mission_delivery_items(
                                snap,
                                char_id,
                                mission_id,
                                req_tid,
                                req_units,
                                is_courier,
                                is_mining_mission,
                                current_station,
                                agent_station_id,
                                active_ship_id,
                                dest_station_id,
                                granted_cid,
                            )?;
                        }

                        finalize_mission_state_cleanup(snap, char_id, mission_id, "Completed", false);

                        let bonus_granted = meta_opt
                            .as_ref().and_then(|mt| mt.bonus_expiry_unix)
                            .is_none_or(|bexp| now <= bexp);
                        let bonus_paid = if bonus_granted { bonus_reward } else { 0 };
                        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id).unwrap_or(0);
                        let lp_mult = crate::services::pve_special_sites::get_incursion_effects_for_system_in_snap(&ctx.sde, snap, char_sys)
                            .map(|e| (1.0 - e.lp_penalty).clamp(0.0, 1.0))
                            .unwrap_or(1.0);
                        // NPC corporations charge 0% LP tax; independent 11% NPC-corp (or player-corp) ISK
                        // taxation for base reward and time-bonus wallet journal entries (>= 100_000 ISK
                        // threshold for NPC corps) is applied right below by `lp_store::record_lp_reward_log`.
                        let credited_lp = ((lp_reward.max(0) as f64) * lp_mult).round() as i64;

                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            if isk_reward > 0 {
                                let reward_cents = IskCents::from_isk(isk_reward);
                                ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                                let bal_after_reward = ch.balance;

                                snap.next_entity_id += 1;
                                let jid1 = snap.next_entity_id;
                                snap.wallet_journal.push(WalletJournalEntry {
                                    entry_id: jid1,
                                    character_id: char_id,
                                    entry_type: "agent_mission_reward".to_string(),
                                    amount: reward_cents,
                                    balance_after: bal_after_reward,
                                    reason: format!("Mission {mission_id} reward"),
                                });
                                snap.wallet_journal_timestamps.insert(jid1, now);
                            }

                            if bonus_paid > 0 {
                                let bonus_cents = IskCents::from_isk(bonus_paid);
                                ch.balance = ch.balance.checked_add(bonus_cents).unwrap_or(ch.balance);
                                snap.next_entity_id += 1;
                                let jid2 = snap.next_entity_id;
                                snap.wallet_journal.push(WalletJournalEntry {
                                    entry_id: jid2,
                                    character_id: char_id,
                                    entry_type: "agent_mission_time_bonus_reward".to_string(),
                                    amount: bonus_cents,
                                    balance_after: ch.balance,
                                    reason: format!("Mission {mission_id} time bonus reward"),
                                });
                                snap.wallet_journal_timestamps.insert(jid2, now);
                            }

                            if credited_lp > 0 {
                                let lp_entry = ch.loyalty_points.entry(corp_id).or_insert(0);
                                *lp_entry = lp_entry.saturating_add(credited_lp);
                                if corp_id == crate::services::lp_store_catalog::PARAGON_CORP_ID && ch.corporation_id >= 98_000_000 {
                                    let c_lp = snap.corp_lp_balances.entry((ch.corporation_id, corp_id)).or_insert(0);
                                    *c_lp = c_lp.saturating_add(credited_lp);
                                }
                            }
                        }
                        crate::services::lp_store::record_lp_reward_log(
                            snap,
                            char_id,
                            corp_id,
                            credited_lp,
                            format!("Mission {mission_id} (agent {m_agent_id})"),
                            now,
                        );

                        let is_rnd = ctx.sde.agents.get(&m_agent_id).is_some_and(is_rnd_agent);
                        if is_rnd {
                            crate::services::pve_ess_reserve_banks::record_agent_mission_completed(snap, char_id, m_agent_id);
                            let ag_lvl = ctx.sde.agents.get(&m_agent_id).map_or(1, |a| a.level);
                            let mut research = load_rnd_research_entries(snap, char_id);
                            if let Some(re) = research.iter_mut().find(|r| r.agent_id == m_agent_id) {
                                let elapsed_days = (now.saturating_sub(re.started_unix) as f64) / 86_400.0;
                                re.initial_points += re.points_per_day * elapsed_days + re.points_per_day;
                                let sk_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&re.research_type_id)).map_or(1, |s| s.level);
                                re.points_per_day = re.points_per_day.max(crate::services::pve_ess_reserve_banks::compute_rnd_rp_per_day(ctx, snap, char_id, m_agent_id, ag_lvl, sk_lvl));
                                re.started_unix = now;
                            }
                            save_rnd_research_entries(snap, char_id, &research);
                        }

                        let is_special = is_storyline
                            || ctx.sde.agents.get(&m_agent_id)
                                .is_some_and(|a| matches!(a.agent_type_id, 5 | 8 | 10));
                        let (new_st, st_changes) = apply_mission_completion_standings(ctx, snap, char_id, m_agent_id, mission_id, corp_id, standing_pct, is_special);

                        let mut arc_completed_now = false;
                        if ctx.sde.agents.get(&m_agent_id)
                            .is_some_and(|a| a.agent_type_id == 10)
                        {
                            let (arc_id, fac_id) = resolve_epic_arc_id_for_agent(ctx, m_agent_id);
                            let required_missions = epic_arc_required_missions(ctx, fac_id);
                            let mut arcs = load_epic_arc_states(snap, char_id);
                            if let Some(arc) = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id) {
                                if arc.completed_at_unix.is_some() {
                                    arc.completed_missions.clear();
                                    arc.completed_at_unix = None;
                                    arc.cooldown_until_unix = None;
                                    arc.chapter = 1;
                                }
                                if !arc.completed_missions.contains(&mission_id) {
                                    arc.completed_missions.push(mission_id);
                                }
                                let done = arc.completed_missions.len();
                                let computed_chapter = ((done * 3).saturating_add(required_missions - 1) / required_missions).clamp(1, 3) as u32;
                                arc.chapter = arc.chapter.max(computed_chapter);
                                if done >= required_missions {
                                    arc.completed_at_unix = Some(now);
                                    arc.cooldown_until_unix = Some(now + 7_776_000);
                                    arc_completed_now = true;
                                }
                            } else {
                                arcs.push(EpicArcStateEntry {
                                    epic_arc_id: arc_id,
                                    agent_id: m_agent_id,
                                    faction_id: fac_id,
                                    chapter: 1,
                                    completed_missions: vec![mission_id],
                                    chosen_branch: None,
                                    completed_at_unix: None,
                                    cooldown_until_unix: None,
                                });
                            }
                            save_epic_arc_states(snap, char_id, &arcs);
                        }

                        let is_epic_arc = ctx.sde.agents.get(&m_agent_id).is_some_and(|a| matches!(a.agent_type_id, 10 | 11));
                        let mut resolved_storyline_agent_id = storyline_agent_id;
                        let mut resolved_storyline_level = ctx.sde.agents.get(&m_agent_id).map_or(1, |a| a.level.clamp(1, 5));
                        let (t, storyline) = if (!is_storyline && tmpl.has_standing_rewards) || is_rnd || is_epic_arc {
                            let sl_opt = ctx.sde.agents.get(&m_agent_id).and_then(|ca| crate::services::pve_ess_reserve_banks::record_mission_completion_and_resolve_storyline(ctx, snap, char_id, ca));
                            let qualifies = ctx.sde.agents.get(&m_agent_id).is_some_and(|ca| crate::services::pve_ess_reserve_banks::is_storyline_qualifying_agent_type(ca.agent_type_id)) && !crate::services::pve_ess_reserve_banks::is_interbus_or_militia_corp(&ctx.sde, corp_id);
                            let t = if !qualifies {
                                snap.mission_tally.get(&char_id).copied().unwrap_or(0)
                            } else {
                                let count = snap.mission_tally.entry(char_id).or_insert(0); *count += 1; *count
                            };
                            if let Some((sl_id, sl_lvl)) = sl_opt { resolved_storyline_agent_id = sl_id; resolved_storyline_level = sl_lvl; }
                            (t, sl_opt.is_some())
                        } else {
                            (snap.mission_tally.get(&char_id).copied().unwrap_or(0), false)
                        };
                        if storyline {
                            let storyline_agent_id = resolved_storyline_agent_id;
                            if let Some(storyline_tmpl) = crate::services::pve_ess_reserve_banks::select_storyline_mission_for_level(
                                &ctx.sde,
                                &snap.active_missions,
                                char_id,
                                resolved_storyline_level,
                            )
                            {
                                let storyline_mission_id = storyline_tmpl.mission_id;
                                let sl_agent = ctx.sde.agents.get(&storyline_agent_id);
                                let sl_sys_id = sl_agent.and_then(|a| a.solar_system_id).unwrap_or(sys_id);
                                let ct = storyline_tmpl.content_template.as_deref().unwrap_or_default();
                                let arch = classify_mission_archetype(Some(storyline_tmpl), None, sl_agent);
                                let is_trade = arch.is_trade;
                                let is_courier = arch.is_courier;
                                let is_kill = arch.is_kill;
                                let is_mining = !is_kill && !is_trade && !is_courier && !arch.is_heraldry && (ct.contains("BasicMining")
                                    || storyline_tmpl.title.to_lowercase().contains("mining")
                                    || sl_agent.is_some_and(|a| a.division_id == 23));
                                let (req_tid, req_qty, origin_sid, dest_sid, sl_npcs) = if is_courier {
                                    if let Some(sa) = sl_agent.and_then(|a| resolve_courier_mission_details(ctx, storyline_tmpl, a).ok()) {
                                        (Some(sa.cargo_type_id), sa.cargo_quantity, Some(sa.origin_station_id), Some(sa.destination_station_id), 0)
                                    } else {
                                        (None, 0, sl_agent.and_then(|a| a.station_id), None, 0)
                                    }
                                } else if is_trade {
                                    let td = resolve_trade_mission_details(ctx, storyline_tmpl);
                                    (Some(td.trade_type_id), td.trade_quantity, sl_agent.and_then(|a| a.station_id), None, 0)
                                } else if is_mining {
                                    let md = resolve_mining_mission_details(ctx, storyline_tmpl);
                                    (Some(md.ore_type_id), md.required_units, sl_agent.and_then(|a| a.station_id), None, 0)
                                } else if let Some(sa) = sl_agent {
                                    let mut scaled_sa = sa.clone();
                                    scaled_sa.level = resolved_storyline_level;
                                    let enc = resolve_security_mission_encounter(ctx, storyline_tmpl, &scaled_sa, sl_sys_id);
                                    (None, 0, sa.station_id, None, enc.wave_npc_count)
                                } else {
                                    (None, 0, None, None, 1)
                                };
                                save_mission_meta(
                                    snap,
                                    char_id,
                                    &MissionTimingAndCargoMeta {
                                        mission_id: storyline_mission_id,
                                        agent_id: storyline_agent_id,
                                        offered_at_unix: now,
                                        offer_expiry_unix: now + 604_800,
                                        accepted_at_unix: None,
                                        accept_expiry_unix: None,
                                        bonus_expiry_unix: None,
                                        completed_at_unix: None,
                                        required_delivery_type_id: req_tid,
                                        required_delivery_qty: req_qty,
                                        granted_courier_item_id: None,
                                        granted_courier_qty: 0,
                                        is_anomic_burner: false,
                                        chosen_branch: None,
                                        destination_station_id: dest_sid,
                                        origin_station_id: origin_sid,
                                        offered_level: Some(resolved_storyline_level),
                                    },
                                );
                                snap.active_missions
                                    .entry((char_id, storyline_mission_id))
                                    .or_insert(ActiveMissionRecord { mission_id: storyline_mission_id, character_id: char_id, agent_id: storyline_agent_id, solar_system_id: sl_sys_id, state: "Offered".into(), current_room_id: 1, npcs_remaining: sl_npcs, spawned_npc_ball_ids: Vec::new(), bookmark_id: None });
                            }
                        }
                        let delivery_station = ctx.sde.agents.get(&m_agent_id).and_then(|a| a.station_id).or_else(|| snap.characters.get(&char_id).and_then(|c| c.station_id)).unwrap_or(0);
                        let item_reward = crate::services::agents_mission_rewards::mint_mission_item_reward_in_snap(ctx, snap, char_id, delivery_station, &tmpl, ctx.sde.agents.get(&m_agent_id));
                        let isk_paid = isk_reward > 0 || bonus_paid > 0;
                        let new_bal_opt = isk_paid.then(|| snap.characters.get(&char_id).map(|c| c.balance)).flatten();
                        Ok((new_st, t, storyline, bonus_paid, st_changes, item_reward, new_bal_opt, credited_lp, m_agent_id, is_courier, is_mining_mission, is_kill, arc_completed_now))
                    });

                    match res {
                        Ok((new_standing, tally, storyline_offered, bonus_paid, st_changes, item_reward, new_bal_opt, credited_lp, completed_agent_id, is_courier, is_mining_mission, is_kill, arc_completed_now)) => {
                            if let Some(bal) = new_bal_opt {
                                let _ = ctx.notify_wallet_balance(false, char_id, 1, bal);
                            }
                            if let Some(irw) = item_reward {
                                crate::services::agents_mission_rewards::emit_mission_item_reward_notification(ctx, char_id, irw);
                            }
                            notify_standing_changes(ctx, char_id, &st_changes);
                            let mission_title_career = ctx.sde.missions.get(&mission_id).map(|m| m.title.as_str()).unwrap_or("");
                            let is_fw_mission_agent = ctx.sde.agents.get(&completed_agent_id).is_some_and(is_fw_agent);
                            crate::quasar_gateway::isolate_side_effect("agents_missions.career_mission_complete", || {
                                crate::services::quasar_career_goals::record_mission_completion_career_goals(
                                    ctx,
                                    char_id,
                                    completed_agent_id,
                                    mission_title_career,
                                    is_courier,
                                    is_mining_mission,
                                    is_kill,
                                    is_fw_mission_agent,
                                    arc_completed_now,
                                    credited_lp,
                                );
                            });
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnAgentMissionObjectivesCompleted {
                                    agent_id: active_agent_id,
                                },
                            );
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnAgentMissionChange {
                                    action: "completed".into(),
                                    agent_id: active_agent_id,
                                },
                            );
                            let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
                            let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone()).unwrap_or_else(|| format!("Mission {mission_id}"));
                            let complete_msg = format!(
                                "Well done! Your mission '{mission_title}' is complete and your rewards have been credited."
                            );
                            emit_agent_mission_dialogue(
                                ctx,
                                char_id,
                                active_agent_id,
                                agent_type_id,
                                &mission_title,
                                &complete_msg,
                                true,
                            );
                            Some(Ok(PyRep::keyval([
                                ("missionID", PyRep::Integer(mission_id as i64)),
                                ("contentID", PyRep::Integer(mission_id as i64)),
                                ("agentID", PyRep::Integer(active_agent_id)),
                                ("title", PyRep::String(mission_title)),
                                ("state", PyRep::String("Completed".into())),
                                ("missionState", PyRep::Integer(4)),
                                ("agentSays", PyRep::String(complete_msg.clone())),
                                ("briefing", PyRep::String(complete_msg)),
                                (
                                    "dialogue",
                                    PyRep::List(vec![PyRep::Tuple(vec![
                                        PyRep::Integer(2),
                                        PyRep::String("Request Mission".into()),
                                    ])]),
                                ),
                                ("iskReward", PyRep::Integer(isk_reward)),
                                ("bonusReward", PyRep::Integer(bonus_paid)),
                                ("lpReward", PyRep::Integer(lp_reward)),
                                ("newStanding", PyRep::Real(new_standing)),
                                ("missionTally", PyRep::Integer(tally as i64)),
                                ("storylineTriggered", PyRep::Bool(storyline_offered)),
                                ("rewardItemID", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.item_id))),
                                ("rewardItemTypeID", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.type_id as i64))),
                                ("rewardItemQuantity", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.quantity as i64))),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    }
                }

                // 9 = Decline, 10 = Defer, 11 = Quit
                9..=11 => {
                    let expected_state = if action_id == 11 { Some("Accepted") } else { Some("Offered") };
                    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
                    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, expected_state, args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
                    let active_agent_id = ctx.db.with_snapshot(|snap| {
                        snap.active_missions.get(&(char_id, mission_id)).map_or(agent_id, |m| m.agent_id)
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

#[cfg(test)] #[path = "agents_missions_unit_tests.rs"] mod agents_missions_unit_tests;
#[cfg(test)] #[path = "agents_missions_edges2_tests.rs"] mod agents_missions_edges2_tests;
#[cfg(test)] #[path = "agents_missions_tests.rs"] mod agents_missions_tests;

