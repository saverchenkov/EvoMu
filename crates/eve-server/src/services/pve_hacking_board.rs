//! `pve_hacking_board` — Hacking board node generation, analyzer/skill/ship/implant/storm dogma
//! stat resolution, V24 turn-based combat rules, and container/Ghost-Site failure detonation
//! (`feat:pve.hacking-node-types`, `feat:pve.hacking-failure-outcome`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    build_wave_npc_balls, with_pve_exploration_reg, HackingGameSession, HackingNodeKind,
    HackingTileState, HackingUtilityKind,
};
use crate::services::pve_hacking_sites::{
    hacking_hex_distance, hacking_tile_distance_to_core,
};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use eve_sde::SolarSystemId;

type ItemId = i64;

fn is_defensive_node_kind(kind: &HackingNodeKind) -> bool {
    matches!(
        kind,
        HackingNodeKind::Firewall
            | HackingNodeKind::Antivirus
            | HackingNodeKind::RestorationNode
            | HackingNodeKind::VirusSuppressor
    )
}

/// Resolves fitted Data/Relic/Integrated Analyzer stats (`1909 virusCoherence`, `1910 virusStrength`,
/// `1911 virusElementSlots`, `54 maxRange`), Hacking (`21718`) / Archaeology (`13278`) skill bonus
/// (`+10` coherence/level), and ship hull / fitted rig / plugged implant / active booster / electrical storm
/// dogma modifiers (`1915 virusCoherenceBonus`, `1918 virusStrengthBonus`).
// parity: feat:pve.hacking-node-types, feat:exploration.hacking-coherence-and-strength
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

    let (mod_type_id, skill_level, bonus_coh, bonus_str) = ctx.db.with_snapshot(|snap| {
        let fitted_analyzers: Vec<u32> = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && (11..=34).contains(&(i.flag as u16)))
            .filter_map(|i| {
                let t = ctx.sde.get_type(i.type_id)?;
                (t.group_id == 538 || t.default_attributes.get(&1909).copied().unwrap_or(0.0) > 0.0)
                    .then_some(i.type_id)
            })
            .collect();

        let matching = fitted_analyzers.into_iter().find(|&tid| {
            let Some(t) = ctx.sde.get_type(tid) else {
                return false;
            };
            let integrated = (t.default_attributes.contains_key(&182)
                && t.default_attributes.contains_key(&183))
                || (t.default_attributes.contains_key(&1330)
                    && t.default_attributes.contains_key(&1331))
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

        let mut extra_coh = 0i32;
        let mut extra_str = 0i32;

        // Ship hull role bonuses (`1915 virusCoherenceBonus`, `1918 virusStrengthBonus`), e.g. Heron +5, Buzzard/Astero +10
        if let Some(ship_item) = snap.items.get(&ship_id) {
            if let Some(st) = ctx.sde.get_type(ship_item.type_id) {
                extra_coh += st.default_attributes.get(&1915).copied().unwrap_or(0.0) as i32;
                extra_str += st.default_attributes.get(&1918).copied().unwrap_or(0.0) as i32;
            }
        }

        // Fitted rigs / subsystems / modules on the ship
        for item in snap.items.values() {
            if item.location_id != ship_id {
                continue;
            }
            let f = item.flag as u16;
            if !((11..=34).contains(&f) || (92..=99).contains(&f) || (125..=132).contains(&f)) {
                continue;
            }
            let Some(t) = ctx.sde.get_type(item.type_id) else {
                continue;
            };
            if t.group_id == 538 || t.default_attributes.contains_key(&1909) {
                continue;
            }
            let applies = if t.name.contains("Emission Scope Sharpener") {
                is_relic
            } else if t.name.contains("Memetic Algorithm Bank") {
                !is_relic
            } else {
                true
            };
            if applies {
                extra_coh += t.default_attributes.get(&1915).copied().unwrap_or(0.0) as i32;
                extra_str += t.default_attributes.get(&1918).copied().unwrap_or(0.0) as i32;
            }
        }

        // Plugged implants & active boosters on the character
        for item in snap.items.values() {
            if item.location_id != char_id
                || !matches!(item.flag, InventoryFlag::Implant | InventoryFlag::Booster)
            {
                continue;
            }
            let Some(t) = ctx.sde.get_type(item.type_id) else {
                continue;
            };
            let applies = if t.name.contains("Archaeology") {
                is_relic
            } else if t.name.contains("Hacking") {
                !is_relic
            } else {
                true
            };
            if applies {
                extra_coh += t.default_attributes.get(&1915).copied().unwrap_or(0.0) as i32;
                extra_str += t.default_attributes.get(&1918).copied().unwrap_or(0.0) as i32;
            }
        }

        // Metaliminal Electrical Storm system-wide weather beacons stored in per-ServerContext `snap.items`
        let mut storm_coh_bonus = 0i32;
        if let Some(sys_id) = session
            .solar_system_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
        {
            let sys_loc = i64::from(sys_id);
            for item in snap.items.values() {
                if item.location_id != sys_loc || item.flag != InventoryFlag::None {
                    continue;
                }
                let item_bonus = match item.type_id {
                    47382 | 56057 => 25,
                    47381 | 56064 => 10,
                    _ => ctx
                        .sde
                        .get_type(item.type_id)
                        .filter(|t| matches!(t.group_id, 920 | 1975))
                        .and_then(|t| t.default_attributes.get(&1915).copied())
                        .map(|v| v as i32)
                        .unwrap_or(0),
                };
                storm_coh_bonus = storm_coh_bonus.max(item_bonus);
            }
        }

        (matching, lvl, extra_coh + storm_coh_bonus, extra_str)
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

    let total_coh = (base_coh + skill_level * 10 + bonus_coh).max(10);
    let total_str = (base_str + bonus_str).max(5);
    Ok((total_coh, total_str, slots, max_range))
}

/// Computes the hex distance (`1..=5`) from `tile_idx` to the nearest unrevealed
/// System Core, Utility Subsystem, or Data Cache node (`feat:pve.hacking-node-types`).
// parity: feat:pve.hacking-node-types, feat:exploration.hacking-node-network
pub fn hacking_tile_distance_indicator(tile_idx: u32, tiles: &[HackingTileState]) -> u32 {
    tiles
        .iter()
        .filter(|t| {
            !t.revealed
                && matches!(
                    t.kind,
                    HackingNodeKind::SystemCore
                        | HackingNodeKind::DataCache
                        | HackingNodeKind::UtilitySubsystem
                )
        })
        .map(|t| hacking_hex_distance(tile_idx, t.tile_index))
        .min()
        .unwrap_or_else(|| hacking_tile_distance_to_core(tile_idx, tiles))
        .clamp(1, 5)
}

/// Resolved per-tier hacking minigame node coherence and strength stats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HackingNodeStats {
    pub core_coh: i32,
    pub core_str: i32,
    pub fw_coh: i32,
    pub fw_str: i32,
    pub av_coh: i32,
    pub av_str: i32,
    pub support_coh: i32,
    pub support_str: i32,
}

/// Resolves effective hacking tier (`1..=5`) and node coherence/strength stats from SDE
/// container `default_attributes` (`1919 tierDifficulty`, `1927 hackable`) when available.
///
/// DERIVED: `evomu_sde.sqlite` stores `1919 tierDifficulty` (`1.0..=5.0`) and `1927 hackable`
/// on hackable container types (`group_id = 1207` etc.) and `1909 virusCoherence` /
/// `1910 virusStrength` on analyzer modules (`group_id = 538`), but does not include separate
/// `invTypes` rows for internal minigame nodes (`SystemCore`, `Firewall`, `Antivirus`,
/// `RestorationNode`, `VirusSuppressor`). Per-tier node stats are derived from EVE's
/// hacking minigame tier scaling.
// parity: feat:pve.hacking-node-types, feat:exploration.hacking-coherence-and-strength
pub fn resolve_hacking_node_stats(
    sde: Option<&eve_sde::SdeRegistry>,
    container_type_id: Option<u32>,
    is_relic: bool,
    difficulty: u8,
) -> (u8, HackingNodeStats) {
    let sde_tier = sde
        .and_then(|s| container_type_id.and_then(|tid| s.get_type(tid)))
        .and_then(|t| t.default_attributes.get(&1919).copied())
        .filter(|&v| v >= 1.0)
        .map(|v| (v as u8).clamp(1, 5));
    let diff = if difficulty == 0 {
        sde_tier.unwrap_or(2)
    } else if let Some(st) = sde_tier {
        difficulty.max(st).clamp(1, 5)
    } else {
        difficulty.clamp(1, 5)
    };
    let d = diff as i32;
    let stats = HackingNodeStats {
        core_coh: 20 + d * 10 + if is_relic { 10 } else { 0 },
        core_str: 10 + ((d - 1) * 2),
        fw_coh: 20 + d * 4,
        fw_str: 10 + d * 2,
        av_coh: 14 + d * 2,
        av_str: 15 + d * 3,
        support_coh: 14 + d * 2,
        support_str: 10,
    };
    (diff, stats)
}

/// Builds a 24-node hexagonal hacking board containing a `SystemCore`, Defensive Subsystems
/// (`Firewall`, `Antivirus`, `RestorationNode`, `VirusSuppressor`), Utility Subsystems
/// (`SelfRepair`, `KernelRot`, `PolymorphicShield`, `SecondaryVector`), `DataCache` nodes
/// (revealing either a Utility Subsystem or a Defensive Subsystem), and `Empty` nodes,
/// with coherence and strength scaled by `difficulty` (`1..=5`).
// parity: feat:pve.hacking-node-types, feat:exploration.hacking-node-network, feat:exploration.hacking-coherence-and-strength
pub fn build_hacking_grid_procedural(
    container_id: ItemId,
    is_relic: bool,
    difficulty: u8,
    rng_seed: u64,
) -> Vec<HackingTileState> {
    build_hacking_grid_procedural_with_sde(None, container_id, None, is_relic, difficulty, rng_seed)
}

/// SDE-aware variant of [`build_hacking_grid_procedural`] that resolves container `1919 tierDifficulty`
/// from `sde` when `container_type_id` is known.
// parity: feat:pve.hacking-node-types, feat:exploration.hacking-node-network, feat:exploration.hacking-coherence-and-strength
pub fn build_hacking_grid_procedural_with_sde(
    sde: Option<&eve_sde::SdeRegistry>,
    container_id: ItemId,
    container_type_id: Option<u32>,
    is_relic: bool,
    difficulty: u8,
    rng_seed: u64,
) -> Vec<HackingTileState> {
    let (diff, stats) = resolve_hacking_node_stats(sde, container_type_id, is_relic, difficulty);
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
    let HackingNodeStats {
        core_coh,
        core_str,
        fw_coh,
        fw_str,
        av_coh,
        av_str,
        support_coh,
        support_str,
    } = stats;

    let mut pool: Vec<(HackingNodeKind, i32, i32, Option<HackingUtilityKind>)> =
        Vec::with_capacity(22);
    for _ in 0..(1 + (diff as usize) / 3) {
        pool.push((HackingNodeKind::Firewall, fw_coh, fw_str, None));
    }
    // wiki:Hacking difficulty table: on Very Easy / Easy boards Anti-Virus only appears from Data Caches.
    if diff >= 3 {
        for _ in 0..(1 + (diff as usize) / 4) {
            pool.push((HackingNodeKind::Antivirus, av_coh, av_str, None));
        }
    }
    if diff >= 3 {
        pool.push((HackingNodeKind::RestorationNode, support_coh, support_str, None));
        pool.push((HackingNodeKind::VirusSuppressor, support_coh, support_str, None));
    }
    pool.push((
        HackingNodeKind::DataCache,
        0,
        0,
        Some(HackingUtilityKind::PolymorphicShield),
    ));
    pool.push((
        HackingNodeKind::DataCache,
        0,
        0,
        Some(HackingUtilityKind::SelfRepair),
    ));
    pool.push((
        HackingNodeKind::DataCache,
        0,
        0,
        Some(HackingUtilityKind::SecondaryVector),
    ));
    for _ in 0..(1 + (diff as usize) / 2) {
        pool.push((
            HackingNodeKind::DataCache,
            0,
            0,
            Some(HackingUtilityKind::KernelRot),
        ));
    }
    // Direct Utility Subsystem nodes scaled by difficulty
    pool.push((
        HackingNodeKind::UtilitySubsystem,
        0,
        0,
        Some(HackingUtilityKind::SelfRepair),
    ));
    if diff >= 2 {
        pool.push((
            HackingNodeKind::UtilitySubsystem,
            0,
            0,
            Some(HackingUtilityKind::KernelRot),
        ));
    }
    if diff >= 3 {
        pool.push((
            HackingNodeKind::UtilitySubsystem,
            0,
            0,
            Some(HackingUtilityKind::PolymorphicShield),
        ));
    }
    if diff >= 4 {
        pool.push((
            HackingNodeKind::UtilitySubsystem,
            0,
            0,
            Some(HackingUtilityKind::SecondaryVector),
        ));
    }
    // Data Cache concealing a Defensive Subsystem (`utility_drop: None`)
    if pool.len() < 22 {
        pool.push((HackingNodeKind::DataCache, 0, 0, None));
    }
    for _ in pool.len()..22 {
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
            pool_iter
                .next()
                .unwrap_or((HackingNodeKind::Empty, 0, 0, None))
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

/// Resolves one V24 hacking turn when the player clicks `tile_idx`:
/// 1. Ticks active `SecondaryVector` DoT (`-20` coherence on target revealed node).
/// 2. Applies `VirusSuppressor` (`-15` virus strength, minimum `10`).
/// 3. Reveals `DataCache` (yielding either a Utility Subsystem or a Defensive Subsystem) or
///    `UtilitySubsystem` (collecting into an open utility slot), or executes turn-based virus-first
///    attack followed by node retaliation (nullified by `PolymorphicShield` charges).
/// 4. If the System Core is still alive and virus coherence > 0, active `RestorationNode`s heal
///    `+20` coherence to one revealed Defensive Subsystem at the end of the turn (never SystemCore).
// parity: feat:pve.hacking-node-types, feat:exploration.hacking-coherence-and-strength
pub fn resolve_hacking_turn_combat(
    game: &mut HackingGameSession,
    tile_idx: u32,
) -> (String, i32, i32, bool) {
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

    let suppressor_active = game.tiles.iter().any(|t| {
        t.revealed && t.kind == HackingNodeKind::VirusSuppressor && t.coherence > 0
    });
    let eff_str = if suppressor_active {
        (game.base_virus_strength - 15).max(10)
    } else {
        game.base_virus_strength
    };

    let inferred_diff = game
        .tiles
        .iter()
        .find(|t| t.kind == HackingNodeKind::SystemCore)
        .map(|t| {
            (((t.coherence - 20 - if game.is_relic { 10 } else { 0 }) / 10).clamp(1, 5)) as u8
        })
        .unwrap_or(2);

    let was_unrevealed = !game.tiles[tile_idx as usize].revealed;
    let t_ref = &mut game.tiles[tile_idx as usize];
    t_ref.revealed = true;

    if t_ref.kind == HackingNodeKind::DataCache && was_unrevealed {
        if let Some(u) = t_ref.utility_drop.take() {
            if let Some(empty_slot) = game.utilities.iter_mut().find(|s| s.is_none()) {
                *empty_slot = Some(u);
            }
        } else {
            // Data Cache concealed a Defensive Subsystem
            let (_, concealed_stats) =
                resolve_hacking_node_stats(None, None, game.is_relic, inferred_diff);
            if tile_idx % 2 == 0 {
                t_ref.kind = HackingNodeKind::Firewall;
                t_ref.coherence = concealed_stats.fw_coh;
                t_ref.strength = concealed_stats.fw_str;
            } else {
                t_ref.kind = HackingNodeKind::Antivirus;
                t_ref.coherence = concealed_stats.av_coh;
                t_ref.strength = concealed_stats.av_str;
            }
        }
    } else if t_ref.kind == HackingNodeKind::UtilitySubsystem {
        if let Some(u) = t_ref.utility_drop.take() {
            if let Some(empty_slot) = game.utilities.iter_mut().find(|s| s.is_none()) {
                *empty_slot = Some(u);
            }
        }
    } else if t_ref.coherence > 0 {
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
    let core_destroyed = game
        .tiles
        .iter()
        .any(|t| t.kind == HackingNodeKind::SystemCore && t.coherence <= 0);

    // End-of-turn Restoration Node healing: +20 coherence to one revealed Defensive Subsystem (never SystemCore)
    if !core_destroyed && game.virus_coherence > 0 {
        let restorer_active = game.tiles.iter().any(|t| {
            t.revealed && t.kind == HackingNodeKind::RestorationNode && t.coherence > 0
        });
        if restorer_active {
            let candidates: Vec<usize> = game
                .tiles
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    t.revealed && t.coherence > 0 && is_defensive_node_kind(&t.kind)
                })
                .map(|(idx, _)| idx)
                .collect();
            if !candidates.is_empty() {
                let revealed_count = game.tiles.iter().filter(|t| t.revealed).count();
                let pick = candidates[(tile_idx as usize + revealed_count) % candidates.len()];
                game.tiles[pick].coherence += 20;
            }
        }
    }

    (kind_name, rem_coh, eff_str, core_destroyed)
}

/// Self-destructs a standard relic/data container after failing a hack attempt twice (`failure_counts >= 2`).
/// The container and all of its loot are destroyed without harming nearby ships: per wiki:Hacking only
/// Ghost Site (covert research) containers explode for ship damage (see [`detonate_ghost_site`]).
// parity: feat:pve.hacking-failure-outcome, feat:exploration.hack-failure-container-explosion
pub async fn handle_second_hack_failure_explosion(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    site_id: &str,
    container_id: ItemId,
    is_relic: bool,
) -> (bool, f64, Vec<i64>, usize) {
    if site_id.starts_with("STRUCTURE_") {
        return (false, 0.0, Vec::new(), 0);
    }

    with_pve_exploration_reg(ctx, |reg| {
        reg.spewed_cans
            .retain(|_, c| c.parent_container_id != container_id);
    });

    let _ = ctx.systems.get_or_spawn(solar_system_id).remove_ball(container_id).await;
    let _ = is_relic;
    let destroyed_loot_count = ctx.db.with_mut_snapshot(|snap| {
        let before = snap.items.len();
        snap.items
            .retain(|&id, i| id != container_id && i.location_id != container_id);
        snap.item_positions.remove(&container_id);
        before.saturating_sub(snap.items.len())
    });
    (true, 0.0, Vec::new(), destroyed_loot_count)
}

/// Triggers Ghost Site container detonation on hack failure (`failed_container_id = Some(cid)`)
/// or timer expiry (`failed_container_id = None`, destroying all remaining site containers):
/// destroys the affected container(s) and their loot, applies security-band AoE explosive damage
/// (`6,000` High-Sec, `8,000` Low-Sec, `10,000` Null-Sec, `12,000` Wormhole) within `10,000m`,
/// and spawns Pirate response NPCs.
// parity: feat:pve.hacking-failure-outcome, feat:pve.ghost-sites
pub async fn detonate_ghost_site_with_can(
    ctx: &ServerContext,
    _session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
    failed_container_id: Option<ItemId>,
) -> (f64, Vec<i64>) {
    let mut pos = [52_000_000.0, 42_000_000.0, -18_000_000.0];
    let mut dungeon_id = 5534_u32;
    let mut already_detonated = false;
    let mut cans_to_destroy: Vec<ItemId> = Vec::new();
    with_pve_exploration_reg(ctx, |reg| {
        if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
            if let Some(site) = sites.iter_mut().find(|s| s.site_id == site_id) {
                pos = site.position;
                dungeon_id = site.dungeon_id;
                already_detonated = site.ghost_detonated;
                site.ghost_detonated = true;
                site.npc_guards_spawned = true;
                if let Some(cid) = failed_container_id {
                    cans_to_destroy.push(cid);
                } else {
                    cans_to_destroy.extend(site.hackable_can_ids.iter().copied());
                }
            }
        }
        if let Some(cid) = failed_container_id {
            if !cans_to_destroy.contains(&cid) {
                cans_to_destroy.push(cid);
            }
        }
        for &cid in &cans_to_destroy {
            reg.hacked_containers.insert((solar_system_id, cid));
            reg.spewed_cans
                .retain(|_, c| c.parent_container_id != cid);
        }
    });

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let explosive_damage = if (31_000_000..32_000_000).contains(&solar_system_id) {
        12_000.0
    } else if sys_sec <= 0.0 {
        10_000.0
    } else if sys_sec < 0.45 {
        8_000.0
    } else {
        6_000.0
    };

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut blast_center = glam::DVec3::from_array(pos);
    for &cid in &cans_to_destroy {
        if let Ok(Some((_, cb))) = sys_handle.get_ball_snapshot(cid).await {
            blast_center = cb.position;
        }
        let _ = sys_handle.remove_ball(cid).await;
    }
    if !cans_to_destroy.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|&id, item| {
                !cans_to_destroy.contains(&id) && !cans_to_destroy.contains(&item.location_id)
            });
            for &cid in &cans_to_destroy {
                snap.item_positions.remove(&cid);
            }
        });
    }

    let dmg_vec = eve_destiny::DamageVector {
        explosive: explosive_damage,
        ..Default::default()
    };
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
            if is_ship && b.position.distance(blast_center) <= 10_000.0 {
                let _ = sys_handle.apply_damage(b.id, dmg_vec).await;
            }
        }
    }

    let mut ambush_ids = Vec::new();
    if !already_detonated {
        let ambush_balls =
            build_wave_npc_balls(&ctx.sde, solar_system_id, dungeon_id, 2, pos, None);
        for b in ambush_balls {
            ambush_ids.push(b.id);
            let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(
                &ctx.sde, b.type_id,
            );
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

/// Triggers Ghost Site detonation across all remaining containers in the site (`timer expiry`).
// parity: feat:pve.hacking-failure-outcome, feat:pve.ghost-sites
pub async fn detonate_ghost_site(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> (f64, Vec<i64>) {
    detonate_ghost_site_with_can(ctx, session, solar_system_id, site_id, None).await
}

#[cfg(test)]
#[path = "pve_hacking_board_tests.rs"]
mod pve_hacking_board_tests;

