//! `nav_warp_interdiction` — Autopilot Waypoints, Celestial/Gate Warp Landing Points,
//! and Cargo Warp Vector Data Consumption (`feat:navigation.route-waypoints`,
//! `feat:navigation.warp-vector`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cluster_macho_misc_services::{persist_misc_state, with_misc_state};
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Zero Point Envelope landing standoff (`0 m` outside target ball radius, so `<= 2,500 m` warp jitter lands inside the `radius + 2,500 m` docking perimeter).
pub const STATION_DOCKING_RING_STANDOFF_M: f64 = 0.0;

/// Per-pilot reactivation cooldown (`300 s` = 5 min) after consuming a Warp Vector Data item.
pub const WARP_VECTOR_COOLDOWN_SECS: u64 = 300;

/// Mirrors `CharacterRecord.autopilot_waypoints` into `CharacterSettingsSvc` (`"waypoints"`),
/// removing the key when empty so fresh characters retain an empty settings dictionary.
pub fn sync_waypoints_to_char_settings(ctx: &ServerContext, char_id: i64, waypoints: &[i64]) {
    with_misc_state(ctx, |st| {
        if waypoints.is_empty() {
            if let Some(m) = st.char_settings.get_mut(&char_id) {
                m.remove("waypoints");
            }
        } else {
            let list = PyRep::List(waypoints.iter().map(|&w| PyRep::Integer(w)).collect());
            st.char_settings
                .entry(char_id)
                .or_default()
                .insert("waypoints".to_owned(), list);
        }
    });
    persist_misc_state(ctx);
}

/// Syncs `CharacterRecord.autopilot_waypoints` when a client saves `"waypoints"` via `charMgr.SaveCharacterSetting`.
pub fn sync_waypoints_from_char_settings(
    ctx: &ServerContext,
    char_id: i64,
    val: &PyRep,
) -> Result<(), BridgeRpcError> {
    let parsed: Vec<i64> = match val {
        PyRep::List(items) | PyRep::Tuple(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let dest_id = item.as_i64().unwrap_or(0);
                if !is_valid_waypoint_destination(ctx, dest_id) {
                    return Err(BridgeRpcError {
                        code: "InvalidDestination".into(),
                        message: format!(
                            "Destination {dest_id} is not a known solar system, station, or structure"
                        ),
                    });
                }
                out.push(dest_id);
            }
            out
        }
        _ => Vec::new(),
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.autopilot_waypoints = parsed.clone();
        }
    });
    sync_waypoints_to_char_settings(ctx, char_id, &parsed);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAutopilotUpdated",
        "charid",
        vec![],
    );
    Ok(())
}

/// Reconciles `CharacterRecord.autopilot_waypoints` and `char_settings["waypoints"]` on character login.
pub fn sync_waypoints_on_login(ctx: &ServerContext, char_id: i64) {
    let db_wps = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| c.autopilot_waypoints.clone())
            .unwrap_or_default()
    });
    if !db_wps.is_empty() {
        sync_waypoints_to_char_settings(ctx, char_id, &db_wps);
        return;
    }
    let settings_val = with_misc_state(ctx, |st| {
        st.char_settings
            .get(&char_id)
            .and_then(|m| m.get("waypoints"))
            .cloned()
    });
    if let Some(PyRep::List(items) | PyRep::Tuple(items)) = settings_val {
        let wps: Vec<i64> = items
            .iter()
            .filter_map(|v| v.as_i64())
            .filter(|&id| is_valid_waypoint_destination(ctx, id))
            .collect();
        if !wps.is_empty() {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.autopilot_waypoints = wps;
                }
            });
        }
    }
}

/// Advances the character's autopilot waypoint queue upon arriving at `arrived_location_id`
/// (solar system on stargate jump, or station/structure on dock), syncing `CharacterSettingsSvc`
/// and emitting `OnAutopilotUpdated`.
pub fn advance_waypoints_on_arrival(ctx: &ServerContext, char_id: i64, arrived_location_id: i64) {
    let updated_opt = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id)?;
        let pos = ch
            .autopilot_waypoints
            .iter()
            .position(|&w| w == arrived_location_id)?;
        ch.autopilot_waypoints.drain(0..=pos);
        Some(ch.autopilot_waypoints.clone())
    });
    if let Some(updated_wps) = updated_opt {
        sync_waypoints_to_char_settings(ctx, char_id, &updated_wps);
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnAutopilotUpdated",
            "charid",
            vec![],
        );
    }
}

/// Returns `true` when `dest_id` refers to a known solar system, NPC station, or Upwell structure.
pub fn is_valid_waypoint_destination(ctx: &ServerContext, dest_id: i64) -> bool {
    resolve_waypoint_solar_system(ctx, dest_id).is_some()
}

/// Resolves a waypoint destination ID (`dest_id`: solar system, NPC station, or Upwell structure)
/// to its containing solar system ID (`feat:navigation.route-waypoints`).
pub fn resolve_waypoint_solar_system(ctx: &ServerContext, dest_id: i64) -> Option<u32> {
    if dest_id <= 0 {
        return None;
    }
    if let Ok(sid) = u32::try_from(dest_id) {
        if ctx.sde.solar_systems.contains_key(&sid) {
            return Some(sid);
        }
    }
    if let Some(st) = ctx.sde.stations.get(&dest_id) {
        return Some(st.solar_system_id);
    }
    ctx.db.with_snapshot(|snap| {
        snap.structures
            .get(&dest_id)
            .filter(|st| st.state != "Destroyed")
            .map(|st| st.solar_system_id)
    })
}

fn dijkstra_single_leg(
    ctx: &ServerContext,
    adj: &HashMap<u32, Vec<u32>>,
    from_sys: u32,
    to_sys: u32,
    avoid_set: &HashSet<u32>,
    flag: &str,
) -> Option<Vec<u32>> {
    if from_sys == to_sys {
        return Some(vec![from_sys]);
    }
    if avoid_set.contains(&from_sys) || avoid_set.contains(&to_sys) {
        return None;
    }
    let mut dist: HashMap<u32, u64> = HashMap::new();
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();

    dist.insert(from_sys, 0);
    heap.push(Reverse((0, from_sys)));

    let mut found = false;
    while let Some(Reverse((d, curr))) = heap.pop() {
        if curr == to_sys {
            found = true;
            break;
        }
        if d > *dist.get(&curr).unwrap_or(&u64::MAX) {
            continue;
        }
        if let Some(neighbors) = adj.get(&curr) {
            for &nxt in neighbors {
                if avoid_set.contains(&nxt) && nxt != to_sys {
                    continue;
                }
                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&nxt)
                    .map(|s| s.security_status)
                    .unwrap_or(0.0);
                let step_cost: u64 = match flag {
                    "secure" => {
                        if sec >= 0.45 {
                            1
                        } else {
                            100
                        }
                    }
                    "insecure" => {
                        if sec < 0.45 {
                            1
                        } else {
                            100
                        }
                    }
                    _ => 1,
                };
                let next_d = d.saturating_add(step_cost);
                if next_d < *dist.get(&nxt).unwrap_or(&u64::MAX) {
                    dist.insert(nxt, next_d);
                    parent.insert(nxt, curr);
                    heap.push(Reverse((next_d, nxt)));
                }
            }
        }
    }

    if !found {
        return None;
    }

    let mut path = vec![to_sys];
    let mut cur = to_sys;
    while let Some(&prev) = parent.get(&cur) {
        path.push(prev);
        if prev == from_sys {
            break;
        }
        cur = prev;
    }
    path.reverse();
    Some(path)
}

/// Calculates a multi-waypoint stargate route respecting avoid lists and explicit waypoints (`feat:navigation.route-waypoints`).
///
/// Accepts explicit waypoint destinations (solar systems, NPC stations, or Upwell structures),
/// resolves their solar systems, and stitches shortest/secure/insecure path legs together without
/// duplicate junction hops, verifying no avoided solar systems are traversed.
pub fn calculate_route_with_waypoints(
    ctx: &ServerContext,
    origin_sys: u32,
    waypoints: &[i64],
    avoid_systems: &[u32],
    flag: &str,
) -> Result<Vec<u32>, BridgeRpcError> {
    if !ctx.sde.solar_systems.contains_key(&origin_sys) {
        return Err(BridgeRpcError {
            code: "InvalidOrigin".into(),
            message: format!("Origin solar system {origin_sys} not found in SDE"),
        });
    }

    let mut resolved_targets = Vec::with_capacity(waypoints.len());
    for &dest_id in waypoints {
        let sid = resolve_waypoint_solar_system(ctx, dest_id).ok_or_else(|| BridgeRpcError {
            code: "InvalidWaypoint".into(),
            message: format!("Destination {dest_id} is not a valid solar system, station, or structure"),
        })?;
        resolved_targets.push(sid);
    }

    let avoid_set: HashSet<u32> = avoid_systems.iter().copied().collect();
    if avoid_set.contains(&origin_sys) {
        return Err(BridgeRpcError {
            code: "OriginSystemInAvoidList".into(),
            message: format!("Origin system {origin_sys} is in the avoid list"),
        });
    }

    if resolved_targets.is_empty() {
        return Ok(vec![origin_sys]);
    }

    let base_adj = crate::esi::universe_routes::cached_base_jump_adjacency(ctx);
    let mut full_route = vec![origin_sys];
    let mut current_sys = origin_sys;

    for &next_sys in &resolved_targets {
        if next_sys == current_sys {
            continue;
        }
        if avoid_set.contains(&next_sys) {
            return Err(BridgeRpcError {
                code: "WaypointInAvoidList".into(),
                message: format!("Waypoint solar system {next_sys} is in the avoid list"),
            });
        }
        let leg = dijkstra_single_leg(ctx, &base_adj, current_sys, next_sys, &avoid_set, flag)
            .ok_or_else(|| {
                let code = if avoid_set.is_empty() {
                    "WaypointUnreachable"
                } else {
                    "RouteBlockedByAvoidList"
                };
                BridgeRpcError {
                    code: code.into(),
                    message: format!(
                        "No stargate route found between {current_sys} and {next_sys} respecting avoid list"
                    ),
                }
            })?;
        full_route.extend(leg.into_iter().skip(1));
        current_sys = next_sys;
    }

    Ok(full_route)
}

/// Computes the exact warp landing point for `target_id` in `sys_id`:
/// - Planets (`group 7`) and Moons (`group 8`): `2,500 m` outside celestial surface radius along approach vector.
/// - Stars (`group 6`): `100,000 m` outside star corona radius along approach vector.
/// - Stargates (`group 10` / `sde.jumps`): stargate perimeter (`gate_radius`) along approach vector (`0 m` from jump activation radius).
/// - NPC Stations (`sde.stations`) and Upwell Structures (`snap.structures`): Zero Point Envelope (`anchor_radius + STATION_DOCKING_RING_STANDOFF_M`, `0.0 m` outside target ball radius along approach vector, pairing with `<= 2,500 m` warp jitter and `target_radius + 2,500.0 m` docking perimeter).
/// - Asteroid Belts (`group 9`), Bookmarks, Signatures, Anomalies: exact anchor position.
pub fn resolve_warp_landing_point(
    ctx: &ServerContext,
    sys_id: u32,
    target_id: i64,
    ship_pos: DVec3,
    anchor_pos: DVec3,
) -> DVec3 {
    let dir = (anchor_pos - ship_pos).normalize_or_zero();
    if dir.length_squared() <= 1e-12 {
        return anchor_pos;
    }
    if ctx
        .sde
        .jumps
        .get(&target_id)
        .is_some_and(|j| j.from_solar_system_id == sys_id)
    {
        let gate_radius = ctx
            .sde
            .types
            .get(&16)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(14_051.0);
        return anchor_pos - dir * gate_radius;
    }
    if let Some(st) = ctx
        .sde
        .stations
        .get(&target_id)
        .filter(|st| st.solar_system_id == sys_id)
    {
        let anchor_radius = ctx
            .sde
            .types
            .get(&st.station_type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0);
        return anchor_pos - dir * (anchor_radius + STATION_DOCKING_RING_STANDOFF_M);
    }
    if let Some(struct_type_id) = ctx.db.with_snapshot(|snap| {
        snap.structures
            .get(&target_id)
            .filter(|st| st.solar_system_id == sys_id && st.state != "Destroyed")
            .map(|st| st.type_id)
    }) {
        let anchor_radius = ctx
            .sde
            .types
            .get(&struct_type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0);
        return anchor_pos - dir * (anchor_radius + STATION_DOCKING_RING_STANDOFF_M);
    }
    if let Some(cel) = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == target_id)
    {
        if cel.group_id == 7 || cel.group_id == 8 {
            let standoff = cel.radius.max(0.0) + 2_500.0;
            return anchor_pos - dir * standoff;
        }
        if cel.group_id == 6 {
            let standoff = cel.radius.max(0.0) + 100_000.0;
            return anchor_pos - dir * standoff;
        }
    }
    anchor_pos
}

/// Shared pre-warp restrictions for `CmdWarpToStuff`, `WarpToStuffAutopilot`, and `warpVectorMgr.UseVector`.
pub(crate) async fn validate_pre_warp_common(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle,
) -> Result<(), BridgeRpcError> {
    crate::services::space_nav_movement_guard::validate_pre_movement_locks(
        ctx,
        ship_id,
        &crate::services::space_nav_movement_guard::MovementIntent::Warp { sys_id },
    )?;
    crate::services::space_destiny::space_nav_travel::ensure_not_mjd_spooling(
        sys_handle,
        ship_id,
    )
    .await?;
    crate::services::pve_ess_reserve_banks::check_ess_intrusion_bubble_restriction(
        ctx, sys_id, ship_id, "warp",
    )
    .await?;
    crate::services::tourney_service::check_tourney_warp_restricted(ctx, sys_id, char_id)?;
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx,
        char_id,
        ship_id,
        sys_id,
        "warp",
        ctx.now_unix(),
    )?;
    Ok(())
}

/// Rejects mid-warp trajectory redirection once a ship has exited `WarpPhase::Aligning`.
pub(crate) fn ensure_not_in_active_warp(mode: &MovementMode) -> Result<(), BridgeRpcError> {
    if matches!(
        mode,
        MovementMode::Warp { phase, .. } if *phase != eve_destiny::WarpPhase::Aligning
    ) {
        return Err(BridgeRpcError {
            code: "ShipAlreadyInWarp".into(),
            message: "Cannot initiate or redirect warp while already in warp".into(),
        });
    }
    Ok(())
}

/// Shared warp-initiation side effects: halts active non-warp module cycles and abandons drones.
pub(crate) async fn on_warp_initiated_hooks(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    let _ = sys_handle.stop_non_warp_module_cycles(ship_id).await;
    crate::sp6_lifecycle::abandon_drones_on_warp_sync(ctx, char_id, ship_id, sys_id);
}

/// Evaluates origin/destination warp-disruption bubbles in `sys_handle` (`#520`):
/// - Rejects warp initiation if a non-bubble-immune ship is inside a warp bubble at `origin`.
/// - Adjusts `dest_pos` to the earliest bubble drag/stop intercept point along `(origin, dest_pos)`.
pub(crate) fn check_warp_interdiction_and_adjust_dest(
    sys_handle: &crate::system_actor::SystemHandle,
    ship_id: i64,
    origin: DVec3,
    dest_pos: DVec3,
) -> Result<DVec3, BridgeRpcError> {
    let bp = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner());
    if bp.is_ball_bubble_immune(ship_id) {
        return Ok(dest_pos);
    }
    if bp.warp_bubble_mgr.is_scrambled_at(origin) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "WarpScrambled: Warp drive disabled by warp disruption bubble".into(),
        });
    }
    if let Some((_bid, intercept_pos)) = bp.warp_bubble_mgr.check_interception(origin, dest_pos) {
        return Ok(intercept_pos);
    }
    Ok(dest_pos)
}

/// Executes full warp validation, bubble interdiction, capacitor deduction, and warp initiation
/// for `custom_agency_provider.warp_to_content` (`#520`).
pub(crate) async fn execute_agency_content_warp(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    current_ball: &eve_destiny::Ball,
    raw_dest_pos: DVec3,
) -> Result<(), BridgeRpcError> {
    validate_pre_warp_common(ctx, char_id, ship_id, sys_id, sys_handle).await?;
    ensure_not_in_active_warp(&current_ball.mode)?;
    crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(
        ctx, char_id, sys_id, ship_id,
    )
    .await;

    let validated = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::AgencyWarp { sys_id },
        )
        .await?;
    let is_covert_cloaked = validated.is_covert_cloaked;

    crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
        ctx,
        sys_id,
        current_ball.position.to_array(),
        raw_dest_pos.to_array(),
        raw_dest_pos.to_array(),
    )?;
    crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
        ctx,
        sys_id,
        raw_dest_pos,
        raw_dest_pos,
    )
    .await?;
    crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
        ctx,
        sys_id,
        raw_dest_pos,
        None,
    )?;

    let mut dest_pos = check_warp_interdiction_and_adjust_dest(
        sys_handle,
        ship_id,
        current_ball.position,
        raw_dest_pos,
    )?;
    let requested_dist_m = current_ball.position.distance(dest_pos);
    let warp_dir = (dest_pos - current_ball.position).normalize_or_zero();

    let (base_warp_cap_need, sde_max_cap) = ctx
        .sde
        .types
        .get(&current_ball.type_id)
        .map(|t| {
            let wcn = t.default_attributes.get(&153).copied().unwrap_or(1.24e-6);
            let cap = t.default_attributes.get(&482).copied().unwrap_or(250.0);
            (wcn, cap)
        })
        .unwrap_or((1.24e-6, 250.0));
    let wdo_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|m| m.get(&3455))
            .map(|s| s.level.min(5))
            .unwrap_or(0)
    });
    let eff_warp_cap_need = base_warp_cap_need * (1.0 - 0.10 * (wdo_lvl as f64));
    let cap_per_m = (eff_warp_cap_need * current_ball.mass) / 149_597_870_700.0;
    let req_cap = cap_per_m * requested_dist_m;

    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
        if (cs.max_capacitor - 250.0).abs() < 1e-6
            && (cs.capacitor - 250.0).abs() < 1e-6
            && sde_max_cap > 250.0
        {
            cs.max_capacitor = sde_max_cap;
            cs.capacitor = sde_max_cap;
        }
        if cs.capacitor < req_cap {
            let affordable_dist_m = if cap_per_m > 0.0 {
                cs.capacitor / cap_per_m
            } else {
                requested_dist_m
            };
            if affordable_dist_m < 150_000.0 {
                return Err(BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!(
                        "Insufficient capacitor ({:.3} GJ) to initiate minimum 150 km warp",
                        cs.capacitor
                    ),
                });
            }
            dest_pos = current_ball.position + warp_dir * affordable_dist_m;
            cs.capacitor = 0.0;
        } else {
            cs.capacitor = (cs.capacitor - req_cap).max(0.0);
        }
        let _ = sys_handle.set_capacitor(ship_id, cs.capacitor).await;
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnCapacitorChange",
            "charid",
            vec![
                PyRep::Integer(ship_id),
                PyRep::Real(cs.capacitor),
                PyRep::Real(cs.max_capacitor),
            ],
        );
    }

    let warp_speed = crate::services::space_destiny::compute_pilot_warp_speed_au_s(
        ctx,
        char_id,
        sys_id,
        current_ball.type_id,
    );
    if !is_covert_cloaked {
        let _ = crate::services::notify_emitters_seasons_tactical::break_gate_cloak_and_notify(
            ctx, sys_handle, char_id, ship_id,
        )
        .await;
    }
    let mode = MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
    if let Err(e) = sys_handle.set_movement_mode(ship_id, mode).await {
        let code = if e.contains("WarpScrambled") {
            "WarpScrambled"
        } else if e.contains("CloakedCannotWarp") {
            "CloakedCannotWarp"
        } else {
            "DestinyCommandFailed"
        };
        return Err(BridgeRpcError {
            code: code.into(),
            message: e,
        });
    }
    on_warp_initiated_hooks(ctx, sys_handle, char_id, ship_id, sys_id).await;
    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, char_id);
    Ok(())
}

fn rollback_warp_vector_reservation(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    prev_item: eve_db::InventoryItemRecord,
    prev_cd_until: Option<u64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(target_id, prev_item);
        if let Some(old_cd) = prev_cd_until {
            snap.cluster_and_runtime
                .warp_vector_cooldown_until
                .insert(char_id, old_cd);
        } else {
            snap.cluster_and_runtime
                .warp_vector_cooldown_until
                .remove(&char_id);
        }
    });
}

/// `warpVectorMgr.UseVector(itemID)` (`feat:navigation.warp-vector`):
/// Validates consumable Warp Vector Data items in ship cargo (`group_id == 4599`, e.g. `77825`
/// `Jovian Symbolic Navigator`), checking proximity to the system star (`attr 5432`), applying the
/// star-centered directional vector delta `(attr 5429, attr 5430, attr 5431)` with `>= 150 km`
/// minimum warp distance, and atomically reserving 1 unit + 300s per-character cooldown.
pub async fn handle_use_warp_vector(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate a warp vector while docked".into(),
        });
    }
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship in space".into(),
    })?;
    let sys_id = session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .ok_or_else(|| BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        })?;
    let target_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "warpVectorMgr.UseVector requires a positive vector itemID".into(),
        })?;
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Cannot activate a warp vector targeting your own active ship".into(),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    validate_pre_warp_common(ctx, char_id, ship_id, sys_id, &sys_handle).await?;
    let validated = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::VectorWarp { sys_id },
        )
        .await?;
    let ship_ball = validated.ball.ok_or_else(|| BridgeRpcError {
        code: "ShipNotInSpace".into(),
        message: format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
    })?;
    ensure_not_in_active_warp(&ship_ball.mode)?;

    // `warpVectorMgr.UseVector(itemID)` only accepts an owned Warp Vector Data item (`group 4599`) in active ship cargo.
    let star_opt = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6);
    let star_pos = star_opt.as_ref().map(|c| c.position).unwrap_or(DVec3::ZERO);
    let has_star = star_opt.is_some();
    let sys_radius_opt = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.radius)
        .filter(|&r| r > 0.0);
    let now_unix = ctx.now_unix_secs();

    // Atomically validate the cargo item, cooldown, star proximity, and >= 150 km vector distance,
    // and reserve 1 unit + 300s per-character cooldown before awaiting `set_movement_mode`.
    let (dest_pos, dist_m, remaining_qty, prev_item, prev_cd_until) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(vec_item) = snap.items.get(&target_id).cloned() else {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!("Warp vector item {target_id} not found in active ship {ship_id} cargo"),
            });
        };
        if vec_item.owner_id != char_id
            || vec_item.location_id != ship_id
            || vec_item.flag != InventoryFlag::Cargo
            || vec_item.quantity <= 0
        {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!("Warp vector item {target_id} must be in active ship {ship_id} cargo"),
            });
        }
        let mod_t = ctx.sde.types.get(&vec_item.type_id).ok_or_else(|| BridgeRpcError {
            code: "InvalidVectorItem".into(),
            message: format!("Unknown vector item type {}", vec_item.type_id),
        })?;
        if mod_t.group_id != 4599 {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!(
                    "Item {} (group {}) is not a valid Warp Vector Data item (group 4599)",
                    mod_t.name, mod_t.group_id
                ),
            });
        }

        let prev_cd = snap
            .cluster_and_runtime
            .warp_vector_cooldown_until
            .get(&char_id)
            .copied();
        if prev_cd.unwrap_or(0) > now_unix {
            return Err(BridgeRpcError {
                code: "WarpVectorCooldown".into(),
                message: format!(
                    "Warp vector reactivation cooldown active for {} s",
                    prev_cd.unwrap_or(0) - now_unix
                ),
            });
        }

        let max_star_dist = vec_item
            .dynamic_attributes
            .get(&5432)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5432).copied())
            .unwrap_or(1_496_000_000_000.0);
        if has_star {
            let star_dist = ship_ball.position.distance(star_pos);
            if star_dist > max_star_dist {
                return Err(BridgeRpcError {
                    code: "WarpVectorTooFarFromStar".into(),
                    message: format!(
                        "Ship is {:.1} m from system star (maximum {:.1} m for warp vector activation)",
                        star_dist, max_star_dist
                    ),
                });
            }
        }

        let dx = vec_item
            .dynamic_attributes
            .get(&5429)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5429).copied())
            .unwrap_or(0.0);
        let dy = vec_item
            .dynamic_attributes
            .get(&5430)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5430).copied())
            .unwrap_or(0.0);
        let dz = vec_item
            .dynamic_attributes
            .get(&5431)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5431).copied())
            .unwrap_or(0.0);
        let delta = DVec3::new(dx, dy, dz);
        if !delta.is_finite() {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Warp vector coordinates must be finite".into(),
            });
        }
        let mut dest_pos = star_pos + delta;
        if let Some(sys_radius) = sys_radius_opt {
            let rel = dest_pos - star_pos;
            if rel.length() > sys_radius {
                dest_pos = star_pos + rel.normalize_or_zero() * sys_radius;
            }
        }
        let dist_m = ship_ball.position.distance(dest_pos);
        if dist_m < 150_000.0 {
            return Err(BridgeRpcError {
                code: "WarpDistanceTooShort".into(),
                message: format!(
                    "Warp vector distance {:.1} m is below minimum 150,000 m",
                    dist_m
                ),
            });
        }

        snap.cluster_and_runtime
            .warp_vector_cooldown_until
            .insert(char_id, now_unix.saturating_add(WARP_VECTOR_COOLDOWN_SECS));
        let mut rem = 0;
        if let Some(it) = snap.items.get_mut(&target_id) {
            it.quantity = (it.quantity - 1).max(0);
            it.version += 1;
            rem = it.quantity;
        }
        if rem == 0 {
            snap.items.remove(&target_id);
        }
        Ok((dest_pos, dist_m, rem, vec_item, prev_cd))
    })?;

    if let Err(e) = crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
        ctx,
        sys_id,
        ship_ball.position.to_array(),
        dest_pos.to_array(),
        dest_pos.to_array(),
    ) {
        rollback_warp_vector_reservation(ctx, char_id, target_id, prev_item, prev_cd_until);
        return Err(e);
    }
    if let Err(e) = crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
        ctx,
        sys_id,
        dest_pos,
        dest_pos,
    )
    .await
    {
        rollback_warp_vector_reservation(ctx, char_id, target_id, prev_item, prev_cd_until);
        return Err(e);
    }

    let warp_speed = crate::services::space_destiny::compute_pilot_warp_speed_au_s(
        ctx,
        char_id,
        sys_id,
        ship_ball.type_id,
    );
    if !validated.is_covert_cloaked {
        let _ = crate::services::notify_emitters_seasons_tactical::break_gate_cloak_and_notify(
            ctx,
            &sys_handle,
            char_id,
            ship_id,
        )
        .await;
    }
    let warp_mode = MovementMode::new_warp(ship_ball.position, dest_pos, warp_speed);
    if let Err(e) = sys_handle.set_movement_mode(ship_id, warp_mode).await {
        rollback_warp_vector_reservation(ctx, char_id, target_id, prev_item, prev_cd_until);
        return Err(BridgeRpcError {
            code: "WarpFailed".into(),
            message: e,
        });
    }
    on_warp_initiated_hooks(ctx, &sys_handle, char_id, ship_id, sys_id).await;

    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, char_id);

    let align_dir = (dest_pos - ship_ball.position).normalize_or_zero();
    let entry_speed = (ship_ball.max_velocity * 0.75).max(1.0);
    let align_time_s = ship_ball.align_time_seconds();

    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("vectorItemID", PyRep::Integer(target_id)),
        ("itemID", PyRep::Integer(target_id)),
        ("consumed", PyRep::Bool(true)),
        ("remainingQuantity", PyRep::Integer(remaining_qty as i64)),
        ("distanceMeters", PyRep::Real(dist_m)),
        ("warpSpeedAuPerSec", PyRep::Real(warp_speed)),
        ("entrySpeed", PyRep::Real(entry_speed)),
        ("alignTimeSeconds", PyRep::Real(align_time_s)),
        (
            "alignmentVector",
            PyRep::Tuple(vec![
                PyRep::Real(align_dir.x),
                PyRep::Real(align_dir.y),
                PyRep::Real(align_dir.z),
            ]),
        ),
        (
            "destination",
            PyRep::Tuple(vec![
                PyRep::Real(dest_pos.x),
                PyRep::Real(dest_pos.y),
                PyRep::Real(dest_pos.z),
            ]),
        ),
    ]))
}

async fn handle_get_session_change_timer(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let sys_id = session
        .solar_system_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
        })
        .unwrap_or(0);
    let cur_tick_opt = if let Some(ship_id) = session.ship_id {
        let h = ctx.systems.get_or_spawn(sys_id);
        h.get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(t, _)| (sys_id, t))
    } else {
        None
    };
    if let Some(active) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        cur_tick_opt,
    ) {
        let rem = active.expires_unix.saturating_sub(now_unix);
        return Ok(PyRep::keyval([
            ("active", PyRep::Bool(true)),
            ("remainingSeconds", PyRep::Integer(rem as i64)),
            ("trigger", PyRep::String(active.reason)),
        ]));
    }
    Ok(PyRep::keyval([
        ("active", PyRep::Bool(false)),
        ("remainingSeconds", PyRep::Integer(0)),
        ("trigger", PyRep::None),
    ]))
}

pub(crate) async fn handle_set_session_check(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if args.is_empty() {
        return handle_get_session_change_timer(ctx, session).await;
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let sys_id = session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .unwrap_or(0);
    let cur_tick_opt = if let Some(ship_id) = session.ship_id {
        if sys_id > 0 {
            let h = ctx.systems.get_or_spawn(sys_id);
            h.get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(t, _)| (sys_id, t))
        } else {
            None
        }
    } else {
        None
    };

    let reason = match args.first() {
        Some(PyRep::Dict(_)) | Some(PyRep::KeyVal(_)) | Some(PyRep::Object { .. }) => {
            let check_rep = args.first().unwrap();
            if let Some(req_sys) = check_rep
                .dict_get("solarsystemid2")
                .or_else(|| check_rep.dict_get("solarsystemid"))
                .and_then(PyRep::as_i64)
            {
                if req_sys > 0 && req_sys as u32 != sys_id {
                    return Err(BridgeRpcError {
                        code: "SessionCheckMismatch".into(),
                        message: format!("Session solarsystemid2 is {sys_id}, not {req_sys}"),
                    });
                }
            }
            "session_change"
        }
        Some(other) => other.as_str().unwrap_or("session_change"),
        None => "session_change",
    };
    let is_valid_trigger = matches!(
        reason,
        "login"
            | "stargate_jump"
            | "wormhole_jump"
            | "jump_bridge"
            | "jump_drive"
            | "dock"
            | "undock"
            | "fleet_join"
            | "fleet_position_change"
            | "fleet_change"
            | "ship_board"
            | "ship_eject"
            | "board_or_eject"
            | "pod_transition"
            | "clone_jump"
            | "clone_swap"
            | "corp_join"
            | "corp_leave"
            | "wallet_division_change"
            | "session_change"
    );
    if !is_valid_trigger {
        return Err(BridgeRpcError {
            code: "InvalidSessionChangeTrigger".into(),
            message: format!("Unknown session-change trigger '{reason}'"),
        });
    }
    if let Some(active) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        cur_tick_opt,
    ) {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot perform session change '{reason}' while 10s timer from '{}' is active",
                active.reason
            ),
        });
    }
    let tick = cur_tick_opt.map(|(_, t)| t).unwrap_or(0);
    crate::services::space_destiny::record_client_session_check_timer(
        ctx, char_id, now_unix, sys_id, tick, reason,
    );
    Ok(PyRep::keyval([
        ("sessionChangeTimerSeconds", PyRep::Integer(10)),
        ("trigger", PyRep::String(reason.to_string())),
        ("expiresUnix", PyRep::Integer((now_unix + 10) as i64)),
    ]))
}

#[cfg(test)]
#[path = "nav_warp_interdiction_tests.rs"]
mod nav_warp_interdiction_tests;

#[cfg(test)]
#[path = "nav_warp_interdiction_edges_tests.rs"]
mod nav_warp_interdiction_edges_tests;

