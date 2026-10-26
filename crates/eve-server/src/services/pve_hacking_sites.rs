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

pub fn build_hacking_grid(container_id: ItemId, is_relic: bool) -> Vec<HackingTileState> {
    let seed = (container_id as u64).wrapping_mul(6364136223846793005);
    let core_idx = (10 + (seed % 14)) as u32;
    let mut tiles = Vec::with_capacity(24);
    for idx in 0..24_u32 {
        let (kind, coh, strg, util) = match idx {
            0 => (HackingNodeKind::Empty, 0, 0, None),
            2 => (HackingNodeKind::DataCache, 20, 10, Some(HackingUtilityKind::SelfRepair)),
            3 => (HackingNodeKind::Firewall, 60, 20, None),
            5 => (HackingNodeKind::Antivirus, 50, 30, None),
            7 => (HackingNodeKind::RestorationNode, 40, 15, None),
            9 => (HackingNodeKind::VirusSuppressor, 40, 15, None),
            i if i == core_idx => (HackingNodeKind::SystemCore, if is_relic { 70 } else { 60 }, 25, None),
            i if i % 6 == 4 => (HackingNodeKind::DataCache, 20, 10, Some(HackingUtilityKind::KernelRot)),
            _ => (HackingNodeKind::Empty, 0, 0, None),
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

/// Trigger Ghost Site can detonation: applies AoE explosive damage to the pilot's ship and spawns Pirate response NPCs.
// parity: feat:pve.ghost-sites
pub async fn detonate_ghost_site(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> (f64, Vec<i64>) {
    let mut pos = [52_000_000.0, 42_000_000.0, -18_000_000.0];
    let mut already_detonated = false;
    with_pve_exploration_reg(ctx, |reg| {
        if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
            if let Some(site) = sites.iter_mut().find(|s| s.site_id == site_id) {
                pos = site.position;
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
        let ambush_balls = build_wave_npc_balls(&ctx.sde, solar_system_id, 5534, 2, pos, None);
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
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let container_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let site_res = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
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
                let dur = s.ghost_detonate_after_sec.max(90);
                if is_ghost && s.ghost_timer_started_at.is_none() {
                    s.ghost_timer_started_at = Some(now);
                    s.ghost_detonate_after_sec = dur;
                }
                let expired = is_ghost
                    && s.ghost_timer_started_at.is_some_and(|st| now >= st + s.ghost_detonate_after_sec);
                Ok((
                    site_id,
                    is_relic,
                    is_ghost,
                    is_sleeper_cache,
                    expired,
                ))
            });

            let (site_id, is_relic, is_ghost, is_sleeper_cache, ghost_expired) = match site_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

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

            let game = with_pve_exploration_reg(ctx, |reg| {
                let game_id = 500_000 + next_pve_seq(reg);
                let tiles = build_hacking_grid(container_id, is_relic);
                let g = HackingGameSession {
                    game_id,
                    character_id: char_id,
                    solar_system_id: sys_id,
                    container_id,
                    site_id: site_id.clone(),
                    is_relic,
                    is_ghost_site: is_ghost,
                    is_sleeper_cache_puzzle: is_sleeper_cache,
                    virus_coherence: 85,
                    max_virus_coherence: 85,
                    base_virus_strength: 30,
                    polymorphic_shield_charges: 0,
                    secondary_vector_target: None,
                    utilities: vec![
                        Some(HackingUtilityKind::SelfRepair),
                        Some(HackingUtilityKind::KernelRot),
                        Some(HackingUtilityKind::PolymorphicShield),
                    ],
                    tiles,
                    status: "InProgress".into(),
                };
                reg.hacking_games.insert((char_id, container_id), g.clone());
                g
            });

            Some(Ok(PyRep::keyval([
                ("gameID", PyRep::Integer(game.game_id)),
                ("containerID", PyRep::Integer(container_id)),
                ("siteID", PyRep::String(site_id)),
                ("virusCoherence", PyRep::Integer(game.virus_coherence as i64)),
                ("virusStrength", PyRep::Integer(game.base_virus_strength as i64)),
                ("gridSize", PyRep::Integer(game.tiles.len() as i64)),
                ("status", PyRep::String("InProgress".into())),
            ])))
        }

        // parity: feat:pve.hacking-minigame, feat:pve.loot-spew, feat:pve.ghost-sites, feat:pve.sleeper-caches
        ("hackingMgr", "ClickedOnTile") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1_870_000_000);
            let tile_idx = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).clamp(0, 23) as u32;

            let step_res = with_pve_exploration_reg(ctx, |reg| {
                let _now = now_sec(reg);
                let Some(mut game) = reg.hacking_games.remove(&(char_id, container_id)) else {
                    return Err(BridgeRpcError {
                        code: "NoActiveHackingGame".into(),
                        message: format!("No active hacking game on container {container_id}"),
                    });
                };
                if game.status != "InProgress" {
                    reg.hacking_games.insert((char_id, container_id), game);
                    return Err(BridgeRpcError {
                        code: "HackingGameAlreadyFinished".into(),
                        message: format!("Hacking game on container {container_id} is already finished"),
                    });
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
                    game.utilities.push(Some(u));
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
                                    let node_idx = container_id % 10;
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

                if game.status == "Won" {
                    reg.hacked_containers.insert((game.solar_system_id, container_id));
                } else if game.status == "Failed" {
                    let fc = reg.failure_counts.entry((game.solar_system_id, container_id)).or_insert(0);
                    *fc += 1;
                    if *fc >= 2 || game.is_ghost_site {
                        reg.hacked_containers.insert((game.solar_system_id, container_id));
                    }
                }

                let detonate = game.is_ghost_site && (game.status == "Failed" || timer_expired);
                let out_tuple = (
                    game.solar_system_id,
                    game.site_id.clone(),
                    game.status.clone(),
                    game.virus_coherence,
                    eff_str,
                    kind_name,
                    rem_coh,
                    spewed,
                    detonate,
                    puzzle_solved,
                    already_hacked,
                );
                reg.hacking_games.insert((char_id, container_id), game);
                Ok(out_tuple)
            });

            let (
                sys_id,
                site_id,
                status,
                virus_coh,
                eff_strength,
                tile_kind_str,
                tile_coh,
                spewed_cans,
                trigger_ghost_detonation,
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

            session.push_notification(
                "OnHackingTurnComplete",
                vec![
                    PyRep::Integer(container_id),
                    PyRep::Integer(tile_idx as i64),
                    PyRep::String(status.clone()),
                    PyRep::Integer(virus_coh as i64),
                ],
            );

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
                ("nodeKind", PyRep::String(tile_kind_str)),
                ("nodeCoherence", PyRep::Integer(tile_coh as i64)),
                ("virusCoherence", PyRep::Integer(virus_coh as i64)),
                ("virusStrength", PyRep::Integer(eff_strength as i64)),
                ("status", PyRep::String(status)),
                ("spewedContainers", PyRep::List(spew_py)),
                ("ghostDetonated", PyRep::Bool(trigger_ghost_detonation)),
                ("ghostExplosiveDamage", PyRep::Real(ghost_dmg)),
                (
                    "ghostSpawnedNpcBallIDs",
                    PyRep::List(ghost_npcs.into_iter().map(PyRep::Integer).collect()),
                ),
                ("sleeperPuzzleSolved", PyRep::Bool(sleeper_puzzle_solved)),
            ])))
        }

        // parity: feat:pve.hacking-minigame
        ("hackingMgr", "QuitHackingAttempt") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(container_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "QuitHackingAttempt requires a container_id argument".into(),
                }));
            };
            let removed = with_pve_exploration_reg(ctx, |reg| {
                if let Some(game) = reg.hacking_games.remove(&(char_id, container_id)) {
                    if game.status == "InProgress" {
                        let sid = game.solar_system_id;
                        let fc = reg.failure_counts.entry((sid, container_id)).or_insert(0);
                        *fc += 1;
                        if *fc >= 2 || game.is_ghost_site {
                            reg.hacked_containers.insert((sid, container_id));
                        }
                    }
                    true
                } else {
                    false
                }
            });
            if !removed {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveHackingGame".into(),
                    message: format!("No active hacking game on container {container_id}"),
                }));
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("hackingMgr", "UsedUtilityElementOnVirus") => Some(Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: "hackingMgr.UsedUtilityElementOnVirus is not implemented".into(),
        })),

        ("hackingMgr", "values") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1_870_000_000);
            let tiles = with_pve_exploration_reg(ctx, |reg| {
                reg.hacking_games.get(&(char_id, container_id)).map(|g| {
                    g.tiles.iter().map(|t| {
                        PyRep::keyval([
                            ("tileIndex", PyRep::Integer(t.tile_index as i64)),
                            ("revealed", PyRep::Bool(t.revealed)),
                            ("nodeType", PyRep::String(format!("{:?}", t.kind))),
                            ("coherence", PyRep::Integer(t.coherence as i64)),
                            ("strength", PyRep::Integer(t.strength as i64)),
                        ])
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
            let container_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1_870_000_000);
            let slot_idx = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as usize;
            let target_tile_idx = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as usize;

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
