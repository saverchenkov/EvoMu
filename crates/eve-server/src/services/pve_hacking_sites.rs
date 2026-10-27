//! `pve_hacking_sites` — Data Sites, Relic Sites, Node-Grid Hacking Minigame (`hackingMgr`),
//! Loot Spew Mini-Containers, Ghost Site Hidden Timer & Detonation, and Sleeper Cache Rift Puzzles.
//!
// parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.hacking-minigame
// parity: feat:pve.loot-spew, feat:pve.ghost-sites, feat:pve.sleeper-caches

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    build_wave_npc_balls, ensure_system_pve_sites_seeded, next_pve_seq, now_sec,
    with_pve_exploration_reg, HackingGameSession, HackingNodeKind, HackingTileState,
    HackingUtilityKind, PveExplorationRegistry, SpewedLootCan,
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
    t.coherence > 0
        && matches!(
            t.kind,
            HackingNodeKind::Firewall
                | HackingNodeKind::Antivirus
                | HackingNodeKind::VirusSuppressor
                | HackingNodeKind::RestorationNode
        )
}

/// Resolves a fitted Data/Relic/Integrated Analyzer module (SDE group 538 or dogma `virusCoherence` attr 1909)
/// on the character's active ship and computes `(virus_coherence, virus_strength, utility_slots)` from SDE dogma
/// attributes (`1909 virusCoherence`, `1910 virusStrength`, `1911 virusElementSlots`) plus Hacking (`21718`) /
/// Archaeology (`13278`) skill bonus (`+10` virus coherence per level).
pub fn resolve_fitted_analyzer_virus_stats(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    is_relic: bool,
) -> Result<(i32, i32, usize, f64), BridgeRpcError> {
    let ship_id = session
        .ship_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id)))
        .ok_or_else(|| BridgeRpcError::user_error("AnalyzerModuleRequired"))?;

    let (mod_type_id, skill_level) = ctx.db.with_snapshot(|snap| {
        let fitted_analyzers: Vec<u32> = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && (11..=34).contains(&(i.flag as u16)))
            .filter_map(|i| {
                let t = ctx.sde.get_type(i.type_id)?;
                if t.group_id == 538 || t.default_attributes.get(&1909).copied().unwrap_or(0.0) > 0.0 {
                    Some(i.type_id)
                } else {
                    None
                }
            })
            .collect();

        let matching = fitted_analyzers.into_iter().find(|&tid| {
            let Some(t) = ctx.sde.get_type(tid) else { return false };
            let integrated = (t.default_attributes.contains_key(&182) && t.default_attributes.contains_key(&183))
                || (t.default_attributes.contains_key(&1330) && t.default_attributes.contains_key(&1331))
                || t.name.contains("Integrated")
                || t.name.contains("QA");
            if integrated {
                return true;
            }
            let relic_mod = t.default_attributes.get(&183) == Some(&13278.0)
                || t.default_attributes.contains_key(&1331)
                || t.name.contains("Relic");
            if is_relic {
                relic_mod
            } else {
                !relic_mod
            }
        });

        let lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| {
                if is_relic {
                    m.get(&13278)
                } else {
                    m.get(&21718)
                }
            })
            .map(|s| s.level as i32)
            .unwrap_or(0);
        (matching, lvl)
    });

    let Some(mod_tid) = mod_type_id else {
        return Err(BridgeRpcError::user_error("AnalyzerModuleRequired"));
    };
    let inv_type = ctx.sde.get_type(mod_tid);
    let base_coh = inv_type
        .and_then(|t| t.default_attributes.get(&1909).copied())
        .unwrap_or(40.0) as i32;
    let base_str = inv_type
        .and_then(|t| t.default_attributes.get(&1910).copied())
        .unwrap_or(20.0) as i32;
    let slots = inv_type
        .and_then(|t| t.default_attributes.get(&1911).copied())
        .unwrap_or(3.0)
        .max(1.0) as usize;
    let max_range = inv_type
        .and_then(|t| t.default_attributes.get(&54).copied())
        .unwrap_or(5_000.0)
        .max(1_000.0);
    Ok((base_coh + skill_level * 10, base_str, slots, max_range))
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

pub fn hacking_tile_distance_indicator(tile_idx: u32, tiles: &[HackingTileState]) -> u32 {
    tiles
        .iter()
        .filter(|t| {
            !t.revealed
                && matches!(
                    t.kind,
                    HackingNodeKind::SystemCore | HackingNodeKind::DataCache
                )
        })
        .map(|t| hacking_hex_distance(tile_idx, t.tile_index))
        .min()
        .unwrap_or_else(|| hacking_tile_distance_to_core(tile_idx, tiles))
        .clamp(1, 5)
}

// parity: feat:exploration.hacking-node-network, feat:exploration.hacking-coherence-and-strength
pub fn build_hacking_grid(container_id: ItemId, is_relic: bool) -> Vec<HackingTileState> {
    build_hacking_grid_procedural(container_id, is_relic, 2, 0)
}

// parity: feat:exploration.hacking-node-network, feat:exploration.hacking-coherence-and-strength
pub fn build_hacking_grid_procedural(
    container_id: ItemId,
    is_relic: bool,
    difficulty: u8,
    rng_seed: u64,
) -> Vec<HackingTileState> {
    let diff = difficulty.clamp(1, 5);
    let mut state = rng_seed
        ^ (container_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ ((diff as u64) << 32)
        ^ (if is_relic { 0xA5A5_5A5A_F0F0_0F0F } else { 0 });
    let mut next_u64 = || -> u64 {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let core_candidates: Vec<u32> = (1..24_u32)
        .filter(|&idx| hacking_hex_distance(0, idx) >= 3)
        .collect();
    let core_idx = core_candidates[(next_u64() as usize) % core_candidates.len()];
    let core_coh = 20 + (diff as i32) * 10 + if is_relic { 10 } else { 0 };
    let core_str = 10 + ((diff as i32 - 1) * 2);

    let mut pool: Vec<(HackingNodeKind, i32, i32, Option<HackingUtilityKind>)> = Vec::with_capacity(22);
    for _ in 0..(1 + (diff as usize) / 3) {
        pool.push((HackingNodeKind::Firewall, 40, 20, None));
    }
    for _ in 0..(1 + (diff as usize) / 4) {
        pool.push((HackingNodeKind::Antivirus, 35, 15, None));
    }
    pool.push((HackingNodeKind::RestorationNode, 20, 10, None));
    pool.push((HackingNodeKind::VirusSuppressor, 20, 10, None));
    pool.push((HackingNodeKind::DataCache, 0, 0, Some(HackingUtilityKind::PolymorphicShield)));
    pool.push((HackingNodeKind::DataCache, 0, 0, Some(HackingUtilityKind::SelfRepair)));
    pool.push((HackingNodeKind::DataCache, 0, 0, Some(HackingUtilityKind::SecondaryVector)));
    for _ in 0..(1 + (diff as usize) / 2) {
        pool.push((HackingNodeKind::DataCache, 0, 0, Some(HackingUtilityKind::KernelRot)));
    }
    while pool.len() < 22 {
        pool.push((HackingNodeKind::Empty, 0, 0, None));
    }
    pool.truncate(22);
    for i in (1..pool.len()).rev() {
        let j = (next_u64() as usize) % (i + 1);
        pool.swap(i, j);
    }

    let mut pool_iter = pool.into_iter();
    let mut tiles = Vec::with_capacity(24);
    for idx in 0..24_u32 {
        let (kind, coh, strg, util) = if idx == 0 {
            (HackingNodeKind::Empty, 0, 0, None)
        } else if idx == core_idx {
            (HackingNodeKind::SystemCore, core_coh, core_str, None)
        } else {
            pool_iter.next().unwrap_or((HackingNodeKind::Empty, 0, 0, None))
        };
        tiles.push(HackingTileState {
            tile_index: idx,
            revealed: idx == 0,
            blocked_by_defensive: false,
            kind,
            coherence: coh,
            strength: strg,
            utility_drop: util,
        });
    }
    tiles
}

/// Self-destructs a relic/data container after failing a hack attempt twice (`failure_counts >= 2`).
/// In low-sec (`sec < 0.45`), null-sec (`sec <= 0.0`), or wormhole space (`31_000_000..32_000_000`),
/// destroys all contained loot and inflicts 2,000-5,000 HP AoE explosive damage to any ship within 5,000m.
// parity: feat:exploration.hack-failure-container-explosion
pub async fn handle_second_hack_failure_explosion(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    site_id: &str,
    container_id: ItemId,
    is_relic: bool,
) -> (bool, f64, Vec<i64>, usize) {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let is_dangerous_space =
        (31_000_000..32_000_000).contains(&solar_system_id) || sys_sec < 0.45;
    if !is_dangerous_space {
        return (false, 0.0, Vec::new(), 0);
    }

    let site_pos = with_pve_exploration_reg(ctx, |reg| {
        let p = reg
            .sites_by_system
            .get(&solar_system_id)
            .and_then(|l| l.iter().find(|s| s.site_id == site_id))
            .map(|s| s.position)
            .unwrap_or([0.0, 0.0, 0.0]);
        reg.spewed_cans
            .retain(|_, c| c.parent_container_id != container_id);
        p
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let can_pos = if let Ok(Some((_, cb))) = sys_handle.get_ball_snapshot(container_id).await {
        cb.position
    } else if let Some(p) = ctx.db.with_snapshot(|s| s.item_positions.get(&container_id).copied()) {
        glam::DVec3::from_array(p)
    } else {
        glam::DVec3::from_array(site_pos)
    };
    let _ = sys_handle.remove_ball(container_id).await;

    let _ = is_relic;
    let destroyed_loot_count = ctx.db.with_mut_snapshot(|snap| {
        let before = snap.items.len();
        snap.items
            .retain(|&id, i| id != container_id && i.location_id != container_id);
        snap.item_positions.remove(&container_id);
        before.saturating_sub(snap.items.len())
    });

    let blast_damage = 2_000.0 + 3_000.0 * ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5);
    let dmg_vec = eve_destiny::DamageVector {
        explosive: blast_damage,
        ..Default::default()
    };
    let mut damaged_ship_ids = Vec::new();
    if let Ok(balls) = sys_handle.list_balls().await {
        for b in balls {
            let is_ship = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&b.id)
                    .and_then(|i| ctx.sde.types.get(&i.type_id))
                    .is_some_and(|t| t.category_id == 6)
            }) || ctx
                .sde
                .types
                .get(&b.type_id)
                .is_some_and(|t| t.category_id == 6);
            if is_ship && b.position.distance(can_pos) <= 5_000.0 {
                if sys_handle.apply_damage(b.id, dmg_vec).await.is_ok() {
                    damaged_ship_ids.push(b.id);
                }
            }
        }
    }
    damaged_ship_ids.sort_unstable();
    (true, blast_damage, damaged_ship_ids, destroyed_loot_count)
}

/// Trigger Ghost Site can detonation: applies AoE explosive damage to the pilot's ship and spawns Pirate response NPCs.
// parity: feat:pve.ghost-sites
pub async fn detonate_ghost_site(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> (f64, Vec<i64>) {
    let mut pos = [52_000_000.0, 42_000_000.0, -18_000_000.0];
    let mut dungeon_id = 5534_u32;
    let mut already_detonated = false;
    with_pve_exploration_reg(ctx, |reg| {
        if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
            if let Some(site) = sites.iter_mut().find(|s| s.site_id == site_id) {
                pos = site.position;
                dungeon_id = site.dungeon_id;
                already_detonated = site.ghost_detonated;
                site.ghost_detonated = true;
                site.npc_guards_spawned = true;
            }
        }
    });

    let explosive_damage = 6_500.0_f64;
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);

    if let Some(ship_id) = session.ship_id {
        let dmg_vec = eve_destiny::DamageVector {
            explosive: explosive_damage,
            ..Default::default()
        };
        let _ = sys_handle.apply_damage(ship_id, dmg_vec).await;
    }

    let mut ambush_ids = Vec::new();
    if !already_detonated {
        let ambush_balls = build_wave_npc_balls(&ctx.sde, solar_system_id, dungeon_id, 2, pos, None);
        for b in ambush_balls {
            ambush_ids.push(b.id);
            let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(&ctx.sde, b.type_id);
            let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
        }
        with_pve_exploration_reg(ctx, |reg| {
            if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
                if let Some(site) = sites.iter_mut().find(|s| s.site_id == site_id) {
                    site.active_npc_ball_ids.extend(ambush_ids.iter().copied());
                }
            }
        });
    }
    (explosive_damage, ambush_ids)
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
                return Some(Err(BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: "Target container not specified".into(),
                }));
            }

            let site_res = with_pve_exploration_reg(ctx, |reg| {
                let sites = reg.sites_by_system.get_mut(&sys_id).ok_or_else(|| BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: "Target container not found".into(),
                })?;

                let s = sites
                    .iter_mut()
                    .find(|x| x.hackable_can_ids.contains(&container_id) && !x.cleared)
                    .ok_or_else(|| BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: "Target container not found".into(),
                    })?;

                if reg.hacked_containers.contains(&(sys_id, container_id))
                    || reg.failure_counts.get(&(sys_id, container_id)).copied().unwrap_or(0) >= 2
                {
                    return Err(BridgeRpcError {
                        code: "HackingGameAlreadyFinished".into(),
                        message: "Container has already been hacked or emptied".into(),
                    });
                }

                if reg
                    .hacking_games
                    .values()
                    .any(|g| g.container_id == container_id && g.status == "InProgress")
                {
                    return Err(BridgeRpcError {
                        code: "HackingGameAlreadyInProgress".into(),
                        message: "Another pilot is already hacking this container".into(),
                    });
                }

                let is_ghost = s.site_category == "Ghost";
                let is_relic = s.site_category == "Relic";
                let is_sleeper_cache = s.site_category == "SleeperCache";
                let site_id = s.site_id.clone();
                let site_pos = s.position;
                Ok((site_id, is_relic, is_ghost, is_sleeper_cache, site_pos))
            });

            let (site_id, is_relic, is_ghost, is_sleeper_cache, site_pos) = match site_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            // BF-PVE-HACK-PROXIMITY-FAILOPEN-04: Explicitly reject hacking while docked or without a ship in space
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot initiate hacking minigame while docked".into(),
                }));
            }
            let ship_id = match session.ship_id {
                Some(sid) if sid > 0 => sid,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "Session has no active ship".into(),
                    }));
                }
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

            let difficulty = args
                .get(4)
                .and_then(|a| a.as_i64())
                .map(|d| (d as u8).clamp(1, 5))
                .unwrap_or(if (31_000_000..32_000_000).contains(&sys_id) { 3 } else { 2 });
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
                if let Some((sv_idx, rem_turns)) = game.secondary_vector_target {
                    if let Some(sv_tile) = game.tiles.get_mut(sv_idx as usize) {
                        if sv_tile.revealed && sv_tile.coherence > 0 {
                            sv_tile.coherence = (sv_tile.coherence - 20).max(0);
                        }
                    }
                    game.secondary_vector_target = if rem_turns > 1 {
                        Some((sv_idx, rem_turns - 1))
                    } else {
                        None
                    };
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

                let suppressor_active = game.tiles.iter().any(|t| {
                    t.revealed && t.kind == HackingNodeKind::VirusSuppressor && t.coherence > 0
                });
                let eff_str = if suppressor_active {
                    (game.base_virus_strength - 15).max(10)
                } else {
                    game.base_virus_strength
                };

                let restorer_active = game.tiles.iter().any(|t| {
                    t.revealed && t.kind == HackingNodeKind::RestorationNode && t.coherence > 0
                });
                if restorer_active {
                    for t in &mut game.tiles {
                        if t.revealed
                            && t.coherence > 0
                            && matches!(
                                t.kind,
                                HackingNodeKind::Firewall
                                    | HackingNodeKind::Antivirus
                                    | HackingNodeKind::SystemCore
                            )
                        {
                            t.coherence += 10;
                        }
                    }
                }

                let t_ref = &mut game.tiles[tile_idx as usize];
                t_ref.revealed = true;
                if let Some(u) = t_ref.utility_drop.take() {
                    if let Some(empty_slot) = game.utilities.iter_mut().find(|s| s.is_none()) {
                        *empty_slot = Some(u);
                    } else {
                        game.utilities.push(Some(u));
                    }
                }
                if t_ref.coherence > 0 {
                    t_ref.coherence = (t_ref.coherence - eff_str).max(0);
                    if t_ref.coherence > 0 {
                        if game.polymorphic_shield_charges > 0 {
                            game.polymorphic_shield_charges -= 1;
                        } else {
                            game.virus_coherence = (game.virus_coherence - t_ref.strength).max(0);
                        }
                    }
                }
                let kind_name = format!("{:?}", t_ref.kind);
                let rem_coh = t_ref.coherence;
                let core_destroyed =
                    t_ref.kind == HackingNodeKind::SystemCore && t_ref.coherence <= 0;

                let mut spewed = Vec::new();
                let mut puzzle_solved = false;
                let already_hacked = reg.hacked_containers.contains(&(game.solar_system_id, container_id));
                if timer_expired || game.virus_coherence <= 0 {
                    game.status = "Failed".into();
                } else if core_destroyed {
                    game.status = "Won".into();
                    if !already_hacked {
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

                let mut failed_twice = false;
                if game.status == "Won" {
                    reg.hacked_containers.insert((game.solar_system_id, container_id));
                } else if game.status == "Failed" {
                    let fc = reg.failure_counts.entry((game.solar_system_id, container_id)).or_insert(0);
                    *fc += 1;
                    if *fc >= 2 || game.is_ghost_site {
                        reg.hacked_containers.insert((game.solar_system_id, container_id));
                    }
                    if *fc >= 2 && !game.is_ghost_site {
                        failed_twice = true;
                    }
                }

                let dist_to_core = hacking_tile_distance_to_core(tile_idx, &game.tiles);
                let detonate = game.is_ghost_site && (game.status == "Failed" || timer_expired);
                let out_tuple = (
                    game.solar_system_id,
                    game.site_id.clone(),
                    game.is_relic,
                    game.status.clone(),
                    game.virus_coherence,
                    eff_str,
                    kind_name,
                    rem_coh,
                    dist_to_core,
                    spewed,
                    detonate,
                    failed_twice,
                    puzzle_solved,
                    already_hacked,
                );
                reg.hacking_games.insert((char_id, container_id), game);
                Ok(out_tuple)
            });

            let (
                sys_id,
                site_id,
                is_relic,
                status,
                virus_coh,
                eff_strength,
                tile_kind_str,
                tile_coh,
                dist_to_core,
                spewed_cans,
                trigger_ghost_detonation,
                trigger_second_fail_explosion,
                sleeper_puzzle_solved,
                already_hacked,
            ) = match step_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let mut ghost_dmg = 0.0;
            let mut ghost_npcs = Vec::new();
            if trigger_ghost_detonation {
                let (dmg, npcs) = detonate_ghost_site(ctx, session, sys_id, &site_id).await;
                ghost_dmg = dmg;
                ghost_npcs = npcs;
            }
            let (can_exploded, can_explosion_dmg, damaged_ships, destroyed_loot_count) =
                if trigger_second_fail_explosion {
                    handle_second_hack_failure_explosion(ctx, sys_id, &site_id, container_id, is_relic)
                        .await
                } else {
                    (false, 0.0, Vec::new(), 0)
                };

            if status == "Won" {
                crate::services::inventory_fitting::fitting_scanners_superweapons::mark_hacking_container_unlocked(
                    ctx,
                    char_id,
                    container_id,
                );
            }
            if status == "Won" && !already_hacked {
                ctx.db.with_mut_snapshot(|s| {
                    let k = format!("hacking_wins:{char_id}");
                    let count: i64 = s.service_state_blobs.get(&k).and_then(|v| v.parse().ok()).unwrap_or(0);
                    s.service_state_blobs.insert(k, (count + 1).to_string());
                });
                // Loot exists only in the spewed cans (real in-space containers); the parent container stays empty.
                crate::services::pve_exploration_sites::materialize_spewed_cans_core(
                    &ctx.db,
                    &ctx.systems,
                    char_id,
                    &spewed_cans,
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

            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)),
                ("tileIndex", PyRep::Integer(tile_idx as i64)),
                ("hexQ", PyRep::Integer(q as i64)),
                ("hexR", PyRep::Integer(r as i64)),
                ("nodeKind", PyRep::String(tile_kind_str)),
                ("nodeCoherence", PyRep::Integer(tile_coh as i64)),
                ("distanceIndicator", PyRep::Integer(dist_indicator as i64)),
                ("virusCoherence", PyRep::Integer(virus_coh as i64)),
                ("virusStrength", PyRep::Integer(eff_strength as i64)),
                ("status", PyRep::String(status)),
                ("spewedContainers", PyRep::List(spew_py)),
                ("containerDestroyed", PyRep::Bool(can_exploded)),
                ("containerExploded", PyRep::Bool(can_exploded)),
                ("explosionDamage", PyRep::Real(can_explosion_dmg)),
                ("damagedShipIDs", PyRep::List(dmg_ships_py)),
                ("destroyedLootCount", PyRep::Integer(destroyed_loot_count as i64)),
                ("ghostDetonated", PyRep::Bool(trigger_ghost_detonation)),
                ("ghostExplosiveDamage", PyRep::Real(ghost_dmg)),
                ("ghostSpawnedNpcBallIDs", PyRep::List(ghost_npcs.into_iter().map(PyRep::Integer).collect())),
                ("sleeperPuzzleSolved", PyRep::Bool(sleeper_puzzle_solved)),
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
            let (removed, second_fail_info) = with_pve_exploration_reg(ctx, |reg| {
                if let Some(game) = reg.hacking_games.remove(&(char_id, container_id)) {
                    let mut explode_info = None;
                    if game.status == "InProgress" {
                        let sid = game.solar_system_id;
                        let fc = reg.failure_counts.entry((sid, container_id)).or_insert(0);
                        *fc += 1;
                        if *fc >= 2 || game.is_ghost_site {
                            reg.hacked_containers.insert((sid, container_id));
                        }
                        if *fc >= 2 && !game.is_ghost_site {
                            explode_info = Some((sid, game.site_id.clone(), game.is_relic));
                        }
                    }
                    (true, explode_info)
                } else {
                    (false, None)
                }
            });
            if !removed {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveHackingGame".into(),
                    message: format!("No active hacking game on container {container_id}"),
                }));
            }
            if let Some((sid, site_id, is_relic)) = second_fail_info {
                let _ = handle_second_hack_failure_explosion(ctx, sid, &site_id, container_id, is_relic)
                    .await;
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
                [PyRep::Integer(a), PyRep::Integer(b)] => {
                    if *a > 1000 { (*a, *b as usize) } else { (*b, *a as usize) }
                }
                [PyRep::Integer(a)] => {
                    let active_cid = with_pve_exploration_reg(ctx, |reg| {
                        reg.hacking_games
                            .keys()
                            .find(|(cid, _)| *cid == char_id)
                            .map(|(_, cont_id)| *cont_id)
                    });
                    match active_cid {
                        Some(cid) => (cid, *a as usize),
                        None => {
                            return Some(Err(BridgeRpcError {
                                code: "TargetNotFound".into(),
                                message: "Target container not specified".into(),
                            }));
                        }
                    }
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: "Target container not specified".into(),
                    }));
                }
            };

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
                let Some(util) = game.utilities.get_mut(slot_idx).and_then(|u| u.take()) else {
                    return Err(BridgeRpcError {
                        code: "UtilitySlotEmpty".into(),
                        message: format!("Utility slot {slot_idx} is empty"),
                    });
                };
                match util {
                    HackingUtilityKind::SelfRepair => {
                        game.virus_coherence =
                            (game.virus_coherence + 30).min(game.max_virus_coherence);
                    }
                    HackingUtilityKind::KernelRot => {
                        if let Some(t) = game.tiles.iter_mut().find(|t| t.revealed && t.coherence > 0) {
                            t.coherence = (t.coherence / 2).max(1);
                        }
                    }
                    HackingUtilityKind::PolymorphicShield => {
                        game.polymorphic_shield_charges += 2;
                    }
                    HackingUtilityKind::SecondaryVector => {
                        game.secondary_vector_target = Some((0, 3));
                    }
                }
                Ok(PyRep::keyval([
                    ("containerID", PyRep::Integer(container_id)),
                    ("usedSlot", PyRep::Integer(slot_idx as i64)),
                    ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                    (
                        "polymorphicShieldCharges",
                        PyRep::Integer(game.polymorphic_shield_charges as i64),
                    ),
                ]))
            });
            Some(res)
        }

        ("hackingMgr", "values") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) => id,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: "Target container not specified".into(),
                    }));
                }
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
            let (container_id, slot_idx, target_tile_idx) = if args.len() >= 3 {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(1_870_000_000),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0).max(0) as usize,
                    extract_tile_index(args.get(2)).unwrap_or(0).max(0) as usize,
                )
            } else {
                let Some(cid) = find_active_hacking_container(ctx, char_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: "No active hacking session for character".into(),
                    }));
                };
                let slot = args.first().and_then(|a| a.as_i64()).unwrap_or(0).max(0) as usize;
                let tile = extract_tile_index(args.get(1)).unwrap_or(0).max(0) as usize;
                (cid, slot, tile)
            };

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
                let Some(util) = game.utilities.get_mut(slot_idx).and_then(|u| u.take()) else {
                    return Err(BridgeRpcError {
                        code: "UtilitySlotEmpty".into(),
                        message: format!("Utility slot {slot_idx} is empty"),
                    });
                };
                match util {
                    HackingUtilityKind::SelfRepair => {
                        game.virus_coherence =
                            (game.virus_coherence + 30).min(game.max_virus_coherence);
                    }
                    HackingUtilityKind::KernelRot => {
                        let Some(t) = game.tiles.get_mut(target_tile_idx) else {
                            game.utilities[slot_idx] = Some(util);
                            return Err(BridgeRpcError::user_error("InvalidTileIndex"));
                        };
                        if !t.revealed || t.coherence <= 0 {
                            game.utilities[slot_idx] = Some(util);
                            return Err(BridgeRpcError::user_error("TargetTileNotRevealed"));
                        }
                        t.coherence = (t.coherence / 2).max(1);
                    }
                    HackingUtilityKind::PolymorphicShield => {
                        game.polymorphic_shield_charges += 2;
                    }
                    HackingUtilityKind::SecondaryVector => {
                        let Some(t) = game.tiles.get(target_tile_idx) else {
                            game.utilities[slot_idx] = Some(util);
                            return Err(BridgeRpcError::user_error("InvalidTileIndex"));
                        };
                        if !t.revealed || t.coherence <= 0 {
                            game.utilities[slot_idx] = Some(util);
                            return Err(BridgeRpcError::user_error("TargetTileNotRevealed"));
                        }
                        game.secondary_vector_target = Some((target_tile_idx as u32, 3));
                    }
                }
                Ok(PyRep::keyval([
                    ("containerID", PyRep::Integer(container_id)),
                    ("usedSlot", PyRep::Integer(slot_idx as i64)),
                    ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                    (
                        "polymorphicShieldCharges",
                        PyRep::Integer(game.polymorphic_shield_charges as i64),
                    ),
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
