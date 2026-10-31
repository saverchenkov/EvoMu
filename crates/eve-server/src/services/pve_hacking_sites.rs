//! `pve_hacking_sites` — Data Sites, Relic Sites, Node-Grid Hacking Minigame (`hackingMgr`),
//! Loot Spew Mini-Containers, Ghost Site Hidden Timer & Detonation, and Sleeper Cache Rift Puzzles.
//!
// parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.hacking-minigame
// parity: feat:pve.loot-spew, feat:pve.ghost-sites, feat:pve.sleeper-caches

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    ensure_system_pve_sites_seeded, next_pve_seq, now_sec,
    with_pve_exploration_reg, HackingGameSession, HackingNodeKind, HackingTileState,
    HackingUtilityKind, PveExplorationRegistry, SpewedLootCan,
};
pub use crate::services::pve_hacking_board::{
    build_hacking_grid_procedural, detonate_ghost_site, detonate_ghost_site_with_can,
    hacking_tile_distance_indicator, handle_second_hack_failure_explosion,
    resolve_fitted_analyzer_virus_stats, resolve_hacking_turn_combat,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use std::collections::HashMap;

type ItemId = i64;

pub fn hacking_tile_axial_coords(tile_idx: u32) -> (i32, i32) {
    ((tile_idx % 6) as i32, (tile_idx / 6) as i32)
}

pub fn hacking_hex_distance(a: u32, b: u32) -> u32 {
    let (q1, r1) = hacking_tile_axial_coords(a);
    let (q2, r2) = hacking_tile_axial_coords(b);
    (((q1 - q2).abs() + (r1 - r2).abs() + (q1 + r1 - q2 - r2).abs()) / 2) as u32
}

pub fn are_hacking_tiles_adjacent(a: u32, b: u32) -> bool {
    hacking_hex_distance(a, b) == 1
}

pub fn is_blocking_defensive_tile(t: &HackingTileState) -> bool {
    t.coherence > 0 && matches!(t.kind, HackingNodeKind::Firewall | HackingNodeKind::Antivirus | HackingNodeKind::VirusSuppressor | HackingNodeKind::RestorationNode)
}

pub fn hacking_tile_distance_to_core(tile_idx: u32, tiles: &[HackingTileState]) -> u32 {
    let core_idx = tiles
        .iter()
        .find(|t| t.kind == HackingNodeKind::SystemCore)
        .map(|t| t.tile_index)
        .unwrap_or(15);
    if tile_idx == core_idx {
        return 0;
    }
    hacking_hex_distance(tile_idx, core_idx).max(1)
}

// parity: feat:exploration.hacking-node-network, feat:exploration.hacking-coherence-and-strength
pub fn build_hacking_grid(container_id: ItemId, is_relic: bool) -> Vec<HackingTileState> {
    build_hacking_grid_procedural(container_id, is_relic, 2, 0)
}

/// Generate scattered mini-containers around a successfully hacked Data or Relic container (`feat:pve.loot-spew`).
// parity: feat:pve.loot-spew, feat:pve.data-sites, feat:pve.relic-sites
pub fn spawn_loot_spew_for_container(
    ctx: &ServerContext,
    reg: &mut PveExplorationRegistry,
    solar_system_id: SolarSystemId,
    container_id: ItemId,
    is_relic: bool,
    center: [f64; 3],
) -> Vec<SpewedLootCan> {
    let loot_items = crate::services::pve_exploration_sites::generate_exploration_container_loot(
        &ctx.sde,
        solar_system_id,
        is_relic,
    );

    let now = now_sec(reg);
    let mut cans = Vec::with_capacity(loot_items.len());
    for (idx, (loot_tid, loot_qty, _)) in loot_items.into_iter().enumerate() {
        let can_id = 1_890_000_000 + next_pve_seq(reg);
        let can_pos = [
            center[0] + ((idx as f64) - 1.5) * 800.0,
            center[1] + ((idx % 2) as f64) * 600.0,
            center[2] + 250.0,
        ];
        let can = SpewedLootCan {
            can_item_id: can_id,
            parent_container_id: container_id,
            solar_system_id,
            position: can_pos,
            loot_type_id: loot_tid,
            loot_quantity: loot_qty,
            spawned_at_sec: now,
            despawn_at_sec: now + 60,
            scooped: false,
        };
        reg.spewed_cans.insert(can_id, can.clone());
        cans.push(can);
    }
    cans
}

fn find_active_hacking_container(ctx: &ServerContext, char_id: i64) -> Option<i64> {
    with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games
            .iter()
            .find(|((cid, _), g)| *cid == char_id && (g.status == "InProgress" || g.status == "active"))
            .map(|((_, cont_id), _)| *cont_id)
            .or_else(|| {
                reg.hacking_games
                    .keys()
                    .find(|(cid, _)| *cid == char_id)
                    .map(|(_, cont_id)| *cont_id)
            })
    })
    .or_else(|| crate::services::inventory_fitting::fitting_scanners_superweapons::active_hacking_target_for_char(char_id))
}

fn extract_tile_index(arg: Option<&PyRep>) -> Option<i64> {
    match arg? {
        PyRep::Integer(i) => Some(*i),
        PyRep::Tuple(items) | PyRep::List(items) => {
            if items.len() >= 2 {
                let x = items[0].as_i64()?;
                let y = items[1].as_i64()?;
                Some((y * 6 + x).clamp(0, 23))
            } else {
                items.first().and_then(|v| v.as_i64())
            }
        }
        _ => None,
    }
}

async fn validate_active_hacking_ship_and_range(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    container_id: i64,
) -> Result<(), BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot continue hacking minigame while docked".into(),
        });
    }
    let ship_id = session.ship_id.unwrap_or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0))
    });
    let game_meta = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games.get(&(char_id, container_id)).map(|g| {
            let spos = reg
                .sites_by_system
                .get(&g.solar_system_id)
                .and_then(|l| l.iter().find(|s| s.site_id == g.site_id))
                .map(|s| s.position)
                .unwrap_or([0.0, 0.0, 0.0]);
            (g.solar_system_id, g.is_relic, spos)
        })
    });
    if let Some((g_sys_id, g_is_relic, site_pos)) = game_meta {
        if session.solar_system_id.is_some_and(|s| s != g_sys_id) {
            return Err(BridgeRpcError {
                code: "NotInSiteSystem".into(),
                message: "Cannot continue hacking minigame from another solar system".into(),
            });
        }
        let sys_handle = ctx.systems.get_or_spawn(g_sys_id);
        if sys_handle
            .get_combat_state(ship_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|cs| cs.is_cloaked)
        {
            return Err(BridgeRpcError {
                code: "CannotHackWhileCloaked".into(),
                message: "Cannot continue hacking minigame while cloaked".into(),
            });
        }
        if let Ok(Some((_, sb))) = sys_handle.get_ball_snapshot(ship_id).await {
            let max_range = resolve_fitted_analyzer_virus_stats(ctx, session, char_id, g_is_relic)
                .map(|v| v.3)
                .unwrap_or(5_000.0);
            let can_pos = if let Ok(Some((_, cb))) = sys_handle.get_ball_snapshot(container_id).await {
                cb.position
            } else if let Some(pos) = ctx.db.with_snapshot(|s| s.item_positions.get(&container_id).copied()) {
                glam::DVec3::from_array(pos)
            } else {
                glam::DVec3::from_array(site_pos)
            };
            let can_radius = sys_handle
                .get_ball_snapshot(container_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.radius)
                .unwrap_or(0.0);
            let dist = (sb.position.distance(can_pos) - sb.radius - can_radius).max(0.0);
            if dist > max_range {
                return Err(BridgeRpcError {
                    code: "OutOfRange".into(),
                    message: format!(
                        "Target container is {dist:.1} m away (analyzer max range is {max_range:.1} m)"
                    ),
                });
            }
        }
    }
    Ok(())
}

pub async fn handle_pve_hacking(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: feat:pve.hacking-minigame, feat:pve.data-sites, feat:pve.relic-sites, feat:pve.ghost-sites, feat:pve.sleeper-caches
        ("hackingMgr", "StartNewGameInstance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let container_id = if args.len() == 1 && matches!(args.first(), Some(PyRep::None)) {
                0
            } else if let Some(explicit_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&id| args.len() == 1 || id >= 1_000_000)
            {
                explicit_id
            } else if let Some(second_id) = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id >= 1_000_000) {
                second_id
            } else {
                find_active_hacking_container(ctx, char_id).unwrap_or_else(|| {
                    with_pve_exploration_reg(ctx, |reg| {
                        reg.sites_by_system
                            .get(&sys_id)
                            .and_then(|sites| {
                                sites
                                    .iter()
                                    .filter(|s| !s.cleared)
                                    .flat_map(|s| s.hackable_can_ids.iter().copied())
                                    .find(|&cid| !reg.hacked_containers.contains(&(sys_id, cid)))
                            })
                            .unwrap_or(0)
                    })
                })
            };
            if container_id == 0 {
                return Some(Err(BridgeRpcError { code: "TargetNotFound".into(), message: "Target container not specified".into() }));
            }

            let struct_info = ctx.db.with_snapshot(|s| {
                let caller_corp = s.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                s.structures.get(&container_id).filter(|st| st.solar_system_id == sys_id && st.state != "Destroyed").map(|st| (st.position, st.owner_corp_id, caller_corp))
            });
            if let Some((_, owner_corp_id, caller_corp)) = struct_info {
                if caller_corp != 0 && caller_corp == owner_corp_id {
                    return Some(Err(BridgeRpcError { code: "CannotHackOwnStructure".into(), message: "Cannot run a Data Analyzer hack against a structure owned by your own corporation".into() }));
                }
            }
            let struct_pos = struct_info.map(|(p, _, _)| p);
            let site_res = with_pve_exploration_reg(ctx, |reg| {
                if let Some(st_pos) = struct_pos {
                    if reg.structure_char_failures.get(&(char_id, container_id)).copied().unwrap_or(0) >= 2 {
                        return Err(BridgeRpcError { code: "HackingGameAlreadyFinished".into(), message: "You have failed to hack this structure twice and are locked out".into() });
                    }
                    if reg.hacking_games.values().any(|g| g.container_id == container_id && g.status == "InProgress") {
                        return Err(BridgeRpcError { code: "HackingGameAlreadyInProgress".into(), message: "Another pilot is already hacking this target".into() });
                    }
                    return Ok((format!("STRUCTURE_{container_id}"), false, false, false, st_pos));
                }
                let now = now_sec(reg);
                let sites = reg.sites_by_system.get_mut(&sys_id).ok_or_else(|| BridgeRpcError { code: "TargetNotFound".into(), message: "Target container not found".into() })?;
                let s = sites.iter_mut().find(|x| x.hackable_can_ids.contains(&container_id) && !x.cleared).ok_or_else(|| BridgeRpcError { code: "TargetNotFound".into(), message: "Target container not found".into() })?;
                let ghost_timer_expired = s.site_category == "Ghost"
                    && !s.ghost_detonated
                    && s.ghost_timer_started_at
                        .is_some_and(|st| now >= st + s.ghost_detonate_after_sec.max(90));
                if !ghost_timer_expired
                    && ((s.site_category == "Ghost" && s.ghost_detonated)
                        || reg.hacked_containers.contains(&(sys_id, container_id))
                        || reg.failure_counts.get(&(sys_id, container_id)).copied().unwrap_or(0) >= 2)
                {
                    return Err(BridgeRpcError { code: "HackingGameAlreadyFinished".into(), message: "Container has already been hacked or emptied".into() });
                }
                if reg.hacking_games.values().any(|g| g.container_id == container_id && g.status == "InProgress") {
                    return Err(BridgeRpcError { code: "HackingGameAlreadyInProgress".into(), message: "Another pilot is already hacking this container".into() });
                }
                Ok((s.site_id.clone(), s.site_category == "Relic", s.site_category == "Ghost", s.site_category == "SleeperCache", s.position))
            });

            let (site_id, is_relic, is_ghost, is_sleeper_cache, site_pos) = match site_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            // BF-PVE-HACK-PROXIMITY-FAILOPEN-04: Explicitly reject hacking while docked or without a ship in space
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError { code: "ShipDocked".into(), message: "Cannot initiate hacking minigame while docked".into() }));
            }
            let ship_id = match session.ship_id {
                Some(sid) if sid > 0 => sid,
                _ => return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "Session has no active ship".into() })),
            };

            let ghost_expired = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
                if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
                    if let Some(s) = sites.iter_mut().find(|x| x.site_id == site_id) {
                        let dur = s.ghost_detonate_after_sec.max(90);
                        if is_ghost && s.ghost_timer_started_at.is_none() {
                            s.ghost_timer_started_at = Some(now);
                            s.ghost_detonate_after_sec = dur;
                        }
                        return is_ghost
                            && s.ghost_timer_started_at
                                .is_some_and(|st| now >= st + s.ghost_detonate_after_sec);
                    }
                }
                false
            });

            if ghost_expired {
                with_pve_exploration_reg(ctx, |reg| {
                    reg.hacked_containers.insert((sys_id, container_id));
                });
                let (dmg, npcs) = detonate_ghost_site(ctx, session, sys_id, &site_id).await;
                return Some(Ok(PyRep::keyval([
                    ("status", PyRep::String("GhostSiteDetonated".into())),
                    ("explosiveDamageApplied", PyRep::Real(dmg)),
                    (
                        "spawnedPirateNpcBallIDs",
                        PyRep::List(npcs.into_iter().map(PyRep::Integer).collect()),
                    ),
                ])));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let sb = match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(Some((_, ball))) => ball,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "ShipNotInSpace".into(),
                        message: format!("Ship {ship_id} is not spawned in space in system {sys_id}"),
                    }));
                }
            };
            if sys_handle
                .get_combat_state(ship_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_cloaked)
            {
                return Some(Err(BridgeRpcError {
                    code: "CannotHackWhileCloaked".into(),
                    message: "Cannot initiate hacking minigame while cloaked".into(),
                }));
            }

            let (virus_coh, virus_str, utility_slots, analyzer_max_range) =
                match resolve_fitted_analyzer_virus_stats(ctx, session, char_id, is_relic) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };

            let can_pos = if let Ok(Some((_, cb))) = sys_handle.get_ball_snapshot(container_id).await {
                cb.position
            } else if let Some(pos) = ctx.db.with_snapshot(|s| s.item_positions.get(&container_id).copied()) {
                glam::DVec3::from_array(pos)
            } else {
                glam::DVec3::from_array(site_pos)
            };
            let can_radius = sys_handle.get_ball_snapshot(container_id).await.ok().flatten().map(|(_, b)| b.radius).unwrap_or(0.0);
            let dist = (sb.position.distance(can_pos) - sb.radius - can_radius).max(0.0);
            if dist > analyzer_max_range {
                return Some(Err(BridgeRpcError {
                    code: "OutOfRange".into(),
                    message: format!("Target container is {dist:.1} m away (analyzer max range is {analyzer_max_range:.1} m)"),
                }));
            }

            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            let min_difficulty: u8 = if is_ghost {
                4
            } else if (31_000_000..32_000_000).contains(&sys_id) || sys_sec <= 0.0 {
                3
            } else if sys_sec < 0.45 || is_relic {
                2
            } else {
                2
            };
            let difficulty = args
                .get(4)
                .and_then(|a| a.as_i64())
                .map(|d| (d as u8).clamp(min_difficulty, 5))
                .unwrap_or(min_difficulty);
            let rng_seed = ctx.mechanic_rng.try_next_u64().unwrap_or(0);
            let game = with_pve_exploration_reg(ctx, |reg| {
                let game_id = 500_000 + next_pve_seq(reg);
                let tiles = build_hacking_grid_procedural(container_id, is_relic, difficulty, rng_seed);
                let g = HackingGameSession {
                    game_id,
                    character_id: char_id,
                    solar_system_id: sys_id,
                    container_id,
                    site_id: site_id.clone(),
                    is_relic,
                    is_ghost_site: is_ghost,
                    is_sleeper_cache_puzzle: is_sleeper_cache,
                    virus_coherence: virus_coh,
                    max_virus_coherence: virus_coh,
                    base_virus_strength: virus_str,
                    polymorphic_shield_charges: 0,
                    secondary_vector_target: None,
                    utilities: vec![None; utility_slots],
                    tiles,
                    status: "InProgress".into(),
                };
                reg.hacking_games.insert((char_id, container_id), g.clone());
                g
            });

            let start_indicator = hacking_tile_distance_indicator(0, &game.tiles);
            let start_events = vec![PyRep::Dict(vec![
                (PyRep::String("eventID".into()), PyRep::Integer(1)),
                (
                    PyRep::String("eventData".into()),
                    PyRep::keyval([
                        ("gameID", PyRep::Integer(game.game_id)),
                        ("containerID", PyRep::Integer(container_id)),
                        ("tileIndex", PyRep::Integer(0)),
                        ("gridTopology", PyRep::String("Hexagonal".into())),
                        ("status", PyRep::String("InProgress".into())),
                        ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                        ("virusStrength", PyRep::Integer(game.base_virus_strength as i64)),
                        ("distanceIndicator", PyRep::Integer(start_indicator as i64)),
                    ]),
                ),
            ])];
            session.push_notification("OnHackingTurnComplete", vec![PyRep::List(start_events)]);
            Some(Ok(PyRep::keyval([
                ("gameID", PyRep::Integer(game.game_id)),
                ("containerID", PyRep::Integer(container_id)),
                ("siteID", PyRep::String(site_id)),
                ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                ("virusStrength", PyRep::Integer(game.base_virus_strength as i64)),
                ("gridSize", PyRep::Integer(game.tiles.len() as i64)),
                ("gridTopology", PyRep::String("Hexagonal".into())),
                ("distanceIndicator", PyRep::Integer(start_indicator as i64)),
                ("status", PyRep::String("InProgress".into())),
            ])))
        }

        // parity: feat:pve.hacking-minigame, feat:pve.loot-spew, feat:pve.ghost-sites, feat:pve.sleeper-caches
        ("hackingMgr", "ClickedOnTile") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot continue hacking minigame while docked".into(),
                }));
            }
            let (container_id, raw_tile_idx) = if args.len() >= 2 {
                (
                    args[0].as_i64().unwrap_or(1_870_000_000),
                    extract_tile_index(args.get(1)).unwrap_or(1),
                )
            } else {
                let Some(cid) = find_active_hacking_container(ctx, char_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: "No active hacking session for character".into(),
                    }));
                };
                let coord = extract_tile_index(args.first()).unwrap_or(1);
                (cid, coord)
            };
            if raw_tile_idx < 0 {
                return Some(Err(BridgeRpcError::user_error("InvalidTileIndex")));
            }
            let tile_idx = raw_tile_idx as u32;
            if let Err(e) = validate_active_hacking_ship_and_range(ctx, session, char_id, container_id).await {
                return Some(Err(e));
            }

            let step_res = with_pve_exploration_reg(ctx, |reg| {
                let _now = now_sec(reg);
                let Some(mut game) = reg.hacking_games.remove(&(char_id, container_id)) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: format!("No active hacking game on container {container_id}"),
                    });
                };
                if session.solar_system_id.is_some_and(|s| s != game.solar_system_id) {
                    reg.hacking_games.insert((char_id, container_id), game);
                    return Err(BridgeRpcError {
                        code: "NotInSiteSystem".into(),
                        message: "Cannot continue hacking minigame from another solar system".into(),
                    });
                }
                if game.status != "InProgress" {
                    reg.hacking_games.insert((char_id, container_id), game);
                    return Err(BridgeRpcError {
                        code: "HackingGameAlreadyFinished".into(),
                        message: format!("Hacking game on container {container_id} is already finished"),
                    });
                }
                if tile_idx >= game.tiles.len() as u32 {
                    reg.hacking_games.insert((char_id, container_id), game);
                    return Err(BridgeRpcError::user_error("InvalidTileIndex"));
                }
                if !game.tiles[tile_idx as usize].revealed {
                    let accessible = game.tiles.iter().any(|adj| {
                        adj.revealed
                            && !is_blocking_defensive_tile(adj)
                            && are_hacking_tiles_adjacent(adj.tile_index, tile_idx)
                    });
                    if !accessible {
                        reg.hacking_games.insert((char_id, container_id), game);
                        return Err(BridgeRpcError::user_error("TileNotAccessible"));
                    }
                }
                let mut timer_expired = false;
                if game.is_ghost_site {
                    timer_expired = crate::services::pve_sleeper_caches::is_ghost_site_timer_expired(
                        reg,
                        game.solar_system_id,
                        &game.site_id,
                    );
                    if timer_expired {
                        game.status = "Failed".into();
                    }
                }

                let (kind_name, rem_coh, eff_str, core_destroyed) =
                    resolve_hacking_turn_combat(&mut game, tile_idx);

                let mut spewed = Vec::new();
                let mut puzzle_solved = false;
                let already_hacked = reg.hacked_containers.contains(&(game.solar_system_id, container_id));
                let revealed_count = game.tiles.iter().filter(|t| t.revealed).count();
                let hack_prog = if core_destroyed {
                    1.0
                } else {
                    (revealed_count as f64 / game.tiles.len().max(1) as f64).clamp(0.0, 1.0)
                };
                crate::services::notify_emitters_batch3_space::emit_local_hack_progress_updated(
                    ctx,
                    game.solar_system_id,
                    container_id,
                    hack_prog,
                );
                if timer_expired || game.virus_coherence <= 0 {
                    game.status = "Failed".into();
                } else if core_destroyed {
                    game.status = "Won".into();
                    if !already_hacked && !game.site_id.starts_with("STRUCTURE_") {
                        let center = reg
                            .sites_by_system
                            .get(&game.solar_system_id)
                            .and_then(|l| l.iter().find(|s| s.site_id == game.site_id))
                            .map(|s| s.position)
                            .unwrap_or([0.0, 0.0, 0.0]);
                        spewed = spawn_loot_spew_for_container(
                            ctx,
                            reg,
                            game.solar_system_id,
                            container_id,
                            game.is_relic,
                            center,
                        );
                        if game.is_sleeper_cache_puzzle {
                            puzzle_solved = true;
                            if let Some(sites) = reg.sites_by_system.get_mut(&game.solar_system_id) {
                                if let Some(s) = sites.iter_mut().find(|x| x.site_id == game.site_id) {
                                    let node_idx = s
                                        .hackable_can_ids
                                        .iter()
                                        .position(|&id| id == container_id)
                                        .unwrap_or((container_id % 10) as usize);
                                    match node_idx {
                                        0 => s.sleeper_rift_stabilized = true,
                                        1 => s.sleeper_sentry_disabled = true,
                                        _ => s.sleeper_gas_cloud_active = false,
                                    }
                                }
                            }
                        }
                    }
                }

                let is_structure = game.site_id.starts_with("STRUCTURE_");
                let mut failed_twice = false;
                if game.status == "Won" {
                    if !is_structure { reg.hacked_containers.insert((game.solar_system_id, container_id)); }
                } else if game.status == "Failed" {
                    if is_structure {
                        let fc = reg.structure_char_failures.entry((game.character_id, container_id)).or_insert(0);
                        *fc += 1;
                    } else {
                        let fc = reg.failure_counts.entry((game.solar_system_id, container_id)).or_insert(0);
                        *fc += 1;
                        if *fc >= 2 || game.is_ghost_site { reg.hacked_containers.insert((game.solar_system_id, container_id)); }
                        if *fc >= 2 && !game.is_ghost_site { failed_twice = true; }
                    }
                }

                let dist_to_core = hacking_tile_distance_to_core(tile_idx, &game.tiles);
                let detonate = game.is_ghost_site && (game.status == "Failed" || timer_expired);
                let out_tuple = (
                    game.solar_system_id, game.site_id.clone(), game.is_relic, game.status.clone(),
                    game.virus_coherence, eff_str, kind_name, rem_coh, dist_to_core, spewed,
                    detonate, timer_expired, failed_twice, puzzle_solved, already_hacked,
                );
                reg.hacking_games.insert((char_id, container_id), game);
                Ok(out_tuple)
            });

            let (
                sys_id, site_id, is_relic, status, virus_coh, eff_strength, tile_kind_str, tile_coh,
                dist_to_core, spewed_cans, trigger_ghost_detonation, ghost_timer_expired,
                trigger_second_fail_explosion, sleeper_puzzle_solved, already_hacked,
            ) = match step_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let (ghost_dmg, ghost_npcs) = if trigger_ghost_detonation {
                let target_can = if ghost_timer_expired { None } else { Some(container_id) };
                detonate_ghost_site_with_can(ctx, session, sys_id, &site_id, target_can).await
            } else {
                (0.0, Vec::new())
            };
            let (can_exploded, can_explosion_dmg, damaged_ships, destroyed_loot_count) = if trigger_second_fail_explosion && !site_id.starts_with("STRUCTURE_") {
                handle_second_hack_failure_explosion(ctx, sys_id, &site_id, container_id, is_relic).await
            } else {
                (false, 0.0, Vec::new(), 0)
            };

            if status == "Won" {
                crate::services::inventory_fitting::fitting_scanners_superweapons::mark_hacking_container_unlocked(
                    ctx, char_id, container_id,
                );
            }
            if status == "Won" && !already_hacked {
                ctx.db.with_mut_snapshot(|s| {
                    let k = format!("hacking_wins:{char_id}");
                    let count: i64 = s.service_state_blobs.get(&k).and_then(|v| v.parse().ok()).unwrap_or(0);
                    s.service_state_blobs.insert(k, (count + 1).to_string());
                });
                crate::services::pve_exploration_sites::materialize_spewed_cans_core(
                    &ctx.db, &ctx.systems, char_id, &spewed_cans,
                ).await;
            }

            let dist_indicator = with_pve_exploration_reg(ctx, |reg| {
                reg.hacking_games
                    .get(&(char_id, container_id))
                    .map(|g| hacking_tile_distance_indicator(tile_idx, &g.tiles))
                    .unwrap_or_else(|| dist_to_core.clamp(1, 5))
            });
            let (q, r) = hacking_tile_axial_coords(tile_idx);
            let dmg_ships_py: Vec<PyRep> = damaged_ships.into_iter().map(PyRep::Integer).collect();
            let turn_events = vec![PyRep::Dict(vec![
                (PyRep::String("eventID".into()), PyRep::Integer(1)),
                (
                    PyRep::String("eventData".into()),
                    PyRep::keyval([
                        ("containerID", PyRep::Integer(container_id)),
                        ("tileIndex", PyRep::Integer(tile_idx as i64)),
                        ("hexQ", PyRep::Integer(q as i64)),
                        ("hexR", PyRep::Integer(r as i64)),
                        ("nodeKind", PyRep::String(tile_kind_str.clone())),
                        ("status", PyRep::String(status.clone())),
                        ("virusCoherence", PyRep::Integer(virus_coh as i64)),
                        ("virusStrength", PyRep::Integer(eff_strength as i64)),
                        ("nodeCoherence", PyRep::Integer(tile_coh as i64)),
                        ("distanceIndicator", PyRep::Integer(dist_indicator as i64)),
                        ("containerExploded", PyRep::Bool(can_exploded)),
                        ("containerDestroyed", PyRep::Bool(can_exploded)),
                        ("explosionDamage", PyRep::Real(can_explosion_dmg)),
                        ("damagedShipIDs", PyRep::List(dmg_ships_py.clone())),
                        ("destroyedLootCount", PyRep::Integer(destroyed_loot_count as i64)),
                    ]),
                ),
            ])];
            session.push_notification("OnHackingTurnComplete", vec![PyRep::List(turn_events)]);
            let spew_py: Vec<PyRep> = spewed_cans
                .iter()
                .map(|c| {
                    PyRep::keyval([
                        ("canItemID", PyRep::Integer(c.can_item_id)),
                        ("lootTypeID", PyRep::Integer(c.loot_type_id as i64)),
                        ("quantity", PyRep::Integer(c.loot_quantity)),
                        ("despawnAt", PyRep::Integer(c.despawn_at_sec as i64)),
                    ])
                })
                .collect();

            let revealed_reinforce = (status == "Won" && site_id.starts_with("STRUCTURE_")).then(|| {
                ctx.db.with_snapshot(|s| {
                    (s.structure_vulnerability_windows.get(&container_id).copied().unwrap_or((18, 5)),
                     crate::services::station_structure::structure_upwell_ops::load_ext_state(s).pending_reinforce_schedule.get(&container_id).copied())
                })
            });
            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)), ("tileIndex", PyRep::Integer(tile_idx as i64)),
                ("hexQ", PyRep::Integer(q as i64)), ("hexR", PyRep::Integer(r as i64)),
                ("nodeKind", PyRep::String(tile_kind_str)), ("nodeCoherence", PyRep::Integer(tile_coh as i64)),
                ("distanceIndicator", PyRep::Integer(dist_indicator as i64)), ("virusCoherence", PyRep::Integer(virus_coh as i64)),
                ("virusStrength", PyRep::Integer(eff_strength as i64)), ("status", PyRep::String(status)),
                ("spewedContainers", PyRep::List(spew_py)), ("containerDestroyed", PyRep::Bool(can_exploded)),
                ("containerExploded", PyRep::Bool(can_exploded)), ("explosionDamage", PyRep::Real(can_explosion_dmg)),
                ("damagedShipIDs", PyRep::List(dmg_ships_py)), ("destroyedLootCount", PyRep::Integer(destroyed_loot_count as i64)),
                ("ghostDetonated", PyRep::Bool(trigger_ghost_detonation)), ("ghostExplosiveDamage", PyRep::Real(ghost_dmg)),
                ("ghostSpawnedNpcBallIDs", PyRep::List(ghost_npcs.into_iter().map(PyRep::Integer).collect())),
                ("sleeperPuzzleSolved", PyRep::Bool(sleeper_puzzle_solved)),
                ("reinforceHour", revealed_reinforce.map_or(PyRep::None, |((h, _), _)| PyRep::Integer(h as i64))),
                ("reinforceWeekday", revealed_reinforce.map_or(PyRep::None, |((_, d), _)| PyRep::Integer(d as i64))),
                ("nextReinforceHour", revealed_reinforce.and_then(|(_, p)| p.map(|(nh, _, _)| PyRep::Integer(nh as i64))).unwrap_or(PyRep::None)),
                ("nextReinforceWeekday", revealed_reinforce.and_then(|(_, p)| p.map(|(_, nd, _)| PyRep::Integer(nd as i64))).unwrap_or(PyRep::None)),
                ("applyAfterUnix", revealed_reinforce.and_then(|(_, p)| p.map(|(_, _, at)| PyRep::Integer(at as i64))).unwrap_or(PyRep::None)),
            ])))
        }

        // parity: feat:pve.hacking-minigame, feat:exploration.hack-failure-container-explosion
        ("hackingMgr", "QuitHackingAttempt") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(container_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| find_active_hacking_container(ctx, char_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveHackingGame".into(),
                    message: "No active hacking game for character".into(),
                }));
            };
            let (removed, second_fail_info, ghost_detonate_info) = with_pve_exploration_reg(ctx, |reg| {
                if let Some(game) = reg.hacking_games.remove(&(char_id, container_id)) {
                    let mut explode_info = None;
                    let mut ghost_info = None;
                    if game.status == "InProgress" {
                        let sid = game.solar_system_id;
                        let fc = reg.failure_counts.entry((sid, container_id)).or_insert(0);
                        *fc += 1;
                        if *fc >= 2 || game.is_ghost_site {
                            reg.hacked_containers.insert((sid, container_id));
                        }
                        if game.is_ghost_site {
                            ghost_info = Some((sid, game.site_id.clone()));
                        } else if *fc >= 2 {
                            explode_info = Some((sid, game.site_id.clone(), game.is_relic));
                        }
                    }
                    (true, explode_info, ghost_info)
                } else {
                    (false, None, None)
                }
            });
            if !removed {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveHackingGame".into(),
                    message: format!("No active hacking game on container {container_id}"),
                }));
            }
            if let Some((sid, site_id)) = ghost_detonate_info {
                let _ = detonate_ghost_site_with_can(ctx, session, sid, &site_id, Some(container_id)).await;
            }
            if let Some((sid, site_id, is_relic)) = second_fail_info {
                let _ = handle_second_hack_failure_explosion(ctx, sid, &site_id, container_id, is_relic).await;
            }
            Some(Ok(PyRep::Bool(true)))
        }

        // parity: rpc:hackingMgr.UsedUtilityElementOnVirus
        ("hackingMgr", "UsedUtilityElementOnVirus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (container_id, slot_idx) = match args {
                [PyRep::Integer(a), PyRep::Integer(b), ..] => {
                    if *a > 1000 { (*a, *b as usize) } else { (*b, *a as usize) }
                }
                [PyRep::Integer(a)] => {
                    let active_cid = with_pve_exploration_reg(ctx, |reg| {
                        reg.hacking_games.keys().find(|(cid, _)| *cid == char_id).map(|(_, cont_id)| *cont_id)
                    });
                    let Some(cid) = active_cid else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetNotFound".into(),
                            message: "Target container not specified".into(),
                        }));
                    };
                    (cid, *a as usize)
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: "Target container not specified".into(),
                    }));
                }
            };
            let target_tile_opt = args.get(2).and_then(|a| a.as_i64());
            if let Some(raw_t) = target_tile_opt {
                if raw_t < 0 {
                    return Some(Err(BridgeRpcError::user_error("InvalidTileIndex")));
                }
            }
            if let Err(e) = validate_active_hacking_ship_and_range(ctx, session, char_id, container_id).await {
                return Some(Err(e));
            }
            let target_tile_u32 = target_tile_opt.map(|t| t as u32);

            let res = with_pve_exploration_reg(ctx, |reg| {
                let Some(game) = reg.hacking_games.get_mut(&(char_id, container_id)) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: format!("No active hacking game on container {container_id}"),
                    });
                };
                if game.status != "InProgress" && game.status != "active" {
                    return Err(BridgeRpcError {
                        code: "HackingGameAlreadyFinished".into(),
                        message: format!("Hacking game on container {container_id} is already finished"),
                    });
                }
                let Some(util) = game.utilities.get(slot_idx).and_then(|u| *u) else {
                    return Err(BridgeRpcError {
                        code: "UtilitySlotEmpty".into(),
                        message: format!("Utility slot {slot_idx} is empty"),
                    });
                };
                if matches!(util, HackingUtilityKind::KernelRot | HackingUtilityKind::SecondaryVector) {
                    if let Some(tid) = target_tile_u32 {
                        let Some(t) = game.tiles.get(tid as usize) else {
                            return Err(BridgeRpcError::user_error("InvalidTileIndex"));
                        };
                        if !t.revealed || t.coherence <= 0 {
                            return Err(BridgeRpcError::user_error("TargetTileNotRevealed"));
                        }
                    } else if !game.tiles.iter().any(|t| t.revealed && t.coherence > 0) {
                        return Err(BridgeRpcError::user_error("TargetTileNotRevealed"));
                    }
                }
                game.utilities[slot_idx] = None;
                match util {
                    HackingUtilityKind::SelfRepair => {
                        game.virus_coherence = (game.virus_coherence + 30).min(game.max_virus_coherence);
                    }
                    HackingUtilityKind::KernelRot => {
                        if let Some(tid) = target_tile_u32 {
                            if let Some(t) = game.tiles.get_mut(tid as usize) {
                                t.coherence = (t.coherence / 2).max(1);
                            }
                        } else if let Some(t) = game.tiles.iter_mut().find(|t| t.revealed && t.coherence > 0) {
                            t.coherence = (t.coherence / 2).max(1);
                        }
                    }
                    HackingUtilityKind::PolymorphicShield => {
                        game.polymorphic_shield_charges += 2;
                    }
                    HackingUtilityKind::SecondaryVector => {
                        let chosen_tile = target_tile_u32
                            .or_else(|| game.tiles.iter().find(|t| t.revealed && t.coherence > 0).map(|t| t.tile_index))
                            .unwrap_or(0);
                        game.secondary_vector_target = Some((chosen_tile, 3));
                    }
                }
                Ok(PyRep::keyval([
                    ("containerID", PyRep::Integer(container_id)),
                    ("usedSlot", PyRep::Integer(slot_idx as i64)),
                    ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                    ("polymorphicShieldCharges", PyRep::Integer(game.polymorphic_shield_charges as i64)),
                ]))
            });
            Some(res)
        }

        ("hackingMgr", "values") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(container_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: "Target container not specified".into(),
                }));
            };
            let tiles = with_pve_exploration_reg(ctx, |reg| {
                reg.hacking_games.get(&(char_id, container_id)).map(|g| {
                    g.tiles.iter().map(|t| {
                        let node_type = if t.revealed { format!("{:?}", t.kind) } else { "Unrevealed".into() };
                        let mut kv = vec![
                            ("tileIndex", PyRep::Integer(t.tile_index as i64)),
                            ("revealed", PyRep::Bool(t.revealed)),
                            ("nodeType", PyRep::String(node_type)),
                            ("coherence", PyRep::Integer(if t.revealed { t.coherence as i64 } else { 0 })),
                            ("strength", PyRep::Integer(if t.revealed { t.strength as i64 } else { 0 })),
                        ];
                        if t.revealed {
                            kv.push(("distanceIndicator", PyRep::Integer(hacking_tile_distance_indicator(t.tile_index, &g.tiles) as i64)));
                        }
                        PyRep::keyval(kv)
                    }).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::List(tiles)))
        }

        // parity: feat:pve.hacking-minigame
        ("hackingMgr", "UsedUtilityElement") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (container_id, slot_idx, raw_tile_idx) = if args.len() >= 3 {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(1_870_000_000),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0).max(0) as usize,
                    extract_tile_index(args.get(2)).unwrap_or(0),
                )
            } else {
                let Some(cid) = find_active_hacking_container(ctx, char_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: "No active hacking session for character".into(),
                    }));
                };
                let slot = args.first().and_then(|a| a.as_i64()).unwrap_or(0).max(0) as usize;
                let tile = extract_tile_index(args.get(1)).unwrap_or(0);
                (cid, slot, tile)
            };
            if raw_tile_idx < 0 {
                return Some(Err(BridgeRpcError::user_error("InvalidTileIndex")));
            }
            let target_tile_idx = raw_tile_idx as usize;
            if let Err(e) = validate_active_hacking_ship_and_range(ctx, session, char_id, container_id).await {
                return Some(Err(e));
            }

            let res = with_pve_exploration_reg(ctx, |reg| {
                let Some(game) = reg.hacking_games.get_mut(&(char_id, container_id)) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: format!("No active hacking game on container {container_id}"),
                    });
                };
                if game.status != "InProgress" {
                    return Err(BridgeRpcError {
                        code: "HackingGameAlreadyFinished".into(),
                        message: format!("Hacking game on container {container_id} is already finished"),
                    });
                }
                let Some(util) = game.utilities.get(slot_idx).and_then(|u| *u) else {
                    return Err(BridgeRpcError {
                        code: "UtilitySlotEmpty".into(),
                        message: format!("Utility slot {slot_idx} is empty"),
                    });
                };
                if matches!(util, HackingUtilityKind::KernelRot | HackingUtilityKind::SecondaryVector) {
                    let Some(t) = game.tiles.get(target_tile_idx) else {
                        return Err(BridgeRpcError::user_error("InvalidTileIndex"));
                    };
                    if !t.revealed || t.coherence <= 0 {
                        return Err(BridgeRpcError::user_error("TargetTileNotRevealed"));
                    }
                }
                game.utilities[slot_idx] = None;
                match util {
                    HackingUtilityKind::SelfRepair => {
                        game.virus_coherence = (game.virus_coherence + 30).min(game.max_virus_coherence);
                    }
                    HackingUtilityKind::KernelRot => {
                        if let Some(t) = game.tiles.get_mut(target_tile_idx) {
                            t.coherence = (t.coherence / 2).max(1);
                        }
                    }
                    HackingUtilityKind::PolymorphicShield => {
                        game.polymorphic_shield_charges += 2;
                    }
                    HackingUtilityKind::SecondaryVector => {
                        game.secondary_vector_target = Some((target_tile_idx as u32, 3));
                    }
                }
                Ok(PyRep::keyval([
                    ("containerID", PyRep::Integer(container_id)),
                    ("usedSlot", PyRep::Integer(slot_idx as i64)),
                    ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                    ("polymorphicShieldCharges", PyRep::Integer(game.polymorphic_shield_charges as i64)),
                ]))
            });
            Some(res)
        }

        ("hackingMgr", "GetMyHackProgress") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = with_pve_exploration_reg(ctx, |reg| {
                if let Some(game) = reg.hacking_games.get(&(char_id, container_id)) {
                    PyRep::keyval([
                        ("containerID", PyRep::Integer(container_id)),
                        ("status", PyRep::String(game.status.clone())),
                        ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                    ])
                } else {
                    PyRep::keyval([
                        ("containerID", PyRep::Integer(container_id)),
                        ("status", PyRep::String("NotStarted".into())),
                        ("virusCoherence", PyRep::Integer(0)),
                    ])
                }
            });
            Some(Ok(res))
        }

        _ => None,
    }
}
