//! `fw_gameplay` — Live Faction Warfare Warzones, Complex Gating, Battlefields, Supply Sites, I-Hubs & Enlistment (`SO-5`, `Q-8`).
//!
//! Exposes ONLY live server RPC, acceleration-gate, combat, structure-attack, station, and 1 Hz tick entrypoints:
//! - `facWarMgr` corporation & alliance militia enlistment (`JoinFactionAsCorporation`, `LeaveFactionAsCorporation`,
//!   `JoinFactionAsAlliance`, `LeaveFactionAsAlliance`), militia lookup (`GetCorporationWarFactionID`,
//!   `GetFactionMilitiaCorporation`), I-Hub LP donations (`DonateLPsToSolarSystem`), and leaderboards (`GetStats_TopAndAllKillsAndVPs`).
//! - `keeper.ActivateAccelerationGate` live space-ball, `<= 2,500 m` range, and FW hull-class gating (`activate_fw_complex_gate_in_space`).
//! - Live `dogmaIM.Activate` combat weapon cycles & space combat destruction for FW Supply Caches/Depots, FW I-Hubs, and Pirate FOBs
//!   (`apply_fw_or_insurgency_structure_damage`, `on_space_entity_destroyed`, `on_player_ship_destroyed_in_combat`).
//! - 1 Hz `tick_fw_gameplay` complex/battlefield capture, 4h Battlefield respawn cooldown, 2h unattacked I-Hub regeneration,
//!   and Pirate Insurgency campaign progression (`fw_gameplay_insurgency`).

#![forbid(unsafe_code)]

#[path = "fw_gameplay_insurgency.rs"]
pub mod fw_gameplay_insurgency;
pub use fw_gameplay_insurgency::*;
#[path = "fw_gameplay_rpc.rs"]
pub mod fw_gameplay_rpc;
pub use fw_gameplay_rpc::*;
#[path = "fw_gameplay_supply.rs"]
pub(crate) mod fw_gameplay_supply;

use crate::router::ServerContext;
use crate::services::fw_services::{
    add_victory_points, calculate_capture_speed_multiplier,
    calculate_fw_lp_reward, complete_fw_dungeon_and_objective, set_facwar_bunker_vulnerable,
    compute_system_frontline_status, fw_opposing_faction,
    spawn_battlefield_site, update_system_advantage, STATUS_COMMAND, STATUS_FRONTLINE,
    STATUS_REARGUARD,
};
use crate::services::insurgency_services::{
    PIRATE_STRONGHOLD_ANGEL_TYPE, PIRATE_STRONGHOLD_GURISTAS_TYPE,
};
use eve_db::UpwellStructureRecord;
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

/// SDE type ID for Faction Warfare Infrastructure Hub (`32226`).
pub const TYPE_FW_IHUB: u32 = 32_226;
/// SDE type ID for Acceleration Gate (`17831`).
pub const TYPE_ACCELERATION_GATE: u32 = 17_831;
/// SDE type ID for Supply Cache / Supply Depot structure (`17621` / `32226` tactical depot).
pub const TYPE_FW_SUPPLY_SITE: u32 = 17_621;
/// Battlefield respawn cooldown (4 hours = `14,400 s`), persisted in `fw_runtime.battlefield_respawn_unix`.
pub const BATTLEFIELD_RESPAWN_COOLDOWN_SECS: u64 = 4 * 3_600;
/// Maximum LP awarded per pilot for a completed Battlefield site (`150,000 LP`).
pub const BATTLEFIELD_MAX_PILOT_LP: i64 = 150_000;
/// Maximum number of militia pilots in a Battlefield before LP payout scales down (`30` pilots).
pub const BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS: usize = 30;
/// Three capture-zone world coordinates inside a Frontline Battlefield grid (`<= 15 km` each).
pub const BATTLEFIELD_ZONE_CENTERS: [[f64; 3]; 3] = [
    [305_000.0, 0.0, 0.0],
    [320_000.0, 15_000.0, 0.0],
    [335_000.0, 0.0, 0.0],
];

/// Returns the authentic capture duration in seconds for an FW complex tier:
/// Novice / Small: 10 minutes (600s), Medium: 15 minutes (900s), Large / Open: 20 minutes (1200s).
pub fn complex_base_capture_duration_seconds(tier: &str) -> u64 {
    match tier {
        "Novice" | "Small" => 600,
        "Medium" => 900,
        "Large" | "Open" => 1200,
        _ => 600,
    }
}

/// Post-Uprising victory points per completed complex by size (`wiki:Faction_warfare` VP table:
/// Scout 50, Small 75, Moderate 600, Large 300, Open 350).
pub fn complex_victory_points(tier: &str) -> i64 {
    match tier {
        "Novice" | "Scout" => 50,
        "Small" => 75,
        "Medium" => 600,
        "Large" => 300,
        "Open" => 350,
        _ => 50,
    }
}

/// Derives authentic Battlefield 3-zone centers in `sys_id` from an SDE celestial anchor.
pub fn battlefield_zone_centers_for_system(ctx: &ServerContext, sys_id: u32) -> [[f64; 3]; 3] {
    let (cel_pos, _) = crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(ctx, sys_id, 99);
    let center = DVec3::from_array(cel_pos);
    [
        (center + DVec3::new(-15_000.0, 0.0, 0.0)).to_array(),
        center.to_array(),
        (center + DVec3::new(15_000.0, 0.0, 0.0)).to_array(),
    ]
}

/// Resolves the canonical SDE militia corporation ID for `faction_id` (`sde_factions.militia_corporation_id`),
/// used as the authoritative key in `CharacterRecord.loyalty_points` across all FW, Insurgency, and LP Store paths.
pub fn militia_lp_corp_id(sde: &eve_sde::SdeRegistry, faction_id: i64) -> i64 {
    u32::try_from(faction_id)
        .ok()
        .and_then(|fid| sde.factions.get(&fid))
        .and_then(|f| f.militia_corporation_id)
        .unwrap_or(match faction_id {
            500_001 => 1_000_180, // State Protectorate
            500_002 => 1_000_182, // Tribal Liberation Force
            500_003 => 1_000_179, // 24th Imperial Crusade
            500_004 => 1_000_181, // Federal Defense Union
            500_010 => 1_000_437, // Commando Guri
            500_011 => 1_000_436, // Gistii Domination
            other => other,
        })
}

/// Returns `(status, lp_vp_multiplier, allowed_site_tiers, capture_count_factor)` for a warzone system.
pub fn get_system_frontline_rules(
    ctx: &ServerContext,
    sys_id: u32,
) -> (i64, f64, Vec<&'static str>, f64) {
    let status = ctx
        .db
        .with_snapshot(|snap| compute_system_frontline_status(ctx, snap, sys_id));
    match status {
        STATUS_FRONTLINE => (
            STATUS_FRONTLINE,
            1.0,
            vec!["Novice", "Small", "Medium", "Large", "Open", "Battlefield"],
            1.0,
        ),
        STATUS_COMMAND => (
            STATUS_COMMAND,
            0.75,
            vec!["Novice", "Small", "Medium"],
            0.75,
        ),
        _ => (
            STATUS_REARGUARD,
            0.50,
            vec!["Novice", "Small"],
            0.50,
        ),
    }
}

pub fn pocket_pos_for_complex(gate_id: i64) -> DVec3 {
    let slot = (gate_id.unsigned_abs() % 11) as f64;
    DVec3::new(250_000.0 + slot * 4_000.0, 0.0, 0.0)
}

/// Ensures warzone complexes (allowed tiers per Frontline/Command/Rearguard status), Frontline Battlefield
/// (if off 4h respawn cooldown), Supply Cache/Depot, and FW Infrastructure Hub (`32226`) are spawned in `sys_id`.
pub async fn ensure_warzone_sites_spawned(ctx: &ServerContext, sys_id: u32) {
    let now = ctx.now_unix();
    let occ_opt = ctx
        .db
        .with_snapshot(|snap| snap.fw_system_occupations.get(&sys_id).map(|o| o.occupier_faction_id));
    let Some(occ_fac) = occ_opt else {
        return;
    };
    let (status, _, allowed_tiers, _) = get_system_frontline_rules(ctx, sys_id);
    let occ_corp = militia_lp_corp_id(&ctx.sde, occ_fac);

    let mut balls_to_spawn: Vec<(i64, u32, [f64; 3], f64)> = Vec::new();
    let needs_mut = ctx.db.with_snapshot(|snap| {
        let has_stale = snap.fw_runtime.fw_complexes.iter().any(|(_, (csys, tier, _, _, completed))| {
            *completed || (*csys == sys_id && !allowed_tiers.contains(&tier.as_str()))
        });
        if has_stale {
            return true;
        }
        let mut has_any_complex_pos = false;
        for &tier in allowed_tiers.iter().filter(|&&t| t != "Battlefield") {
            let cd_key = format!("{sys_id}:{tier}");
            let tier_cd = snap.fw_runtime.complex_tier_cooldowns.get(&cd_key).copied().unwrap_or(0);
            let existing = snap
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, t, _, _, completed))| *csys == sys_id && t == tier && !*completed)
                .map(|(&gid, _)| gid);
            match existing {
                None if now >= tier_cd => return true,
                Some(gid) => {
                    if !snap.item_positions.contains_key(&gid) {
                        return true;
                    }
                    has_any_complex_pos = true;
                }
                None => {}
            }
        }
        if has_any_complex_pos
            && snap
                .system_states
                .get(&sys_id)
                .is_none_or(|st| st.fw_complex_position.is_none())
        {
            return true;
        }
        for kind in ["SupplyCache", "SupplyDepot", "ListeningPost", "Propaganda"] {
            let exists = snap.fw_runtime.supply_sites.iter().any(|(&sid, (ssys, skind, _))| {
                *ssys == sys_id
                    && skind == kind
                    && snap.structures.get(&sid).is_some_and(|st| st.state != "Destroyed")
            });
            if !exists {
                return true;
            }
        }
        let ihub_ok = snap.structures.values().any(|st| {
            st.solar_system_id == sys_id
                && st.type_id == TYPE_FW_IHUB
                && st.state != "Destroyed"
                && st.owner_corp_id == occ_corp
        });
        if !ihub_ok {
            return true;
        }
        let bf_active = snap
            .fw_runtime
            .battlefield_instances
            .iter()
            .any(|b| b.solar_system_id == sys_id && !b.completed);
        let bf_cd = snap
            .fw_runtime
            .battlefield_respawn_unix
            .get(&sys_id)
            .copied()
            .unwrap_or(0);
        status == STATUS_FRONTLINE && !bf_active && now >= bf_cd
    });
    let spawn_bf = if needs_mut {
        ctx.db.with_mut_snapshot(|snap| {
        // 1. Remove disallowed complex tiers if system downgraded (e.g., Frontline -> Rearguard).
        let stale_gates: Vec<i64> = snap
            .fw_runtime
            .fw_complexes
            .iter()
            .filter(|(_, (csys, tier, _, _, completed))| {
                *csys == sys_id && !*completed && !allowed_tiers.contains(&tier.as_str())
            })
            .map(|(&gid, _)| gid)
            .collect();
        for gid in stale_gates {
            snap.fw_runtime.fw_complexes.remove(&gid);
            snap.fw_runtime.complex_occupants.remove(&gid);
            snap.item_positions.remove(&gid);
        }
        snap.fw_runtime.fw_complexes.retain(|_, (_, _, _, _, completed)| !*completed);

        // 2. Spawn missing allowed FW complex tiers (`Novice`, `Small`, `Medium`, `Large`, `Open`).
        let mut first_complex_pos: Option<[f64; 3]> = None;
        for (idx, &tier) in allowed_tiers.iter().filter(|&&t| t != "Battlefield").enumerate() {
            let cd_key = format!("{sys_id}:{tier}");
            let tier_cd = snap.fw_runtime.complex_tier_cooldowns.get(&cd_key).copied().unwrap_or(0);
            let existing = snap
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, t, _, _, completed))| *csys == sys_id && t == tier && !*completed)
                .map(|(&gid, _)| gid);
            if existing.is_none() && now < tier_cd {
                continue;
            }
            let gid = if let Some(id) = existing {
                id
            } else {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.fw_runtime
                    .fw_complexes
                    .insert(id, (sys_id, tier.to_string(), occ_fac, 0.0, false));
                snap.fw_runtime.complex_guards_cleared.insert(id, false);
                snap.fw_runtime.complex_seconds_elapsed.insert(id, 0);
                snap.fw_runtime.complex_last_step_unix.insert(id, now);
                id
            };
            if !snap.fw_runtime.complex_guards_cleared.get(&gid).copied().unwrap_or(false) {
                let ppos = pocket_pos_for_complex(gid);
                balls_to_spawn.push((gid + 500_000_000, 37_453, [ppos.x + 500.0, ppos.y, ppos.z], 25.0));
            }
            let pos = snap
                .item_positions
                .get(&gid)
                .copied()
                .unwrap_or_else(|| {
                    crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(ctx, sys_id, (idx as u32) + 10).0
                });
            snap.item_positions.insert(gid, pos);
            if first_complex_pos.is_none() {
                first_complex_pos = Some(pos);
            }
            balls_to_spawn.push((gid, TYPE_ACCELERATION_GATE, pos, 250.0));
        }
        if let Some(complex_pos) = first_complex_pos {
            let cels = ctx.sde.celestials_in_system(sys_id);
            let anchor = cels.iter().find(|c| c.group_id == 8).or_else(|| cels.last()).map(|c| [c.position.x + c.radius.max(0.0) + 15_000.0, c.position.y, c.position.z]).unwrap_or(complex_pos);
            let st = snap.system_states.entry(sys_id).or_default();
            st.solar_system_id = sys_id;
            st.fw_complex_position.get_or_insert(anchor);
        }

        // 3. Ensure SupplyCache, SupplyDepot, ListeningPost, and Propaganda tactical objectives exist for Advantage.
        fw_gameplay_supply::spawn_warzone_supply_sites_in_snap(
            ctx,
            snap,
            sys_id,
            occ_fac,
            occ_corp,
            TYPE_FW_SUPPLY_SITE,
            &mut balls_to_spawn,
        );

        // 4. Ensure FW Infrastructure Hub (`32226`) exists in `snap.structures`.
        // DERIVED (synthetic): baseline warzone I-Hub orbital anchor coordinates.
        let ihub_pos = [200_000.0, 0.0, 0.0];
        let ihub_id = snap
            .structures
            .values()
            .find(|st| st.solar_system_id == sys_id && st.type_id == TYPE_FW_IHUB && st.state != "Destroyed")
            .map(|st| st.structure_id)
            .unwrap_or_else(|| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.structures.insert(
                    id,
                    UpwellStructureRecord {
                        structure_id: id,
                        type_id: TYPE_FW_IHUB,
                        owner_corp_id: occ_corp,
                        solar_system_id: sys_id,
                        name: "Faction Warfare Infrastructure Hub".into(),
                        state: "ShieldVulnerable".into(),
                        position: ihub_pos,
                        quantum_core_installed: true,
                        fuel_blocks: 1_000,
                        shield_hp: 5_000.0,
                        armor_hp: 5_000.0,
                        hull_hp: 8_000.0,
                        reinforced_until_unix: 0,
                        last_damage_unix: 0,
                        last_damage_tick: 0,
                        self_destruct_at_unix: None,
                    },
                );
                id
            });
        if let Some(ihub) = snap.structures.get_mut(&ihub_id) {
            ihub.owner_corp_id = occ_corp;
        }
        balls_to_spawn.push((ihub_id, TYPE_FW_IHUB, ihub_pos, 600.0));

        // 5. Check if a Frontline Battlefield should spawn (only in `STATUS_FRONTLINE` and after 4h cooldown).
        let bf_active = snap
            .fw_runtime
            .battlefield_instances
            .iter()
            .any(|b| b.solar_system_id == sys_id && !b.completed);
        let bf_cd = snap
            .fw_runtime
            .battlefield_respawn_unix
            .get(&sys_id)
            .copied()
            .unwrap_or(0);
        status == STATUS_FRONTLINE && !bf_active && now >= bf_cd
    })
    } else {
        false
    };

    if spawn_bf {
        let enemy_fac = fw_opposing_faction(occ_fac).unwrap_or(occ_fac);
        let bf_id = spawn_battlefield_site(ctx, sys_id, enemy_fac, "Frontline Battlefield");
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.battlefield_zones.insert(bf_id, [0, 0, 0]);
            snap.fw_runtime.battlefield_participants.insert(bf_id, Vec::new());
            snap.fw_runtime.battlefield_waves.insert(bf_id, 1);
            snap.fw_runtime.complex_last_step_unix.insert(bf_id, now);
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos, radius) in balls_to_spawn {
        if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none() {
            let ball = Ball {
                id: bid,
                type_id: tid,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: if tid == 37_453 { 350.0 } else { 0.0 },
                inertia: if tid == 37_453 { 1.0 } else { 0.0 },
                mass: if tid == 37_453 { 1.2e6 } else { 5.0e7 },
                radius,
                speed_fraction: 0.0,
                mode: if tid == 37_453 { MovementMode::Stop } else { MovementMode::Rigid },
            };
            if tid == 37_453 {
                let mut cs = ctx.build_type_combat_state(tid);
                if cs.max_structure_hp <= 0.0 {
                    cs.max_shield_hp = 350.0;
                    cs.shield_hp = 350.0;
                    cs.max_armor_hp = 350.0;
                    cs.armor_hp = 350.0;
                    cs.max_structure_hp = 400.0;
                    cs.structure_hp = 400.0;
                }
                let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
            } else {
                let _ = sys_handle.spawn_ball(ball).await;
            }
        }
    }
}

/// Verifies tackle, warp state, navigation-lock modules, immobilizing modules, and Warp Core Stabilizers
/// before activating an acceleration gate.
pub(crate) async fn check_gate_tackle_and_nav_locks(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    ship_id: i64,
    ship_ball: &Ball,
) -> Result<(), BridgeRpcError> {
    if matches!(ship_ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot activate an acceleration gate while warping".into(),
        });
    }
    if let Err(reason) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp") {
        return Err(BridgeRpcError {
            code: "ShipNavigationLocked".into(),
            message: reason.message,
        });
    }
    if let Err(reason) = crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "warp") {
        return Err(BridgeRpcError {
            code: "ImmobilizingModuleActive".into(),
            message: reason.message,
        });
    }
    let has_wcs = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|item| {
            item.location_id == ship_id
                && matches!(item.flag as u16, 11..=34)
                && ctx.sde.types.get(&item.type_id).is_some_and(|t| t.group_id == 315)
        })
    });
    if has_wcs {
        return Err(BridgeRpcError {
            code: "WarpCoreStabilizerRestricted".into(),
            message: "Ships fitted with Warp Core Stabilizers cannot activate this acceleration gate".into(),
        });
    }
    let is_scrambled = sys_handle
        .is_ball_scrambled(ship_id)
        .await
        .unwrap_or_else(|_| sys_handle.is_ball_scrambled_sync(ship_id));
    if is_scrambled {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot activate acceleration gate while warp scrambled or disrupted".into(),
        });
    }
    Ok(())
}

/// Validates a live ship ball's solar system, `<= 2,500 m` proximity, and FW complex hull-size/meta/faction
/// rules on `keeper.ActivateAccelerationGate`, then warps the ship into the complex capture pocket and
/// registers occupancy for `tick_fw_gameplay`.
pub async fn activate_fw_complex_gate_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    gate_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Active ship {ship_id} is not spawned in space in system {sys_id}"),
        })?;

    let (complex_sys, tier, completed, persisted_gate_pos) = ctx
        .db
        .with_snapshot(|snap| {
            let (csys, tier, _, _, done) = snap.fw_runtime.fw_complexes.get(&gate_id)?;
            let pos = snap.item_positions.get(&gate_id).copied();
            Some((*csys, tier.clone(), *done, pos))
        })
        .ok_or_else(|| BridgeRpcError {
            code: "ComplexNotFound".into(),
            message: format!("FW complex {gate_id} not found"),
        })?;

    if complex_sys != sys_id {
        return Err(BridgeRpcError {
            code: "GateNotInSystem".into(),
            message: format!("Acceleration gate {gate_id} is in system {complex_sys}, not {sys_id}"),
        });
    }
    if completed {
        return Err(BridgeRpcError {
            code: "ComplexAlreadyCompleted".into(),
            message: format!("FW complex {gate_id} has already been captured"),
        });
    }

    let (gate_pos, gate_radius) = if let Ok(Some((_, gb))) = sys_handle.get_ball_snapshot(gate_id).await {
        (gb.position, gb.radius)
    } else if let Some(arr) = persisted_gate_pos {
        (DVec3::from_array(arr), 250.0)
    } else {
        return Err(BridgeRpcError {
            code: "GateNotInSpace".into(),
            message: format!("Acceleration gate {gate_id} has no space coordinates"),
        });
    };
    let dist = (ship_ball.position.distance(gate_pos) - ship_ball.radius - gate_radius).max(0.0);
    if dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutsideGateActivationRange".into(),
            message: format!(
                "Ship is {dist:.1} m from FW acceleration gate {gate_id} (must be <= 2500 m)"
            ),
        });
    }
    check_gate_tackle_and_nav_locks(ctx, &sys_handle, ship_id, &ship_ball).await?;

    let ship_type_id = ship_ball.type_id;
    let (group_id, meta_group_id, faction_id, rig_size, sde_mass) = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| {
            (
                t.group_id,
                t.meta_group_id,
                t.faction_id,
                t.default_attributes.get(&1547).copied().unwrap_or(0.0),
                t.default_attributes.get(&4).copied().unwrap_or(ship_ball.mass),
            )
        })
        .unwrap_or((0, None, None, 0.0, ship_ball.mass));
    if ship_type_id == 670 || group_id == 29 {
        return Err(BridgeRpcError {
            code: "ShipTypeNotAllowedInComplex".into(),
            message: "Capsules cannot activate FW complex acceleration gates".into(),
        });
    }

    let is_empire_faction = matches!(faction_id, None | Some(500_001..=500_004));
    let is_t1_or_navy_frigate = group_id == 25
        && matches!(meta_group_id, None | Some(1) | Some(3) | Some(4))
        && is_empire_faction;
    let is_t2_or_pirate_frigate = matches!(group_id, 324 | 830 | 831 | 893 | 1527)
        || (group_id == 25 && (!is_empire_faction || meta_group_id == Some(2)));
    let is_destroyer = matches!(group_id, 420 | 541 | 1305 | 1534);
    let is_cruiser = matches!(group_id, 26 | 358 | 832 | 833 | 894 | 906 | 963);
    let is_bc_or_bs = matches!(group_id, 419 | 540 | 1201 | 27 | 898 | 900);

    let too_large = |t: &str| {
        Err(BridgeRpcError {
            code: "ShipTooLargeForGate".into(),
            message: format!(
                "Ship type {ship_type_id} (group {group_id}) is too large for a {t} acceleration gate"
            ),
        })
    };
    match tier.as_str() {
        "Novice" => {
            if !is_t1_or_navy_frigate {
                if is_t2_or_pirate_frigate {
                    return Err(BridgeRpcError {
                        code: "ShipTypeNotAllowedInComplex".into(),
                        message: format!(
                            "Ship type {ship_type_id} (group {group_id}) is not allowed in a Novice complex (T1/Navy Frigates only)"
                        ),
                    });
                }
                return too_large("Novice");
            }
        }
        "Small" => {
            if !(is_t1_or_navy_frigate || is_t2_or_pirate_frigate || is_destroyer) {
                return too_large("Small");
            }
        }
        "Medium" => {
            if !(is_t1_or_navy_frigate || is_t2_or_pirate_frigate || is_destroyer || is_cruiser) {
                return too_large("Medium");
            }
        }
        "Large" => {
            if !(is_t1_or_navy_frigate
                || is_t2_or_pirate_frigate
                || is_destroyer
                || is_cruiser
                || is_bc_or_bs)
            {
                return too_large("Large");
            }
        }
        _ => {
            let is_capital_hull = rig_size >= 4.0
                || sde_mass > 500_000_000.0
                || ship_ball.mass > 500_000_000.0
                || matches!(
                    group_id,
                    30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594 | 5120
                );
            if is_capital_hull {
                return too_large(&tier);
            }
        }
    }

    let pocket_pos = pocket_pos_for_complex(gate_id);
    let existing_occ: Vec<(i64, i64)> = ctx.db.with_snapshot(|snap| {
        snap.fw_runtime
            .complex_occupants
            .get(&gate_id)
            .cloned()
            .unwrap_or_default()
    });
    let mut someone_on_grid = false;
    for (other_c, other_s) in existing_occ {
        if other_c == char_id {
            continue;
        }
        if let Ok(Some((_, ob))) = sys_handle.get_ball_snapshot(other_s).await {
            if ob.position.distance(pocket_pos) <= 30_000.0
                && !matches!(ob.mode, MovementMode::Warp { .. })
            {
                someone_on_grid = true;
                break;
            }
        }
    }
    let _ = sys_handle.set_ball_position(ship_id, pocket_pos).await;
    let _ = sys_handle.set_speed_fraction(ship_id, 0.0).await;
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions
            .insert(ship_id, [pocket_pos.x, pocket_pos.y, pocket_pos.z]);
        let occ = snap.fw_runtime.complex_occupants.entry(gate_id).or_default();
        if !someone_on_grid {
            // The capture timer only runs while a pilot holds the pocket: restart its step clock on entry.
            snap.fw_runtime.complex_last_step_unix.insert(gate_id, now_unix);
        }
        if !occ.iter().any(|&(c, _)| c == char_id) {
            occ.push((char_id, ship_id));
        }
    });

    Ok(PyRep::keyval([
        ("gateID", PyRep::Integer(gate_id)),
        ("complexTier", PyRep::String(tier)),
        ("activated", PyRep::Bool(true)),
        ("destinationX", PyRep::Real(pocket_pos.x)),
        ("destinationY", PyRep::Real(pocket_pos.y)),
        ("destinationZ", PyRep::Real(pocket_pos.z)),
    ]))
}

/// Returns true if `structure_id` / `type_id` is an FW Infrastructure Hub, FW Supply Cache/Depot, or Pirate FOB.
pub fn is_fw_or_insurgency_structure_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    structure_id: i64,
    type_id: u32,
) -> bool {
    type_id == TYPE_FW_IHUB
        || snap.fw_runtime.supply_sites.contains_key(&structure_id)
        || matches!(type_id, PIRATE_STRONGHOLD_ANGEL_TYPE | PIRATE_STRONGHOLD_GURISTAS_TYPE)
        || sde.types.get(&type_id).is_some_and(|t| t.group_id == 4644)
}

/// Applies live weapon damage (`dogmaIM.Activate`) to an FW Supply Cache/Depot,
/// FW Infrastructure Hub (`32226`), or Pirate FOB (`79172` / `78260`).
pub async fn apply_fw_or_insurgency_structure_damage(
    ctx: &ServerContext,
    attacker_char_id: i64,
    structure_id: i64,
    damage: f64,
) -> Result<PyRep, BridgeRpcError> {
    let st_type = ctx
        .db
        .with_snapshot(|s| s.structures.get(&structure_id).map(|st| st.type_id))
        .ok_or_else(|| BridgeRpcError {
            code: "StructureNotFound".into(),
            message: format!("Structure {structure_id} not found"),
        })?;
    if matches!(st_type, PIRATE_STRONGHOLD_ANGEL_TYPE | PIRATE_STRONGHOLD_GURISTAS_TYPE)
        || ctx.sde.types.get(&st_type).is_some_and(|t| t.group_id == 4644)
    {
        return apply_pirate_fob_structure_damage(ctx, attacker_char_id, structure_id, damage).await;
    }

    let now = ctx.now_unix();
    // 1. Supply Cache / Supply Depot objective destruction -> Advantage shift + space container loot drop!
    let supply_info = ctx
        .db
        .with_snapshot(|s| s.fw_runtime.supply_sites.get(&structure_id).cloned());
    if let Some((sys_id, site_kind, site_fac)) = supply_info {
        return fw_gameplay_supply::apply_supply_site_damage(
            ctx,
            attacker_char_id,
            structure_id,
            damage,
            sys_id,
            site_kind,
            site_fac,
            now,
        )
        .await;
    }

    // 2. FW Infrastructure Hub (`32226`) siege -> flips warzone system occupancy on destruction!
    let (sys_id, flipped_to, s_hp, a_hp, h_hp) = ctx.db.try_with_mut_snapshot(|snap| {
        let sys_id = snap
            .structures
            .get(&structure_id)
            .map(|s| s.solar_system_id)
            .ok_or_else(|| BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("FW I-Hub {structure_id} not found"),
            })?;
        let occ = snap.fw_system_occupations.get(&sys_id).ok_or_else(|| BridgeRpcError {
            code: "SystemNotWarzone".into(),
            message: format!("System {sys_id} is not an FW warzone system"),
        })?;
        let occ_fac = occ.occupier_faction_id;
        // An invulnerable I-Hub rejects every attacker before militia checks (client shows it as invulnerable).
        let surr_vuln = snap
            .insurgency_runtime
            .surrender_ihub_vulnerable_unix
            .get(&sys_id)
            .is_some_and(|&ts| now >= ts);
        let is_vuln = snap
            .fw_runtime
            .ihub_vulnerable
            .get(&sys_id)
            .copied()
            .unwrap_or(false)
            || surr_vuln;
        if !is_vuln {
            return Err(BridgeRpcError {
                code: "IHubInvulnerable".into(),
                message: format!(
                    "FW Infrastructure Hub in system {sys_id} is invulnerable until 100% contested or post-surrender window"
                ),
            });
        }
        let att_fac = snap
            .characters
            .get(&attacker_char_id)
            .and_then(|c| c.war_faction_id)
            .ok_or_else(|| BridgeRpcError {
                code: "NotEnlistedInOpposingMilitia".into(),
                message: "Only opposing militia pilots can attack an FW Infrastructure Hub".into(),
            })?;
        if fw_opposing_faction(occ_fac) != Some(att_fac) {
            return Err(BridgeRpcError {
                code: "NotEnlistedInOpposingMilitia".into(),
                message: format!(
                    "Pilot faction {att_fac} is not the opposing militia for occupier {occ_fac}"
                ),
            });
        }
        if surr_vuln {
            if let Some(o) = snap.fw_system_occupations.get_mut(&sys_id) {
                o.victory_points = o.victory_points_threshold;
                snap.fw_runtime
                    .system_victory_points
                    .insert(sys_id, o.victory_points_threshold);
            }
            if let Some(ss) = snap.system_states.get_mut(&sys_id) {
                ss.fw_contested_pct = 100.0;
            }
        }
        snap.fw_runtime.last_ihub_attacked_unix.insert(sys_id, now);
        let st = snap.structures.get_mut(&structure_id).expect("checked above");
        st.last_damage_unix = now;
        let mut rem = damage.max(0.0);
        for layer in [&mut st.shield_hp, &mut st.armor_hp, &mut st.hull_hp] {
            if rem > 0.0 && *layer > 0.0 {
                let take = rem.min(*layer);
                *layer -= take;
                rem -= take;
            }
        }
        if st.hull_hp > 0.0 {
            return Ok((sys_id, None, st.shield_hp, st.armor_hp, st.hull_hp));
        }
        // Queue system capture for the next daily downtime (`feat:fw.system-capture`).
        st.state = "Destroyed".into();
        st.shield_hp = 0.0;
        st.armor_hp = 0.0;
        st.hull_hp = 0.0;
        snap.fw_runtime.pending_downtime_captures.insert(sys_id, att_fac);
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, false);
        Ok((sys_id, Some(att_fac), 0.0, 0.0, 0.0))
    })?;

    if let Some(new_fac) = flipped_to {
        set_facwar_bunker_vulnerable(ctx, sys_id, new_fac, false);
        return Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(structure_id)),
            ("state", PyRep::String("Destroyed".into())),
            ("pendingOccupierFactionID", PyRep::Integer(new_fac)),
            ("shieldHP", PyRep::Real(0.0)),
            ("armorHP", PyRep::Real(0.0)),
            ("hullHP", PyRep::Real(0.0)),
        ]));
    }

    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("state", PyRep::String("ShieldVulnerable".into())),
        ("shieldHP", PyRep::Real(s_hp)),
        ("armorHP", PyRep::Real(a_hp)),
        ("hullHP", PyRep::Real(h_hp)),
    ]))
}

/// Live 1 Hz Faction Warfare & Pirate Insurgency background scheduler (`cluster_tick.rs`).
pub async fn tick_fw_gameplay(ctx: &ServerContext, now_unix: u64) {
    // 1. Seed warzone complexes, supply sites, I-Hubs, and Frontline Battlefields in active warzone systems.
    let active_wz_sys: Vec<u32> = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .keys()
            .copied()
            .filter(|sys| {
                ctx.systems.active_system_handles().iter().any(|(s, _)| s == sys)
                    || snap.characters.values().any(|c| c.solar_system_id == *sys)
            })
            .collect()
    });
    for sys in active_wz_sys {
        ensure_warzone_sites_spawned(ctx, sys).await;
    }

    // 2. Step live FW complex capture timers for ships inside `complex_occupants`.
    let active_complexes: Vec<(i64, u32, String, i64, Vec<(i64, i64)>)> = ctx.db.with_snapshot(|snap| {
        snap.fw_runtime
            .complex_occupants
            .iter()
            .filter_map(|(&gid, occ)| {
                let (sys, tier, fac, _, done) = snap.fw_runtime.fw_complexes.get(&gid)?;
                if *done || occ.is_empty() {
                    return None;
                }
                Some((gid, *sys, tier.clone(), *fac, occ.clone()))
            })
            .collect()
    });
    for (gid, sys_id, tier, occ_fac, occupants) in active_complexes {
        let pocket = pocket_pos_for_complex(gid);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let guard_id = gid + 500_000_000;
        let mut present: Vec<(i64, i64)> = Vec::new();
        for (cid, shid) in occupants {
            let cloaked = sys_handle
                .get_combat_state(shid)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_cloaked);
            if cloaked {
                continue;
            }
            if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(shid).await {
                let is_pod = b.type_id == 670
                    || ctx.sde.types.get(&b.type_id).is_some_and(|t| t.group_id == 29);
                if !is_pod
                    && !matches!(b.mode, MovementMode::Warp { .. })
                    && b.position.distance(pocket) <= 30_000.0
                {
                    let fac = ctx
                        .db
                        .with_snapshot(|s| s.characters.get(&cid).and_then(|c| c.war_faction_id));
                    if let Some(f) = fac.filter(|&f| matches!(f, 500_001..=500_004)) {
                        present.push((cid, f));
                    }
                }
            }
        }
        let hold_timer = |ctx: &ServerContext| {
            ctx.fw_idle_last_step_unix
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(gid, now_unix);
        };
        if present.is_empty() {
            hold_timer(ctx);
            continue;
        }
        let first_fac = present[0].1;
        if present.iter().any(|&(_, f)| f != first_fac) {
            // Contested by opposing militia ships inside the capture pocket -> timer paused!
            hold_timer(ctx);
            continue;
        }
        if first_fac != occ_fac {
            let guard_alive = sys_handle.get_ball_snapshot(guard_id).await.ok().flatten().is_some();
            let guard_cleared = ctx.db.with_snapshot(|s| {
                s.fw_runtime.complex_guards_cleared.get(&gid).copied().unwrap_or(false)
            }) || !guard_alive;
            if !guard_cleared {
                hold_timer(ctx);
                continue;
            }
            ctx.db.with_mut_snapshot(|s| {
                s.fw_runtime.complex_guards_cleared.insert(gid, true);
            });
        }
        let (occ_adv, attacker_adv) = ctx
            .db
            .with_snapshot(|s| s.fw_runtime.system_advantage.get(&sys_id).copied())
            .unwrap_or((0.0, 0.0));
        let (my_adv, their_adv) = if first_fac == occ_fac {
            (occ_adv, attacker_adv)
        } else {
            (attacker_adv, occ_adv)
        };
        let speed_mult = calculate_capture_speed_multiplier(my_adv, their_adv);
        let mem_last = ctx
            .fw_idle_last_step_unix
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&gid)
            .copied();
        let db_last = ctx.db.with_snapshot(|s| {
            s.fw_runtime.complex_last_step_unix.get(&gid).copied()
        });
        let last_step = db_last.into_iter().chain(mem_last).max().unwrap_or(now_unix);
        let dt = now_unix.saturating_sub(last_step).max(1);
        let base_dur = complex_base_capture_duration_seconds(&tier);
        let delta_pct = ((dt as f64 * speed_mult) / (base_dur as f64) * 100.0).clamp(0.01, 100.0);
        let done_now = ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.complex_last_step_unix.insert(gid, now_unix);
            let Some(entry) = snap.fw_runtime.fw_complexes.get_mut(&gid) else {
                return false;
            };
            let prev_cap = snap.fw_runtime.complex_capturing_faction.get(&gid).copied();
            if prev_cap.is_some_and(|cf| cf != first_fac) && entry.3 > 0.0 {
                if entry.3 > delta_pct {
                    entry.3 = (entry.3 - delta_pct).max(0.0);
                } else {
                    let rem = (delta_pct - entry.3).clamp(0.0, 100.0);
                    entry.3 = rem;
                    if rem > 0.0 {
                        snap.fw_runtime.complex_capturing_faction.insert(gid, first_fac);
                    } else {
                        snap.fw_runtime.complex_capturing_faction.remove(&gid);
                    }
                }
            } else {
                snap.fw_runtime.complex_capturing_faction.insert(gid, first_fac);
                entry.3 = (entry.3 + delta_pct).clamp(0.0, 100.0);
            }
            if entry.3 >= 100.0 && !entry.4 {
                entry.4 = true;
                let cd_key = format!("{sys_id}:{tier}");
                snap.fw_runtime.complex_tier_cooldowns.insert(cd_key, now_unix + 1800);
                snap.fw_runtime.complex_occupants.remove(&gid);
                snap.item_positions.remove(&gid);
                snap.fw_runtime.complex_guards_cleared.remove(&gid);
                snap.fw_runtime.complex_seconds_elapsed.remove(&gid);
                snap.fw_runtime.complex_last_step_unix.remove(&gid);
                snap.fw_runtime.complex_capturing_faction.remove(&gid);
                true
            } else {
                false
            }
        });
        crate::services::notify_emitters_space_dungeons::emit_fw_capture_changed(
            ctx,
            sys_id,
            gid,
            first_fac,
            delta_pct,
            base_dur as f64 * 1000.0,
        );
        if done_now {
            let _ = sys_handle.remove_ball(gid).await;
            let _ = sys_handle.remove_ball(guard_id).await;
            // EVE Uprising NVY-1 / Open base LP rewards by complex tier, split across capturing pilots (`feat:fw.fw-lp`).
            let base_lp = match tier.as_str() {
                "Novice" | "Scout" => 10_000,
                "Small" => 15_000,
                "Medium" => 20_000,
                "Large" | "Open" => 30_000,
                _ => 15_000,
            };
            let (status, _, _, _) = get_system_frontline_rules(ctx, sys_id);
            let (is_contested_or_vulnerable, is_vulnerable_or_lost) = ctx.db.with_snapshot(|s| {
                s.fw_system_occupations
                    .get(&sys_id)
                    .map(|o| {
                        (
                            o.victory_points > 0,
                            o.victory_points >= o.victory_points_threshold,
                        )
                    })
                    .unwrap_or((false, false))
            });
            let supp_lp_mult = crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier(ctx, sys_id, first_fac);
            let offensive_lp = (calculate_fw_lp_reward(base_lp, status) as f64) * supp_lp_mult;
            let total_lp = if first_fac == occ_fac {
                if is_contested_or_vulnerable {
                    offensive_lp.round() as i64
                } else {
                    0
                }
            } else if is_vulnerable_or_lost {
                0
            } else {
                offensive_lp.round() as i64
            };
            let per_pilot_lp = total_lp / (present.len().max(1) as i64);
            let lp_corp = militia_lp_corp_id(&ctx.sde, first_fac);
            let net_adv = (my_adv - their_adv).max(0.0);
            let base_vp = ((complex_victory_points(&tier) as f64 * (1.0 + net_adv * 0.01)).round() as i64).max(1);
            let vp_earned = match status {
                STATUS_FRONTLINE => base_vp,
                STATUS_COMMAND => (base_vp * 3) / 4,
                _ => base_vp / 2,
            };
            let per_pilot_vp = (vp_earned / (present.len().max(1) as i64)).max(1);
            ctx.db.with_mut_snapshot(|snap| {
                for &(cid, _) in &present {
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        *ch.loyalty_points.entry(lp_corp).or_insert(0) += per_pilot_lp;
                    }
                    crate::services::fw_services::record_fw_stat_event_in_snap(
                        snap,
                        now_unix,
                        cid,
                        Some(first_fac),
                        0,
                        per_pilot_vp,
                    );
                }
            });
            let signed_vp = if first_fac == occ_fac { -vp_earned } else { vp_earned };
            add_victory_points(ctx, sys_id, signed_vp);
            complete_fw_dungeon_and_objective(ctx, sys_id, first_fac, first_fac != occ_fac, 1, gid);
        }
    }

    // 2b. Step live FW Supply Cache / Depot friendly defense resolution
    let active_supply_sites: Vec<(i64, u32, String, i64)> = ctx.db.with_snapshot(|snap| {
        snap.fw_runtime.supply_sites.iter().filter_map(|(&sid, (sys, kind, fac))| {
            if snap.structures.get(&sid).map(|st| st.state.as_str()) != Some("Destroyed") {
                Some((sid, *sys, kind.clone(), *fac))
            } else {
                None
            }
        }).collect()
    });
    for (sid, sys_id, site_kind, site_fac) in active_supply_sites {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let site_pos = ctx.db.with_snapshot(|s| s.structures.get(&sid).map(|st| DVec3::from_array(st.position)));
        let Some(pos) = site_pos else { continue; };
        let balls = sys_handle.list_balls().await.unwrap_or_default();
        let (friendly, enemy) = ctx.db.with_snapshot(|snap| {
            let mut f_cnt = 0;
            let mut e_cnt = 0;
            for b in &balls {
                if b.position.distance(pos) <= 30_000.0 {
                    if let Some(f) = snap.characters.values().find(|c| c.active_ship_id == b.id).and_then(|c| c.war_faction_id) {
                        if f == site_fac { f_cnt += 1; } else if matches!(f, 500_001..=500_004) { e_cnt += 1; }
                    }
                }
            }
            (f_cnt, e_cnt)
        });
        if friendly > 0 && enemy == 0 {
            let should_reward = ctx.db.with_mut_snapshot(|snap| {
                let defended = snap
                    .structures
                    .get(&sid)
                    .is_some_and(|st| st.last_damage_unix > 0 && now_unix >= st.last_damage_unix + 30);
                if defended {
                    snap.fw_runtime.supply_sites.remove(&sid);
                    snap.structures.remove(&sid);
                    snap.items.remove(&sid);
                    snap.item_positions.remove(&sid);
                    true
                } else {
                    false
                }
            });
            if should_reward {
                let _ = sys_handle.remove_ball(sid).await;
                let adv_delta = if site_kind == "SupplyDepot" { 5.0 } else { 2.0 };
                let occ_fac = ctx
                    .db
                    .with_snapshot(|s| s.fw_system_occupations.get(&sys_id).map(|o| o.occupier_faction_id))
                    .unwrap_or(site_fac);
                let (occ_d, att_d) = if site_fac == occ_fac { (adv_delta, 0.0) } else { (0.0, adv_delta) };
                update_system_advantage(ctx, sys_id, occ_d, att_d);
            }
        }
    }

    // 3. Step live Frontline Battlefield 3-zone capture from real militia ship balls in space.
    let active_bfs: Vec<(i64, u32, i64)> = ctx.db.with_snapshot(|snap| {
        snap.fw_runtime
            .battlefield_instances
            .iter()
            .filter(|b| !b.completed)
            .map(|b| (b.instance_id, b.solar_system_id, b.faction_id))
            .collect()
    });
    for (bf_id, sys_id, bf_fac) in active_bfs {
        let occ_fac = ctx
            .db
            .with_snapshot(|s| s.fw_system_occupations.get(&sys_id).map(|o| o.occupier_faction_id))
            .unwrap_or(bf_fac);
        let enemy_fac = fw_opposing_faction(occ_fac).unwrap_or(bf_fac);
        let has_pilots = ctx.db.with_snapshot(|snap| {
            snap.characters.values().any(|c| {
                c.solar_system_id == sys_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && matches!(c.war_faction_id, Some(f) if f == occ_fac || f == enemy_fac)
            })
        });
        if !has_pilots {
            continue;
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let sys_zones = battlefield_zone_centers_for_system(ctx, sys_id);
        let bf_center = DVec3::from_array(sys_zones[1]);
        // Spawn real combat-capable friendly and enemy NPC wave ships at the 3 capture zones.
        let wave_id = ctx.db.with_snapshot(|snap| snap.fw_runtime.battlefield_waves.get(&bf_id).copied().unwrap_or(1));
        let npcs_to_spawn = ctx.db.with_mut_snapshot(|snap| {
            let wave_npcs = snap.fw_runtime.battlefield_npc_waves.entry(bf_id).or_default();
            if wave_npcs.len() < 6 {
                let mut spawns = Vec::new();
                for zi in 0..3 {
                    let zpos = DVec3::from_array(sys_zones[zi]);
                    let fnid = bf_id + 1_000_000 + (wave_id as i64) * 100 + (zi as i64) * 2 + 1;
                    let enid = bf_id + 1_000_000 + (wave_id as i64) * 100 + (zi as i64) * 2 + 2;
                    if !wave_npcs.contains(&fnid) {
                        wave_npcs.push(fnid);
                        spawns.push((fnid, 37_453, occ_fac, zpos + DVec3::new(1_500.0, 0.0, 0.0)));
                    }
                    if !wave_npcs.contains(&enid) {
                        wave_npcs.push(enid);
                        spawns.push((enid, 37_453, enemy_fac, zpos + DVec3::new(-1_500.0, 0.0, 0.0)));
                    }
                }
                spawns
            } else {
                Vec::new()
            }
        });
        for (nid, ntid, nfac, npos) in npcs_to_spawn {
            spawn_battlefield_wave_npc(ctx, sys_id, bf_id, nid, ntid, nfac, npos).await;
        }

        let balls = sys_handle.list_balls().await.unwrap_or_default();
        let mut uncloaked_balls = Vec::with_capacity(balls.len());
        for b in balls {
            if b.type_id == 670 {
                // Faction_warfare.md:208: Pods cannot capture or hold a Battlefield point.
                continue;
            }
            let cloaked = sys_handle
                .get_combat_state(b.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_cloaked);
            if !cloaked {
                uncloaked_balls.push(b);
            }
        }
        let pilot_ships: Vec<(i64, i64, DVec3)> = ctx.db.with_snapshot(|snap| {
            uncloaked_balls
                .iter()
                .filter_map(|b| {
                    let ch = snap.characters.values().find(|c| {
                        c.active_ship_id == b.id
                            && c.solar_system_id == sys_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                    })?;
                    let fac = ch.war_faction_id?;
                    Some((ch.character_id, fac, b.position))
                })
                .collect()
        });
        let fixed_center = DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[1]);
        let all_on_grid: Vec<(i64, i64)> = pilot_ships
            .iter()
            .filter(|(_, f, pos)| (*f == occ_fac || *f == enemy_fac) && (pos.distance(bf_center) <= 60_000.0 || pos.distance(fixed_center) <= 60_000.0))
            .map(|&(cid, f, _)| (cid, f))
            .collect();
        if all_on_grid.is_empty() {
            ctx.fw_idle_last_step_unix
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(bf_id, now_unix);
            continue;
        }
        let mem_last = ctx
            .fw_idle_last_step_unix
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&bf_id)
            .copied();
        let db_last = ctx.db.with_snapshot(|s| {
            s.fw_runtime.complex_last_step_unix.get(&bf_id).copied()
        });
        let last_step = db_last.into_iter().chain(mem_last).max().unwrap_or(now_unix);
        let dt = now_unix.saturating_sub(last_step).max(1);
        let mut zones = [0i64; 3];
        for zi in 0..3 {
            let zpos_cel = DVec3::from_array(sys_zones[zi]);
            let zpos_fixed = DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[zi]);
            let occ_in = pilot_ships
                .iter()
                .any(|(_, f, p)| *f == occ_fac && (p.distance(zpos_cel) <= 15_000.0 || p.distance(zpos_fixed) <= 15_000.0));
            let enemy_in = pilot_ships
                .iter()
                .any(|(_, f, p)| *f == enemy_fac && (p.distance(zpos_cel) <= 15_000.0 || p.distance(zpos_fixed) <= 15_000.0));
            if occ_in && !enemy_in {
                zones[zi] = occ_fac;
            } else if enemy_in && !occ_in {
                zones[zi] = enemy_fac;
            }
        }
        let occ_held = zones.iter().filter(|&&z| z == occ_fac).count();
        let enemy_held = zones.iter().filter(|&&z| z == enemy_fac).count();
        if occ_held == enemy_held {
            ctx.db.with_mut_snapshot(|snap| {
                let sec_map = snap.fw_runtime.battlefield_pilot_seconds.entry(bf_id).or_default();
                for &(cid, _) in &all_on_grid {
                    *sec_map.entry(cid).or_insert(0) += dt;
                }
                snap.fw_runtime.battlefield_zones.insert(bf_id, zones);
                snap.fw_runtime.complex_last_step_unix.insert(bf_id, now_unix);
            });
            continue;
        }
        let (lead_fac, net_held) = if enemy_held > occ_held {
            (enemy_fac, enemy_held - occ_held)
        } else {
            (occ_fac, occ_held - enemy_held)
        };
        let mut winning_pilots: Vec<i64> = all_on_grid
            .iter()
            .filter(|&&(_, f)| f == lead_fac)
            .map(|&(cid, _)| cid)
            .collect();
        let (occ_adv, att_adv) = ctx
            .db
            .with_snapshot(|s| s.fw_runtime.system_advantage.get(&sys_id).copied())
            .unwrap_or((0.0, 0.0));
        let (my_adv, their_adv) = if lead_fac == occ_fac { (occ_adv, att_adv) } else { (att_adv, occ_adv) };
        let speed_mult = calculate_capture_speed_multiplier(my_adv, their_adv);
        // DERIVED (synthetic): baseline 900s (15 min) full-zone Battlefield capture timer.
        let delta_pct = ((dt as f64) * (net_held as f64) * speed_mult / 900.0 * 100.0).clamp(0.1, 100.0);
        let done_bf = ctx.db.with_mut_snapshot(|snap| {
            let sec_map = snap.fw_runtime.battlefield_pilot_seconds.entry(bf_id).or_default();
            for &(cid, _) in &all_on_grid {
                *sec_map.entry(cid).or_insert(0) += dt;
            }
            // DERIVED (synthetic): Faction_warfare.md:209 documents full LP payout for up to 30 pilots;
            // ranking winning pilots by cumulative time on the Battlefield grid and applying the same 30-pilot cap to standing awards is derived.
            winning_pilots.sort_by(|a, b| {
                let a_sec = sec_map.get(a).copied().unwrap_or(0);
                let b_sec = sec_map.get(b).copied().unwrap_or(0);
                b_sec.cmp(&a_sec).then_with(|| a.cmp(b))
            });
            snap.fw_runtime.complex_last_step_unix.insert(bf_id, now_unix);
            let Some(pos) = snap
                .fw_runtime
                .battlefield_instances
                .iter()
                .position(|b| b.instance_id == bf_id)
            else {
                return false;
            };
            snap.fw_runtime.battlefield_zones.insert(bf_id, zones);
            snap.fw_runtime
                .battlefield_participants
                .insert(bf_id, winning_pilots.clone());
            snap.fw_runtime
                .battlefield_waves
                .entry(bf_id)
                .and_modify(|v| *v += 1)
                .or_insert(1);
            let bf = &mut snap.fw_runtime.battlefield_instances[pos];
            if bf.faction_id != lead_fac && bf.capture_progress > 0.0 {
                if bf.capture_progress > delta_pct {
                    bf.capture_progress -= delta_pct;
                } else {
                    bf.capture_progress = (delta_pct - bf.capture_progress).clamp(0.0, 100.0);
                    bf.faction_id = lead_fac;
                }
            } else {
                bf.faction_id = lead_fac;
                bf.capture_progress = (bf.capture_progress + delta_pct).clamp(0.0, 100.0);
            }
            if bf.capture_progress >= 100.0 && !bf.completed {
                bf.completed = true;
                snap.fw_runtime
                    .battlefield_respawn_unix
                    .insert(sys_id, now_unix + BATTLEFIELD_RESPAWN_COOLDOWN_SECS);
                snap.fw_runtime.complex_last_step_unix.remove(&bf_id);
                true
            } else {
                false
            }
        });
        if done_bf {
            let n = winning_pilots.len().max(1);
            let raw_per_pilot_lp = if n <= BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS {
                BATTLEFIELD_MAX_PILOT_LP
            } else {
                (BATTLEFIELD_MAX_PILOT_LP * (BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS as i64)) / (n as i64)
            };
            let supp_lp_mult = crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier(ctx, sys_id, lead_fac);
            let per_pilot_lp = ((raw_per_pilot_lp as f64) * supp_lp_mult).round() as i64;
            let lp_corp = militia_lp_corp_id(&ctx.sde, lead_fac);
            let per_pilot_vp = (2_000 / (n as i64)).max(1);
            let cleanup_npcs = ctx.db.with_mut_snapshot(|snap| {
                snap.fw_runtime.battlefield_pilot_seconds.remove(&bf_id);
                for &cid in winning_pilots.iter().take(BATTLEFIELD_MAX_FULL_PAYOUT_PILOTS) {
                    let social_lvl = snap.skills.get(&cid).and_then(|m| m.get(&3355u32)).map(|s| s.level.min(5)).unwrap_or(0);
                    // DERIVED (synthetic): baseline 0.15 raw standing gain before Social skill multiplier.
                    let scaled_st = 0.15 * (1.0 + 0.05 * (social_lvl as f64));
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        let cur_fac_st = ch.standings.get(&lead_fac).copied().unwrap_or(0.0);
                        ch.standings.insert(lead_fac, crate::formulas::apply_standing_increase(cur_fac_st, scaled_st));
                        let cur_corp_st = ch.standings.get(&lp_corp).copied().unwrap_or(0.0);
                        ch.standings.insert(lp_corp, crate::formulas::apply_standing_increase(cur_corp_st, scaled_st));
                    }
                }
                for &cid in &winning_pilots {
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        *ch.loyalty_points.entry(lp_corp).or_insert(0) += per_pilot_lp;
                    }
                    crate::services::fw_services::record_fw_stat_event_in_snap(
                        snap,
                        now_unix,
                        cid,
                        Some(lead_fac),
                        0,
                        per_pilot_vp,
                    );
                }
                snap.fw_runtime.battlefield_npc_waves.remove(&bf_id).unwrap_or_default()
            });
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                for &nid in &cleanup_npcs {
                    rt.npc_entities.remove(&nid);
                }
            });
            for nid in cleanup_npcs {
                let _ = sys_handle.remove_ball(nid).await;
            }
            let _ = sys_handle.remove_ball(bf_id).await;
            let signed_vp = if lead_fac == occ_fac { -2_000 } else { 2_000 };
            add_victory_points(ctx, sys_id, signed_vp);
            let (occ_adv_d, att_adv_d) = if lead_fac == occ_fac { (6.0, 0.0) } else { (0.0, 6.0) };
            update_system_advantage(ctx, sys_id, occ_adv_d, att_adv_d);
            complete_fw_dungeon_and_objective(ctx, sys_id, lead_fac, lead_fac != occ_fac, 2, bf_id);
        }
    }

    // 4. Regenerate unattacked vulnerable FW I-Hubs after 2 hours (`7,200 s`).
    let unattacked_expired_sys: Vec<u32> = ctx.db.with_snapshot(|snap| {
        snap.fw_runtime
            .ihub_vulnerable
            .iter()
            .filter(|(&sys, &vuln)| {
                if !vuln {
                    return false;
                }
                let last_atk = snap
                    .fw_runtime
                    .last_ihub_attacked_unix
                    .get(&sys)
                    .copied()
                    .unwrap_or_else(|| {
                        snap.fw_runtime.ihub_vulnerable_since_unix.get(&sys).copied().unwrap_or(0)
                    });
                now_unix >= last_atk.saturating_add(7_200)
            })
            .map(|(&s, _)| s)
            .collect()
    });
    for sys in unattacked_expired_sys {
        let _ = regenerate_unattacked_fw_ihub(ctx, sys);
    }

    // 5. Advance Pirate Insurgency campaigns and complex timers.
    tick_insurgency_campaigns(ctx, now_unix).await;
    crate::services::insurgency_complex_ops::tick_insurgency_complex_ops(ctx, now_unix).await;
}
/// Removes a destroyed FW complex guard NPC ball from space and marks the complex guard cleared.
pub async fn on_fw_complex_guard_destroyed(
    systems: &crate::system_actor::SystemRegistry,
    db: &eve_db::WorldRepository,
    solar_system_id: u32,
    npc_id: i64,
) {
    let Some(gid) = npc_id.checked_sub(500_000_000).filter(|&g| g > 0) else { return; };
    let is_guard = db.with_mut_snapshot(|snap| {
        let ok = snap.fw_runtime.fw_complexes.get(&gid).is_some_and(|(sys, ..)| *sys == solar_system_id);
        if ok { snap.fw_runtime.complex_guards_cleared.insert(gid, true); }
        ok
    });
    if is_guard {
        let _ = systems.get_or_spawn(solar_system_id).remove_ball(npc_id).await;
    }
}

#[cfg(test)] #[path = "fw_gameplay_tests.rs"]
mod fw_gameplay_tests;

#[path = "fw_gameplay_cov_unit_tests.rs"]
#[cfg(test)]
mod fw_gameplay_cov_unit_tests;

