//! Typed warp target resolution and mission bookmark lookup for `agents_missions` (`PVE-003`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{BookmarkRecord, MissionInstance, MissionState, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

fn splitmix64_hash(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Computes a deterministic, per-instance mission site origin in `mission_sys_id` (`PVE-001`).
///
/// Anchors off the system's first station (or `DVec3::ZERO`) and offsets by a SplitMix64-derived
/// vector quantized onto 500 km cells spanning `+/- 50,000 km` outside station undock grids
/// (`>= 5,000,000 m` per axis), so distinct `(mission_sys_id, instance_id)` sites hash
/// uniformly across `182^3` cells with negligible collision probability.
pub(crate) fn mission_site_origin_for_instance(
    ctx: &ServerContext,
    mission_sys_id: u32,
    instance_id: i64,
) -> DVec3 {
    let base = ctx
        .sde
        .solar_systems
        .get(&mission_sys_id)
        .and_then(|s| s.stations.first().map(|st| st.position))
        .unwrap_or(DVec3::ZERO);
    let seed = splitmix64_hash(
        (mission_sys_id as u64).rotate_left(32)
            ^ (instance_id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15),
    );
    let hx = splitmix64_hash(seed);
    let hy = splitmix64_hash(hx);
    let hz = splitmix64_hash(hy);
    let axis_offset = |h: u64| -> f64 {
        let sign = if (h & 1) == 0 { 1.0 } else { -1.0 };
        let cell = ((h >> 1) % 91) as f64;
        sign * (5_000_000.0 + cell * 500_000.0)
    };
    let pos = base + DVec3::new(axis_offset(hx), axis_offset(hy), axis_offset(hz));
    DVec3::new(pos.x.round(), pos.y.round(), pos.z.round())
}

fn validate_mission_bookmark_origin_in_snap(
    snap: &WorldStateSnapshot,
    session: Option<&SessionState>,
    char_id: i64,
    bid: i64,
    sys_id: u32,
    now_unix: u64,
) -> Result<Option<DVec3>, BridgeRpcError> {
    if crate::services::fleet_social::is_bookmark_expired_in_snap(snap, bid, now_unix) {
        return Ok(None);
    }
    let Some(bm) = snap.bookmarks.get(&bid) else {
        return Ok(None);
    };
    if bm.solar_system_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH {
        return Err(BridgeRpcError::from_user_key(
            eve_proto::UserErrorKey::DunBlacklistCannotWarp,
        ));
    }
    let owner_session_storage;
    let owner_sess = match session {
        Some(s) => s,
        None => {
            owner_session_storage = {
                let mut s = SessionState::new(0);
                s.character_id = Some(char_id);
                if let Some(ch) = snap.characters.get(&char_id) {
                    s.corporation_id = Some(ch.corporation_id);
                    s.alliance_id = ch.alliance_id;
                }
                s
            };
            &owner_session_storage
        }
    };
    Ok((can_use_bookmark_in_snap(snap, owner_sess, char_id, bm)
        && bm.solar_system_id == sys_id
        && bm.item_id.is_none()
        && bm.x.is_finite()
        && bm.y.is_finite()
        && bm.z.is_finite())
    .then(|| DVec3::new(bm.x, bm.y, bm.z)))
}

/// Canonical mission pocket-origin resolver shared across warp origin lookup, warp-entry
/// materialization, and downtime wave entry (`site_origin_f64` -> validated non-expired ACL
/// `bookmark_id` -> optional `mission_site_origin_for_instance` fallback).
pub(crate) fn resolve_mission_pocket_origin_in_snap(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: Option<&SessionState>,
    mission: &MissionInstance,
    sys_id: u32,
    now_unix: u64,
    allow_site_fallback: bool,
) -> Result<Option<DVec3>, BridgeRpcError> {
    if sys_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH {
        return Err(BridgeRpcError::from_user_key(
            eve_proto::UserErrorKey::DunBlacklistCannotWarp,
        ));
    }
    let site_origin = mission
        .site_origin_f64()
        .map(DVec3::from)
        .filter(|pos| pos.is_finite());
    let bm_origin = match mission.bookmark_id {
        Some(bid) => validate_mission_bookmark_origin_in_snap(
            snap,
            session,
            mission.character_id,
            bid,
            sys_id,
            now_unix,
        )?,
        None => None,
    };
    let mut origin = site_origin.or(bm_origin);
    if origin.is_none() && allow_site_fallback {
        let fallback = mission_site_origin_for_instance(ctx, sys_id, mission.instance_id);
        if fallback.is_finite() {
            origin = Some(fallback);
        }
    }
    Ok(origin)
}

/// Resolves an accepted mission target (`template_id` or `agent_id`) in `sys_id` to its canonical
/// pocket origin (`site_origin_f64` -> valid non-expired ACL `bookmark_id` -> `mission_site_origin_for_instance`),
/// enforcing the Zarzakh dungeon warp ban matching `mission_dungeon_warp_target`.
pub(crate) fn resolve_accepted_mission_warp_origin(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    target_id: i64,
) -> Result<Option<DVec3>, BridgeRpcError> {
    let now_unix = ctx.now_unix();
    ctx.db
        .with_snapshot(|snap| -> Result<Option<DVec3>, BridgeRpcError> {
            let mut missions: Vec<&MissionInstance> = snap
                .missions
                .iter_by_character(char_id)
                .filter(|m| {
                    m.solar_system_id == sys_id
                        && m.state == MissionState::Accepted
                        && (target_id == m.template_id as i64 || target_id == m.agent_id as i64)
                })
                .collect();
            missions.sort_by_key(|m| m.template_id);
            let Some(m) = missions.into_iter().next() else {
                return Ok(None);
            };
            resolve_mission_pocket_origin_in_snap(ctx, snap, Some(session), m, sys_id, now_unix, true)
        })
}

fn resolve_mission_pocket_match_in_snap(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    char_id: i64,
    sys_id: u32,
    stuff_type: &str,
    target_id: i64,
    dest_pos: DVec3,
    now_unix: u64,
) -> Option<(i64, u32, DVec3)> {
    if !dest_pos.is_finite()
        || sys_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH
    {
        return None;
    }
    let is_mission_stuff = matches!(stuff_type, "mission" | "dungeon" | "location");
    let target_bm = (stuff_type == "bookmark")
        .then(|| {
            (!crate::services::fleet_social::is_bookmark_expired_in_snap(snap, target_id, now_unix))
                .then(|| snap.bookmarks.get(&target_id))
                .flatten()
                .filter(|bm| {
                    bm.solar_system_id == sys_id
                        && bm.item_id.is_none()
                        && bm.x.is_finite()
                        && bm.y.is_finite()
                        && bm.z.is_finite()
                })
        })
        .flatten();

    let mut best: Option<((u8, i64), (i64, u32, DVec3))> = None;
    let mut consider = |m: &MissionInstance, allow_site_fallback: bool, tier: u8| {
        if m.state != MissionState::Accepted || m.solar_system_id != sys_id {
            return;
        }
        let Ok(Some(pocket_pos)) = resolve_mission_pocket_origin_in_snap(
            ctx,
            snap,
            None,
            m,
            sys_id,
            now_unix,
            allow_site_fallback,
        ) else {
            return;
        };
        if !pocket_pos.is_finite() || dest_pos.distance(pocket_pos) > 200_000.0 {
            return;
        }
        let key = (tier, m.instance_id);
        if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
            best = Some((key, (m.character_id, m.template_id, pocket_pos)));
        }
    };

    if let Some(bm) = target_bm {
        for m in snap.missions.iter() {
            let tier = if m.bookmark_id == Some(target_id) {
                0
            } else if m.character_id == char_id {
                1
            } else if m.character_id == bm.owner_id {
                2
            } else {
                3
            };
            consider(m, false, tier);
        }
    } else {
        for m in snap.missions.iter_by_character(char_id) {
            let explicit_mission_match = is_mission_stuff
                && (target_id == i64::from(m.template_id) || target_id == m.agent_id);
            let tier = if explicit_mission_match
                || (stuff_type == "bookmark" && m.bookmark_id == Some(target_id))
            {
                0
            } else {
                1
            };
            consider(m, explicit_mission_match, tier);
        }
    }

    best.map(|(_, matched)| matched)
}

async fn materialize_matched_mission_pocket(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    mission_owner_id: i64,
    sys_id: u32,
    mid: u32,
    pocket_pos: DVec3,
) {
    super::reconcile_stale_mission_npcs(ctx, handle, mission_owner_id, mid);
    let _ = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
        ctx,
        mission_owner_id,
        sys_id,
        pocket_pos,
        Some(mid),
    );
    super::agents_missions_helpers::ensure_mission_asteroid_spawned_with_handle(
        ctx,
        handle,
        mission_owner_id,
        sys_id,
        Some(mid),
    )
    .await;
}

/// Matches the targeted mission on a `beyonce.CmdWarpToStuff` / solo warp and materializes both
/// its encounter NPCs and persisted mining asteroid under `mission_owner_id` (`PVE-020`).
pub(crate) async fn materialize_mission_pocket_on_cmd_warp(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    sys_id: u32,
    stuff_type: &str,
    target_id: i64,
    dest_pos: DVec3,
) {
    if !dest_pos.is_finite() {
        return;
    }
    let now_unix = ctx.now_unix();
    let matched_mission = ctx.db.with_snapshot(|snap| {
        resolve_mission_pocket_match_in_snap(
            ctx,
            snap,
            char_id,
            sys_id,
            stuff_type,
            target_id,
            dest_pos,
            now_unix,
        )
    });
    if let Some((mission_owner_id, mid, pocket_pos)) = matched_mission {
        materialize_matched_mission_pocket(ctx, handle, mission_owner_id, sys_id, mid, pocket_pos)
            .await;
    }
}

/// Materializes mission pockets for warped fleet members whose actual landing points reached within
/// `200 km` of the pocket (`CmdWarpToStuff(..., fleet=true)` / direct `handle_fleet_warp`),
/// deduplicating snapshot matches into one snapshot scan (`P2-2`).
pub(crate) async fn materialize_mission_pocket_on_fleet_warp(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    caller_char_id: i64,
    sys_id: u32,
    stuff_type: &str,
    target_id: i64,
    warped_landings: &[(i64, DVec3)],
) {
    if warped_landings.is_empty()
        || sys_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH
    {
        return;
    }
    let now_unix = ctx.now_unix();
    let mut targets: BTreeMap<(i64, u32), DVec3> = BTreeMap::new();
    ctx.db.with_snapshot(|snap| {
        for &(member_char_id, landing_pos) in warped_landings {
            if let Some((owner_id, mid, pocket_pos)) = resolve_mission_pocket_match_in_snap(
                ctx,
                snap,
                caller_char_id,
                sys_id,
                stuff_type,
                target_id,
                landing_pos,
                now_unix,
            ) {
                targets.entry((owner_id, mid)).or_insert(pocket_pos);
            }
            if member_char_id != caller_char_id && stuff_type == "bookmark" {
                if let Some((owner_id, mid, pocket_pos)) = resolve_mission_pocket_match_in_snap(
                    ctx,
                    snap,
                    member_char_id,
                    sys_id,
                    stuff_type,
                    target_id,
                    landing_pos,
                    now_unix,
                ) {
                    targets.entry((owner_id, mid)).or_insert(pocket_pos);
                }
            }
        }
    });
    for ((mission_owner_id, mid), pocket_pos) in targets {
        materialize_matched_mission_pocket(ctx, handle, mission_owner_id, sys_id, mid, pocket_pos)
            .await;
    }
}

pub(crate) const MIN_AGENT_WARP_DISTANCE_M: f64 = 150_000.0;
pub(crate) const MAX_AGENT_WARP_RANGE_M: f64 = 100_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentWarpError {
    InvalidArgument,
    InvalidMissionLocation,
    ShipDocked,
    NoActiveShip,
    NoSolarSystem,
    DestinyQueryFailed,
    ShipNotInSpace,
    CrossSystemWarp,
    BookmarkExpired,
    BookmarkNotFound,
    ShipAlreadyInWarp,
    CantMoveWhileModuleActive,
    CloakedCannotWarp,
}

impl AgentWarpError {
    pub(crate) fn into_rpc(self, message: impl Into<String>) -> BridgeRpcError {
        let msg = message.into();
        let client_error = eve_proto::ClientError::CustomInfo(msg.clone());
        match self {
            Self::InvalidArgument => BridgeRpcError::with_client_error("InvalidArgument", msg, client_error),
            Self::InvalidMissionLocation => BridgeRpcError::with_client_error("InvalidMissionLocation", msg, client_error),
            Self::ShipDocked => BridgeRpcError::with_client_error("ShipDocked", msg, client_error),
            Self::NoActiveShip => BridgeRpcError::with_client_error("NoActiveShip", msg, client_error),
            Self::NoSolarSystem => BridgeRpcError::with_client_error("NoSolarSystem", msg, client_error),
            Self::DestinyQueryFailed => BridgeRpcError::with_client_error("DestinyQueryFailed", msg, client_error),
            Self::ShipNotInSpace => BridgeRpcError::ship_not_in_space(0, None, None, "AgentMissionWarp", msg),
            Self::CrossSystemWarp => BridgeRpcError::with_client_error("CrossSystemWarp", msg, client_error),
            Self::BookmarkExpired => BridgeRpcError::with_client_error("BookmarkExpired", msg, client_error),
            Self::BookmarkNotFound => BridgeRpcError::with_client_error("BookmarkNotFound", msg, client_error),
            Self::ShipAlreadyInWarp => BridgeRpcError::ship_already_in_warp(None, "AgentMissionWarp", None, msg),
            Self::CantMoveWhileModuleActive => BridgeRpcError::with_client_error("CantMoveWhileModuleActive", msg, client_error),
            Self::CloakedCannotWarp => BridgeRpcError::with_client_error("CloakedCannotWarp", msg, client_error),
        }
    }
}

pub(crate) fn can_use_bookmark_in_snap(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    bm: &BookmarkRecord,
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

pub(crate) fn mission_journal_bookmarks<'a>(
    snap: &'a WorldStateSnapshot,
    char_id: i64,
    mission: &MissionInstance,
) -> Vec<&'a BookmarkRecord> {
    let active_mission_bms: Vec<(u32, i64)> = snap
        .missions
        .iter_by_character(char_id)
        .filter_map(|om| om.bookmark_id.map(|bid| (om.template_id, bid)))
        .collect();
    let mut matching_bms: Vec<&BookmarkRecord> = snap
        .bookmarks
        .values()
        .filter(|b| {
            let owned_by_other_mission = active_mission_bms
                .iter()
                .any(|&(mid, bid)| mid != mission.template_id && bid == b.bookmark_id);
            b.owner_id == char_id
                && (mission.bookmark_id == Some(b.bookmark_id)
                    || (!owned_by_other_mission
                        && b.solar_system_id == mission.solar_system_id
                        && b.memo.contains("Mission Pocket")))
        })
        .collect();
    matching_bms.sort_by_key(|b| b.bookmark_id);
    matching_bms
}

/// Typed warp destination resolved by `agentMgr` warp / approach calls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WarpTarget {
    Dungeon {
        mission_id: u32,
        bookmark_id: Option<i64>,
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
    StationOrCelestial {
        location_id: i64,
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
    Bookmark {
        bookmark_id: i64,
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
    AgentInSpace {
        agent_id: i64,
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
    SpacePoint {
        solar_system_id: u32,
        anchor_pos: DVec3,
    },
}

impl WarpTarget {
    pub(crate) fn solar_system_id(&self) -> u32 {
        match *self {
            Self::Dungeon {
                solar_system_id, ..
            }
            | Self::StationOrCelestial {
                solar_system_id, ..
            }
            | Self::Bookmark {
                solar_system_id, ..
            }
            | Self::AgentInSpace {
                solar_system_id, ..
            }
            | Self::SpacePoint {
                solar_system_id, ..
            } => solar_system_id,
        }
    }

    pub(crate) fn landing_position(&self, ctx: &ServerContext, current_pos: DVec3) -> DVec3 {
        match *self {
            Self::StationOrCelestial {
                location_id,
                solar_system_id,
                anchor_pos,
            } => crate::services::nav_warp_interdiction::resolve_warp_landing_point(
                ctx,
                solar_system_id,
                location_id,
                current_pos,
                anchor_pos,
            ),
            Self::Dungeon { anchor_pos, .. }
            | Self::Bookmark { anchor_pos, .. }
            | Self::AgentInSpace { anchor_pos, .. }
            | Self::SpacePoint { anchor_pos, .. } => anchor_pos,
        }
    }
}

/// How a client's numeric location selector matched an accepted mission entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MissionLocationRef {
    DefaultMissionLocation,
    JournalOrdinal { index: usize },
    ExplicitBookmark { bookmark_id: i64 },
    MissionOrAgentId { id: i64 },
}

pub(crate) struct ParsedAgentWarpArgs<'a> {
    pub(crate) loc_type: Option<&'a str>,
    pub(crate) first_id: Option<i64>,
    pub(crate) loc_num: Option<i64>,
    pub(crate) raw_loc_num: Option<i64>,
    pub(crate) referring_agent_id: Option<i64>,
    pub(crate) warp_range: f64,
    pub(crate) fleet: bool,
    pub(crate) is_goto: bool,
}

pub(crate) fn parse_agent_warp_args<'a>(
    method: &str,
    args: &'a [PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<ParsedAgentWarpArgs<'a>, BridgeRpcError> {
    let (arg0, arg1, arg2, arg3, arg4) = match args {
        [a0, a1, a2, a3, a4, ..] => (Some(a0), Some(a1), Some(a2), Some(a3), Some(a4)),
        [a0, a1, a2, a3] => (Some(a0), Some(a1), Some(a2), Some(a3), None),
        [a0, a1, a2] => (Some(a0), Some(a1), Some(a2), None, None),
        [a0, a1] => (Some(a0), Some(a1), None, None, None),
        [a0] => (Some(a0), None, None, None, None),
        [] => (None, None, None, None, None),
    };
    let is_goto = method == "GotoLocation";
    // V24 `ApproachLocation` sends `agentMgr.GotoLocation(locType, locNumber, referringAgentID)`:
    // `args[2]` is an integer agent ID, never a warp standoff range.
    // V24 `WarpToBookmark` / `WarpFleetToBookmark` send
    // `agentMgr.WarpToLocation(locType, locNumber, warpRange, fleet, referringAgentID, formationSettings)`.
    let (referring_agent_id, warp_range, fleet) = if is_goto {
        let ref_agent = arg2
            .and_then(|v| v.as_i64())
            .filter(|&id| id > 0)
            .or_else(|| {
                kwargs
                    .get("referringAgentID")
                    .and_then(|v| v.as_i64())
                    .filter(|&id| id > 0)
            });
        (ref_agent, 0.0, false)
    } else {
        let raw_range = arg2
            .and_then(|v| v.as_f64())
            .or_else(|| kwargs.get("warpRange").and_then(|v| v.as_f64()))
            .or_else(|| kwargs.get("minRange").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        if !raw_range.is_finite() {
            return Err(AgentWarpError::InvalidArgument.into_rpc("warpRange must be a finite number",
            ));
        }
        let fleet_flag = arg3
            .or_else(|| kwargs.get("fleet"))
            .is_some_and(|v| match v {
                PyRep::Bool(b) => *b,
                PyRep::Integer(n) => *n != 0,
                _ => false,
            });
        let ref_agent = arg4
            .and_then(|v| v.as_i64())
            .filter(|&id| id > 0)
            .or_else(|| {
                kwargs
                    .get("referringAgentID")
                    .and_then(|v| v.as_i64())
                    .filter(|&id| id > 0)
            });
        (
            ref_agent,
            raw_range.clamp(0.0, MAX_AGENT_WARP_RANGE_M),
            fleet_flag,
        )
    };
    let raw_loc_num = arg1.and_then(|v| v.as_i64()).filter(|&n| n >= 0);
    Ok(ParsedAgentWarpArgs {
        loc_type: arg0.and_then(|v| v.as_str()),
        first_id: arg0.and_then(|v| v.as_i64()).filter(|&id| id > 0),
        loc_num: raw_loc_num.filter(|&n| n > 0),
        raw_loc_num,
        referring_agent_id,
        warp_range,
        fleet,
        is_goto,
    })
}

pub(crate) struct SelectedMissionWarp {
    pub(crate) mission: MissionInstance,
    pub(crate) bookmark_id: Option<i64>,
    pub(crate) explicit_bookmark_selected: bool,
    pub(crate) matched_by: MissionLocationRef,
}

fn mission_bookmark_id_match(
    bms: &[&BookmarkRecord],
    mission: &MissionInstance,
    loc: i64,
) -> Option<i64> {
    if mission.bookmark_id == Some(loc) || bms.iter().any(|b| b.bookmark_id == loc) {
        Some(loc)
    } else {
        None
    }
}

fn select_mission_with_journal_ordinal(
    bms: &[&BookmarkRecord],
    mission: &MissionInstance,
    idx: usize,
) -> Option<SelectedMissionWarp> {
    let bm_id = bms.get(idx)?.bookmark_id;
    let explicit_bookmark_selected = idx > 0 || mission.bookmark_id != Some(bm_id);
    Some(SelectedMissionWarp {
        mission: mission.clone(),
        bookmark_id: Some(bm_id),
        explicit_bookmark_selected,
        matched_by: MissionLocationRef::JournalOrdinal { index: idx },
    })
}

fn select_default_zero_location(
    bms: &[&BookmarkRecord],
    mission: &MissionInstance,
) -> SelectedMissionWarp {
    let (bookmark_id, explicit_bookmark_selected, matched_by) = match bms.first() {
        Some(bm) => (
            Some(bm.bookmark_id),
            mission.bookmark_id != Some(bm.bookmark_id),
            MissionLocationRef::JournalOrdinal { index: 0 },
        ),
        None => (
            mission.bookmark_id,
            false,
            MissionLocationRef::DefaultMissionLocation,
        ),
    };
    SelectedMissionWarp {
        mission: mission.clone(),
        bookmark_id,
        explicit_bookmark_selected,
        matched_by,
    }
}

fn select_scoped_mission_location(
    snap: &WorldStateSnapshot,
    char_id: i64,
    mission: &MissionInstance,
    raw_loc: Option<i64>,
) -> Option<SelectedMissionWarp> {
    let bms = mission_journal_bookmarks(snap, char_id, mission);
    let Some(loc) = raw_loc else {
        return Some(SelectedMissionWarp {
            mission: mission.clone(),
            bookmark_id: mission.bookmark_id,
            explicit_bookmark_selected: false,
            matched_by: MissionLocationRef::DefaultMissionLocation,
        });
    };
    if loc == 0 {
        return Some(select_default_zero_location(&bms, mission));
    }
    if let Some(sel) = usize::try_from(loc)
        .ok()
        .and_then(|idx| select_mission_with_journal_ordinal(&bms, mission, idx))
    {
        return Some(sel);
    }
    if let Some(bm_id) = mission_bookmark_id_match(&bms, mission, loc) {
        return Some(SelectedMissionWarp {
            mission: mission.clone(),
            bookmark_id: Some(bm_id),
            explicit_bookmark_selected: true,
            matched_by: MissionLocationRef::ExplicitBookmark { bookmark_id: bm_id },
        });
    }
    if i64::from(mission.template_id) == loc || mission.agent_id == loc {
        return Some(SelectedMissionWarp {
            mission: mission.clone(),
            bookmark_id: mission.bookmark_id,
            explicit_bookmark_selected: false,
            matched_by: MissionLocationRef::MissionOrAgentId { id: loc },
        });
    }
    None
}

pub(crate) fn find_accepted_mission_for_warp(
    ctx: &ServerContext,
    char_id: i64,
    bound_agent_id: Option<i64>,
    raw_location_number: Option<i64>,
    referring_agent_id: Option<i64>,
    kwargs: &HashMap<String, PyRep>,
) -> Option<SelectedMissionWarp> {
    let kw_mission_id = kwargs
        .get("missionID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0)
        .map(|id| id as u32);
    let kw_agent_id = kwargs
        .get("agentID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0);
    let agent_selector = bound_agent_id.or(kw_agent_id).or(referring_agent_id);
    ctx.db.with_snapshot(|snap| {
        let mut missions: Vec<&MissionInstance> = snap
            .missions
            .iter_by_character(char_id)
            .filter(|m| m.state == MissionState::Accepted)
            .collect();
        missions.sort_by_key(|m| m.template_id);
        if missions.is_empty() {
            return None;
        }
        if let Some(mid) = kw_mission_id {
            let m = missions.iter().copied().find(|m| m.template_id == mid)?;
            return select_scoped_mission_location(snap, char_id, m, raw_location_number);
        }
        if let Some(ag_id) = agent_selector {
            let m = missions.iter().copied().find(|m| m.agent_id == ag_id)?;
            return select_scoped_mission_location(snap, char_id, m, raw_location_number);
        }
        let Some(loc) = raw_location_number else {
            let m = *missions.first()?;
            return Some(SelectedMissionWarp {
                mission: m.clone(),
                bookmark_id: m.bookmark_id,
                explicit_bookmark_selected: false,
                matched_by: MissionLocationRef::DefaultMissionLocation,
            });
        };
        if loc == 0 {
            let m = *missions.first()?;
            let bms = mission_journal_bookmarks(snap, char_id, m);
            return Some(select_default_zero_location(&bms, m));
        }
        // Unscoped positive location number: match concrete bookmark / mission / agent IDs before
        // checking a single active mission's exact journal bookmark slice (`0..bookmarks.len()`).
        for &m in &missions {
            let bms = mission_journal_bookmarks(snap, char_id, m);
            if let Some(bm_id) = mission_bookmark_id_match(&bms, m, loc) {
                return Some(SelectedMissionWarp {
                    mission: m.clone(),
                    bookmark_id: Some(bm_id),
                    explicit_bookmark_selected: true,
                    matched_by: MissionLocationRef::ExplicitBookmark { bookmark_id: bm_id },
                });
            }
        }
        if let Some(&m) = missions
            .iter()
            .find(|m| i64::from(m.template_id) == loc || m.agent_id == loc)
        {
            return Some(SelectedMissionWarp {
                mission: m.clone(),
                bookmark_id: m.bookmark_id,
                explicit_bookmark_selected: false,
                matched_by: MissionLocationRef::MissionOrAgentId { id: loc },
            });
        }
        if missions.len() == 1 {
            let bms = mission_journal_bookmarks(snap, char_id, missions[0]);
            if let Some(sel) = usize::try_from(loc)
                .ok()
                .and_then(|idx| select_mission_with_journal_ordinal(&bms, missions[0], idx))
            {
                return Some(sel);
            }
        }
        None
    })
}

fn station_warp_target(ctx: &ServerContext, station_id: i64) -> Result<WarpTarget, BridgeRpcError> {
    let st = ctx.sde.stations.get(&station_id).ok_or_else(|| {
        AgentWarpError::InvalidMissionLocation.into_rpc(format!("Station {station_id} not found in SDE"),
        )
    })?;
    Ok(WarpTarget::StationOrCelestial {
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
) -> Result<WarpTarget, BridgeRpcError> {
    if let Some(agent) = ctx.sde.agents.get(&agent_id) {
        if let Some(ag_sys) = agent.solar_system_id {
            if ag_sys != current_sys_id {
                return Ok(WarpTarget::SpacePoint {
                    solar_system_id: ag_sys,
                    anchor_pos: DVec3::ZERO,
                });
            }
        }
        if let Some(st_id) = agent.station_id {
            return station_warp_target(ctx, st_id);
        }
        if let Some(pos) = sys_handle.get_ball_position_sync(agent_id) {
            return Ok(WarpTarget::AgentInSpace {
                agent_id,
                solar_system_id: current_sys_id,
                anchor_pos: pos,
            });
        }
        return Err(AgentWarpError::InvalidMissionLocation.into_rpc(format!("Agent {agent_id} has no warpable station or space ball"),
        ));
    }
    // Only treat an uncatalogued ballpark entity as an in-space agent when it is an Agent-in-Space ball (typeID 24).
    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(agent_id).await {
        if ball.type_id == 24 {
            return Ok(WarpTarget::AgentInSpace {
                agent_id,
                solar_system_id: current_sys_id,
                anchor_pos: ball.position,
            });
        }
    }
    Err(AgentWarpError::InvalidMissionLocation.into_rpc(format!("Unknown agent {agent_id} for warp target"),
    ))
}

fn mission_dungeon_warp_target(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    fallback_sys_id: u32,
    mission: &MissionInstance,
    selected_bookmark_id: Option<i64>,
) -> Result<WarpTarget, BridgeRpcError> {
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
        return Err(AgentWarpError::InvalidMissionLocation.into_rpc(format!("Mission {} has no solar system", mission.template_id),
        ));
    }
    if mission_sys_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH {
        return Err(BridgeRpcError::from_user_key(
            eve_proto::UserErrorKey::DunBlacklistCannotWarp,
        ));
    }
    let target_bmid = selected_bookmark_id.or(mission.bookmark_id);
    let now_unix = ctx.now_unix();
    let bm_pos = ctx.db.with_snapshot(|snap| -> Result<Option<DVec3>, BridgeRpcError> {
        let Some(bmid) = target_bmid else {
            return Ok(None);
        };
        validate_mission_bookmark_origin_in_snap(
            snap,
            Some(session),
            char_id,
            bmid,
            mission_sys_id,
            now_unix,
        )
    })?;
    let anchor_pos = bm_pos
        .or_else(|| mission.site_origin_f64().map(DVec3::from).filter(|pos| pos.is_finite()))
        .unwrap_or_else(|| {
            mission_site_origin_for_instance(ctx, mission_sys_id, mission.instance_id)
        });
    Ok(WarpTarget::Dungeon {
        mission_id: mission.template_id,
        bookmark_id: bm_pos.and(target_bmid),
        solar_system_id: mission_sys_id,
        anchor_pos,
    })
}

pub(crate) async fn resolve_agent_warp_target(
    ctx: &ServerContext,
    session: &SessionState,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    sys_id: u32,
    bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    parsed: &ParsedAgentWarpArgs<'_>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<WarpTarget, BridgeRpcError> {
    let bound_agent_id = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
    let kw_agent_id = kwargs
        .get("agentID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0);
    let loc_num = parsed.loc_num;
    let raw_loc_num = parsed.raw_loc_num;
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
                    raw_loc_num,
                    ref_agent_id,
                    kwargs,
                )
                .map(|sel| sel.mission.agent_id)
            })
            .ok_or_else(|| {
                AgentWarpError::InvalidMissionLocation.into_rpc("No agent specified for WarpToAgentInSpace",
                )
            })?;
        return agent_homebase_or_space_target(ctx, sys_handle, sys_id, target_agent_id).await;
    }

    if let Some(raw_type) = parsed.loc_type {
        let loc_type = raw_type.trim().to_ascii_lowercase();
        match loc_type.as_str() {
            "dungeon" | "mission" | "site" | "pocket" | "encounter" => {
                if let Some(sel) = find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    raw_loc_num,
                    ref_agent_id,
                    kwargs,
                ) {
                    return mission_dungeon_warp_target(
                        ctx,
                        session,
                        char_id,
                        sys_id,
                        &sel.mission,
                        sel.bookmark_id,
                    );
                }
                return Err(AgentWarpError::InvalidMissionLocation.into_rpc("No active mission dungeon location found",
                ));
            }
            "objective.destination" | "destination" | "courier" | "dropoff" => {
                if let Some(sel) = find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    raw_loc_num,
                    ref_agent_id,
                    kwargs,
                ) {
                    let m = &sel.mission;
                    if let Some(bmid) = sel.bookmark_id.filter(|_| {
                        sel.explicit_bookmark_selected && raw_loc_num.is_some_and(|n| n > 0)
                    }) {
                        let now_unix = ctx.now_unix();
                        let bm_station =
                            ctx.db.with_snapshot(|snap| -> Result<Option<i64>, BridgeRpcError> {
                                if crate::services::fleet_social::is_bookmark_expired_in_snap(
                                    snap, bmid, now_unix,
                                ) {
                                    return Ok(None);
                                }
                                let Some(bm) = snap.bookmarks.get(&bmid) else {
                                    return Ok(None);
                                };
                                if bm.solar_system_id
                                    == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH
                                {
                                    return Err(BridgeRpcError::from_user_key(
                                        eve_proto::UserErrorKey::DunBlacklistCannotWarp,
                                    ));
                                }
                                if !can_use_bookmark_in_snap(snap, session, char_id, bm) {
                                    return Ok(None);
                                }
                                Ok(bm.item_id.filter(|id| ctx.sde.stations.contains_key(id)))
                            })?;
                        if let Some(st_id) = bm_station {
                            return station_warp_target(ctx, st_id);
                        }
                    }
                    let dest_sid = m.destination_station_id().or(m.origin_station_id);
                    if let Some(st_id) = dest_sid {
                        return station_warp_target(ctx, st_id);
                    }
                    if sel.bookmark_id.is_some()
                        || m.bookmark_id.is_some()
                        || m.npcs_remaining() > 0
                    {
                        return mission_dungeon_warp_target(
                            ctx,
                            session,
                            char_id,
                            sys_id,
                            m,
                            sel.bookmark_id,
                        );
                    }
                    return agent_homebase_or_space_target(ctx, sys_handle, sys_id, m.agent_id)
                        .await;
                }
                return Err(AgentWarpError::InvalidMissionLocation.into_rpc("No active mission destination found",
                ));
            }
            "agenthomebase" | "station" | "agent" => {
                let mission_opt = find_accepted_mission_for_warp(
                    ctx,
                    char_id,
                    bound_agent_id,
                    raw_loc_num,
                    ref_agent_id,
                    kwargs,
                )
                .map(|sel| sel.mission);
                if let Some(ref m) = mission_opt {
                    if bound_agent_id.is_none() || bound_agent_id == Some(m.agent_id) {
                        if let Some(st_id) = m
                            .origin_station_id
                            .filter(|id| ctx.sde.stations.contains_key(id))
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
                        AgentWarpError::InvalidMissionLocation.into_rpc("No agent homebase location found",
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
                        AgentWarpError::InvalidMissionLocation.into_rpc("Bookmark warp requires bookmarkID",
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
                            AgentWarpError::BookmarkExpired.into_rpc(format!("Bookmark {bmid} has expired"),
                            )
                        } else {
                            AgentWarpError::BookmarkNotFound.into_rpc(format!("Bookmark {bmid} not found"),
                            )
                        });
                    };
                    if !can_use_bookmark_in_snap(snap, session, char_id, b) {
                        return Err(AgentWarpError::BookmarkNotFound.into_rpc(format!("Bookmark {bmid} not found"),
                        ));
                    }
                    if expired {
                        return Err(AgentWarpError::BookmarkExpired.into_rpc(format!("Bookmark {bmid} has expired"),
                        ));
                    }
                    if b.solar_system_id == crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH {
                        return Err(BridgeRpcError::from_user_key(
                            eve_proto::UserErrorKey::DunBlacklistCannotWarp,
                        ));
                    }
                    if !b.x.is_finite() || !b.y.is_finite() || !b.z.is_finite() {
                        return Err(AgentWarpError::BookmarkNotFound.into_rpc(format!("Bookmark {bmid} has invalid coordinates"),
                        ));
                    }
                    Ok(b.clone())
                })?;
                let mut anchor_pos = DVec3::new(bm.x, bm.y, bm.z);
                if let Some((st_id, st)) = bm
                    .item_id
                    .and_then(|id| ctx.sde.stations.get(&id).map(|st| (id, st)))
                {
                    if anchor_pos.length_squared() <= 1e-6 {
                        anchor_pos = st.position;
                    }
                    return Ok(WarpTarget::StationOrCelestial {
                        location_id: st_id,
                        solar_system_id: bm.solar_system_id,
                        anchor_pos,
                    });
                }
                return Ok(WarpTarget::Bookmark {
                    bookmark_id: bmid,
                    solar_system_id: bm.solar_system_id,
                    anchor_pos,
                });
            }
            _ => {
                return Err(AgentWarpError::InvalidMissionLocation.into_rpc(format!("Unsupported agent warp locationType '{raw_type}'"),
                ));
            }
        }
    }

    if let Some(id) = parsed.first_id.or(loc_num) {
        if ctx.sde.agents.contains_key(&id) {
            return agent_homebase_or_space_target(ctx, sys_handle, sys_id, id).await;
        }
        if let Some(sel) = find_accepted_mission_for_warp(
            ctx,
            char_id,
            bound_agent_id,
            Some(id),
            ref_agent_id,
            kwargs,
        ) {
            if matches!(
                sel.matched_by,
                MissionLocationRef::ExplicitBookmark { .. }
                    | MissionLocationRef::MissionOrAgentId { .. }
            ) && (i64::from(sel.mission.template_id) == id || sel.bookmark_id == Some(id))
            {
                return mission_dungeon_warp_target(
                    ctx,
                    session,
                    char_id,
                    sys_id,
                    &sel.mission,
                    sel.bookmark_id,
                );
            }
        }
        if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(id).await {
            if ball.type_id == 24 {
                return Ok(WarpTarget::AgentInSpace {
                    agent_id: id,
                    solar_system_id: sys_id,
                    anchor_pos: ball.position,
                });
            }
        }
        return Err(AgentWarpError::InvalidMissionLocation.into_rpc(format!("Unknown agent warp location {id}"),
        ));
    }

    if let Some(sel) = find_accepted_mission_for_warp(
        ctx,
        char_id,
        bound_agent_id,
        None,
        ref_agent_id,
        kwargs,
    ) {
        return mission_dungeon_warp_target(
            ctx,
            session,
            char_id,
            sys_id,
            &sel.mission,
            sel.bookmark_id,
        );
    }
    if let Some(ag_id) = bound_agent_id.or(kw_agent_id).or(ref_agent_id) {
        return agent_homebase_or_space_target(ctx, sys_handle, sys_id, ag_id).await;
    }
    Err(AgentWarpError::InvalidMissionLocation.into_rpc("No active mission or warp target specified",
    ))
}
