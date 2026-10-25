//! `combat_npc_ai` — Extended NPC AI, Factions, Clone Soldiers, EWAR, Remote Repair, Bounties & Multi-Wave Encounters (`C-4`).
//!
// parity: plan:C-4
// parity: feat:combat.bounty-payout-npc, feat:npc.belt-rats, feat:npc.npc-ai-targeting, feat:npc.npc-damage-types
// parity: feat:npc.npc-loot, feat:npc.sentry-guns, feat:npc.sentry-gun-grid-aggression, feat:npc.drifters
// parity: feat:npc.triglavians, feat:npc.sleepers, feat:npc.edencom-npc, feat:npc.rogue-drones
// parity: feat:npc.incursion-npc, feat:npc.navy-npc, feat:npc.npc-hauler, feat:npc.npc-spawn-encounter
// parity: feat:npc.npc-standings-aggression, feat:npc.officer-npc, feat:npc.npc-ewar-application
// parity: feat:npc.npc-remote-repair, feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns

#![forbid(unsafe_code)]

use super::*;
use eve_db::WalletJournalEntry;
use eve_destiny::{Ball, MovementMode};
use tracing::info;

/// Derives NPC behavioral archetype dynamically from the NPC's `type_id` and `group_id` / `category_id`
/// in `ctx.sde.types` / `ctx.sde.groups` rather than accepting an arbitrary string parameter.
///
/// parity: feat:npc.rogue-drones, feat:npc.incursion-npc, feat:npc.edencom-npc, feat:npc.officer-npc, feat:npc.sleepers
pub fn derive_npc_archetype(ctx: &ServerContext, npc_type_id: u32) -> &'static str {
    let sde_type = ctx.sde.types.get(&npc_type_id);
    let group = sde_type.and_then(|t| ctx.sde.groups.get(&t.group_id));

    let type_name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
    let group_name = group.map(|g| g.name.as_str()).unwrap_or("");
    let group_id = sde_type.map(|t| t.group_id).unwrap_or(0);
    let faction_id = sde_type.and_then(|t| t.faction_id);
    let meta_group_id = sde_type.and_then(|t| t.meta_group_id);
    let meta_level = sde_type.and_then(|t| t.meta_level).unwrap_or(0);

    // 1. Pochven Hypnosian Sleepers
    if type_name.contains("Hypnosian") || npc_type_id == 56680 || (56319..=56327).contains(&npc_type_id) {
        return "PochvenHypnosianSleeper";
    }

    // 2. Sleepers: Sleeper group IDs (e.g. 959..=961, 982..=1005), Sleeper factions (500021, 500022), or group/type name
    if group_name.contains("Sleeper")
        || type_name.contains("Sleeper")
        || (959..=961).contains(&group_id)
        || (982..=1005).contains(&group_id)
        || matches!(faction_id, Some(500021 | 500022))
    {
        return "Sleeper";
    }

    // 3. Rogue Drones: Rogue Drone faction (500008), groups (287, 755..=761, 801..=805, 1174), or group name
    if group_name.contains("Rogue Drone")
        || matches!(faction_id, Some(500008))
        || group_id == 287
        || (755..=761).contains(&group_id)
        || (801..=805).contains(&group_id)
        || group_id == 1174
        || type_name.contains("Alvi")
        || type_name.contains("Alvus")
    {
        return "RogueDrone";
    }

    // 4. EDENCOM: EDENCOM faction (500027), group (4034), or group/type name
    if group_name.contains("EDENCOM")
        || type_name.contains("EDENCOM")
        || matches!(faction_id, Some(500027))
        || group_id == 4034
    {
        return "EDENCOM";
    }

    // 5. Incursion Sansha: Incursion Sansha groups (1051..=1056), or group/type name
    if group_name.contains("Incursion")
        || (1051..=1056).contains(&group_id)
        || type_name.starts_with("Outuni")
        || type_name.starts_with("Kupusam")
        || type_name.starts_with("Deltole")
    {
        return "IncursionSansha";
    }

    // 6. Officer / Commander: SDE group name containing Officer/Commander, meta_group_id, or meta_level >= 11
    if group_name.contains("Officer")
        || group_name.contains("Commander")
        || meta_group_id == Some(4) // Officer
        || meta_group_id == Some(5) // Deadspace
        || meta_level >= 11
    {
        return "Officer";
    }

    // 7. Drifters: Drifter faction (500024), groups (1310, 1530, 1788, 1956, 4035), or group/type name
    if group_name.contains("Drifter")
        || type_name.contains("Drifter")
        || matches!(faction_id, Some(500024))
        || group_id == 1310
        || group_id == 1530
        || group_id == 1788
        || group_id == 1956
        || group_id == 4035
    {
        return "Drifter";
    }

    // 8. Triglavians: Triglavian faction (500026), group (4028), or group/type name
    if group_name.contains("Triglavian")
        || type_name.contains("Triglavian")
        || matches!(faction_id, Some(500026))
        || group_id == 4028
        || type_name.contains("Damavik")
        || type_name.contains("Vedmak")
        || type_name.contains("Drekavac")
        || type_name.contains("Leshak")
        || type_name.contains("Raznaborg")
    {
        return "Triglavian";
    }

    // 9. Clone Soldier: type name or group name
    if type_name.contains("Clone Soldier") || group_name.contains("Clone Soldier") {
        return "CloneSoldier";
    }

    // 10. Hauler / Mining Hauler
    if group_name.contains("Hauler")
        || group_name.contains("Industrial")
        || type_name.contains("Hauler")
        || type_name.starts_with('♦')
        || group_id == 1767
    {
        return "Hauler";
    }

    // 11. Navy / Faction Police / Faction
    if group_name.contains("Faction Drone")
        || group_name.contains("Navy")
        || type_name.contains("Navy")
        || group_name.contains("Police")
        || type_name.contains("Police")
        || matches!(faction_id, Some(500001 | 500002 | 500003 | 500004))
    {
        return "Navy";
    }

    "Pirate"
}

/// Spawns an NPC combat entity from SDE, dynamically determining its archetype from SDE metadata.
pub async fn spawn_npc_entity_from_sde(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_type_id: u32,
    spawn_group_id: u32,
    pos: DVec3,
) -> i64 {
    spawn_specialized_npc_entity(ctx, solar_system_id, npc_type_id, "", spawn_group_id, pos).await
}

/// Spawns a specialized NPC combat entity from SDE (`feat:npc.drifters`, `feat:npc.triglavians`, `feat:npc.sleepers`,
/// `feat:npc.edencom-npc`, `feat:npc.rogue-drones`, `feat:npc.incursion-npc`, `feat:npc.navy-npc`,
/// `feat:npc.clone-soldier-spawns`, `feat:npc.officer-npc`, `feat:npc.npc-hauler`).
pub async fn spawn_specialized_npc_entity(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_type_id: u32,
    archetype_hint: &str,
    spawn_group_id: u32,
    pos: DVec3,
) -> i64 {
    let sde_derived = derive_npc_archetype(ctx, npc_type_id);
    let archetype = if !archetype_hint.is_empty()
        && (sde_derived == "Pirate" || matches!(archetype_hint, "FactionPolice" | "Police"))
    {
        archetype_hint
    } else {
        sde_derived
    };
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let ball_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let sde_type = ctx.sde.types.get(&npc_type_id);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    let max_vel = attr(37).unwrap_or(350.0);
    let opt_range = attr(54).unwrap_or(15_000.0).max(5_000.0);
    let orb_range = (opt_range * 0.6).clamp(2_500.0, 20_000.0);
    let bounty = match archetype {
        "Sleeper" => 0.0, // Sleepers pay 0 direct bounty and drop blue loot (`feat:npc.sleepers`)
        "Officer" => attr(481).unwrap_or(50_000_000.0).max(25_000_000.0),
        "CloneSoldier" => attr(481).unwrap_or(1_000_000.0).max(500_000.0),
        _ => attr(481).unwrap_or(120_000.0),
    };
    let dmg_mult = attr(64).unwrap_or(2.0).max(1.0);
    let raw_em = attr(114).unwrap_or(0.0);
    let raw_exp = attr(116).unwrap_or(0.0);
    let raw_kin = attr(117).unwrap_or(0.0);
    let raw_thm = attr(118).unwrap_or(0.0);
    let damage_vector = if raw_em + raw_exp + raw_kin + raw_thm > 0.0 {
        DamageVector {
            em: raw_em * dmg_mult,
            explosive: raw_exp * dmg_mult,
            kinetic: raw_kin * dmg_mult,
            thermal: raw_thm * dmg_mult,
        }
    } else {
        match archetype {
            "Triglavian" => DamageVector::new(0.0, 75.0, 0.0, 75.0),
            "EDENCOM" => DamageVector::new(60.0, 0.0, 60.0, 0.0),
            "Drifter" | "Sleeper" | "IncursionSansha" => DamageVector::new(50.0, 50.0, 50.0, 50.0),
            _ => DamageVector::new(25.0, 25.0, 25.0, 25.0),
        }
    };

    // Clone Soldier tier security tag mapping (`feat:npc.clone-soldier-spawns`)
    let tag_drop = if archetype == "CloneSoldier" {
        let name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
        if name.contains("Trainer") {
            Some(35661)
        } else if name.contains("Recruiter") {
            Some(35662)
        } else if name.contains("Transporter") {
            Some(35663)
        } else {
            Some(35664)
        }
    } else {
        None
    };

    let (web_factor, web_range, scram_str, scram_range) = if archetype == "CloneSoldier" {
        let name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
        if name.contains("Transporter") {
            (-55.0, 10_000.0, 0.0, 0.0) // Transporters web at 10 km
        } else if name.contains("Negotiator") {
            (0.0, 0.0, 2.0, 24_000.0) // Negotiators warp scram at 24 km
        } else {
            (attr(20).unwrap_or(0.0), attr(580).unwrap_or(10_000.0), 0.0, 0.0)
        }
    } else {
        (
            attr(20).unwrap_or(-50.0),
            attr(580).unwrap_or(10_000.0),
            attr(105).unwrap_or(2.0),
            attr(582).unwrap_or(20_000.0),
        )
    };

    let mut cs = ctx.build_type_combat_state(npc_type_id);
    if cs.max_structure_hp <= 0.0 {
        cs.max_shield_hp = 2_000.0;
        cs.shield_hp = 2_000.0;
        cs.max_armor_hp = 2_000.0;
        cs.armor_hp = 2_000.0;
        cs.max_structure_hp = 2_000.0;
        cs.structure_hp = 2_000.0;
    }
    if archetype == "PochvenHypnosianSleeper" {
        let name = sde_type.map(|t| t.name.as_str()).unwrap_or("");
        if name.contains("Aroused") || npc_type_id == 56680 || (56319..=56327).contains(&npc_type_id) {
            cs.max_shield_hp = 5_000.0;
            cs.shield_hp = 5_000.0;
        }
    }
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ball_id,
                type_id: npc_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia: 2.0,
                mass: 1_500_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;

    let wh_class = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|s| {
            ctx.sde
                .constellations
                .get(&s.constellation_id)
                .and_then(|c| c.wormhole_class_id)
                .or_else(|| {
                    ctx.sde
                        .regions
                        .get(&s.region_id)
                        .and_then(|r| r.wormhole_class_id)
                })
        })
        .unwrap_or(3) as u8;

    let entity = ExtendedNpcEntity {
        ball_id,
        type_id: npc_type_id,
        faction_id: sde_type.and_then(|t| t.faction_id).unwrap_or_else(|| match archetype {
            "Sleeper" | "PochvenHypnosianSleeper" => 500_021,
            "RogueDrone" => 500_008,
            "EDENCOM" => 500_027,
            "Drifter" => 500_024,
            "Triglavian" => 500_026,
            _ => 500_010,
        }),
        archetype: archetype.to_string(),
        solar_system_id,
        spawn_group_id,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: bounty,
        optimal_range: opt_range,
        orbit_range: orb_range,
        damage_vector,
        entropic_cycles: 0,
        drifter_overshield_hp: if archetype == "Drifter" { 4_000.0 } else { 0.0 },
        drifter_doomsday_fired: false,
        web_speed_factor: web_factor,
        web_range,
        scram_strength: scram_str,
        scram_range,
        neut_amount: attr(97).unwrap_or(120.0),
        ecm_jammed: false,
        remote_shield_rep: if matches!(archetype, "Sleeper" | "IncursionSansha") {
            250.0
        } else {
            attr(68).unwrap_or(0.0)
        },
        remote_armor_rep: if matches!(archetype, "Sleeper" | "Triglavian" | "IncursionSansha") {
            250.0
        } else {
            attr(84).unwrap_or(0.0)
        },
        remote_rep_range: 25_000.0,
        tag_drop_type_id: tag_drop,
        spawn_position: pos,
        sleeper_wh_class: wh_class,
    };
    with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.insert(ball_id, entity);
    });
    ball_id
}

/// Determines if a candidate ball is hostile to EDENCOM or matches primary target hostility/fleet (`NPC-AI-2`).
pub fn is_candidate_hostile_to_edencom(
    ctx: &ServerContext,
    npc: &ExtendedNpcEntity,
    cand_id: i64,
    target_id: i64,
) -> bool {
    let cand_char_opt = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == cand_id)
            .map(|c| (c.character_id, c.fleet_id))
    });
    if let Some((cid, cand_fleet_id)) = cand_char_opt {
        let standing = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&cid)
                .and_then(|c| c.standings.get(&(npc.faction_id as i64)).copied())
                .unwrap_or(0.0)
        });
        if standing < 0.0 {
            return true;
        }
        let target_fleet_id = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == target_id)
                .and_then(|c| c.fleet_id)
        });
        if let (Some(cfid), Some(tfid)) = (cand_fleet_id, target_fleet_id) {
            if cfid == tfid && cfid != 0 {
                return true;
            }
        }
        return false;
    }

    let drone_owner_opt = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.get(&cand_id).map(|d| d.owner_char_id)
    })
    .or_else(|| {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&cand_id)
                .and_then(|item| snap.characters.get(&item.owner_id))
                .map(|c| c.character_id)
        })
    });
    if let Some(owner_cid) = drone_owner_opt {
        let is_target_owner = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&owner_cid)
                .map(|c| c.active_ship_id == target_id)
                .unwrap_or(false)
        });
        if is_target_owner {
            return true;
        }
        let standing = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&owner_cid)
                .and_then(|c| c.standings.get(&(npc.faction_id as i64)).copied())
                .unwrap_or(0.0)
        });
        return standing < 0.0;
    }

    let other_npc_opt = with_combat_runtime(ctx, |rt| rt.npc_entities.get(&cand_id).cloned());
    if let Some(other_npc) = other_npc_opt {
        if matches!(
            other_npc.archetype.as_str(),
            "Triglavian" | "Sleeper" | "PochvenHypnosianSleeper" | "Drifter" | "RogueDrone" | "BeltRat" | "Officer" | "Pirate"
        ) {
            return true;
        }
        if matches!(other_npc.faction_id, 500026 | 500024 | 500021 | 500025 | 500008 | 500010..=500020) {
            return true;
        }
        return false;
    }

    false
}

/// Steps extended NPC AI (`feat:npc.npc-ai-targeting`, `feat:npc.npc-ewar-application`, `feat:npc.npc-remote-repair`,
/// `feat:npc.npc-target-switching`, `feat:npc.npc-standings-aggression`, `feat:npc.drifters`, `feat:npc.triglavians`,
/// `feat:npc.sleepers`, `feat:npc.edencom-npc`).
pub fn step_extended_npc_ai_sync(ctx: &ServerContext, solar_system_id: u32) {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let ball_map: HashMap<i64, Ball> = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .balls
        .iter()
        .map(|(&id, b)| (id, b.clone()))
        .collect();

    let npc_ids: Vec<i64> = with_combat_runtime(ctx, |rt| {
        rt.npc_entities
            .values()
            .filter(|n| n.solar_system_id == solar_system_id)
            .map(|n| n.ball_id)
            .collect()
    });

    for nid in npc_ids {
        let Some(mut npc) = with_combat_runtime(ctx, |rt| rt.npc_entities.get(&nid).cloned()) else {
            continue;
        };
        let Some(npc_ball) = ball_map.get(&nid) else {
            with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.remove(&nid);
            });
            continue;
        };

        let is_debugged = with_combat_runtime(ctx, |rt| {
            rt.behavior_debugging_active.contains(&nid)
                || rt.behavior_debugging_active.contains(&(solar_system_id as i64))
                || !rt.behavior_debugging_active.is_empty()
        });
        if is_debugged {
            info!(
                target: "ai::behavior",
                npc_id = nid,
                solar_system_id,
                archetype = %npc.archetype,
                target_id = ?npc.target_id,
                "NPC behavior decision execution tick"
            );
        }

        // 0. Sleeper spawn leash (`feat:npc.sleeper-capacitor-immunity-and-leash`):
        // Pulling a Sleeper > 200 km from its spawn position triggers immediate teleport back to spawn.
        if (npc.archetype == "Sleeper" || npc.archetype == "PochvenHypnosianSleeper")
            && npc_ball.position.distance(npc.spawn_position) > 200_000.0
        {
            sys_handle.try_set_ball_position(nid, npc.spawn_position);
            continue;
        }

        // 1. Remote Repair (`feat:npc.npc-remote-repair`, `feat:npc.sleepers`):
        // If not ECM-jammed, repair damaged fellow NPCs in the same `spawn_group_id` within `remote_rep_range`.
        if !npc.ecm_jammed && (npc.remote_shield_rep > 0.0 || npc.remote_armor_rep > 0.0) {
            let group_peers: Vec<i64> = with_combat_runtime(ctx, |rt| {
                rt.npc_entities
                    .values()
                    .filter(|p| {
                        p.solar_system_id == solar_system_id
                            && p.spawn_group_id == npc.spawn_group_id
                            && p.ball_id != nid
                    })
                    .map(|p| p.ball_id)
                    .collect()
            });
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            for pid in group_peers {
                if let Some(peer_ball) = ball_map.get(&pid) {
                    if npc_ball.position.distance(peer_ball.position) <= npc.remote_rep_range {
                        if let Some(pcs) = bp.combat_states.get_mut(&pid) {
                            if npc.remote_shield_rep > 0.0 && pcs.shield_hp < pcs.max_shield_hp {
                                pcs.shield_hp =
                                    (pcs.shield_hp + npc.remote_shield_rep).min(pcs.max_shield_hp);
                            }
                            if npc.remote_armor_rep > 0.0 && pcs.armor_hp < pcs.max_armor_hp {
                                pcs.armor_hp =
                                    (pcs.armor_hp + npc.remote_armor_rep).min(pcs.max_armor_hp);
                            }
                        }
                    }
                }
            }
        }

        // 2. Target Selection & Switching (`feat:npc.npc-target-switching`, `feat:npc.npc-standings-aggression`):
        for (&tid, &threat) in &npc.threat_table {
            npc.aggression_threat.entry(tid).or_insert(threat);
        }
        // Prune any targets in `aggression_threat` that are no longer on grid (> 150 km or removed from Ballpark).
        npc.aggression_threat.retain(|&tid, _| {
            ball_map
                .get(&tid)
                .is_some_and(|tb| tb.position.distance(npc_ball.position) <= 150_000.0)
        });

        // Add any hostile ships or active player drones within 75 km that pass standings aggression check
        for (&cand_id, cand_ball) in &ball_map {
            if cand_id == nid
                || cand_id >= 700_000_000
                || cand_ball.type_id == 26468
                || cand_ball.max_velocity <= 0.0
            {
                continue;
            }
            let is_other_npc = with_combat_runtime(ctx, |rt| rt.npc_entities.contains_key(&cand_id));
            if is_other_npc {
                continue;
            }
            let dist = npc_ball.position.distance(cand_ball.position);
            if dist > 75_000.0 {
                continue;
            }
            let is_player_drone = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.contains_key(&cand_id)
            }) || ctx.db.with_snapshot(|snap| {
                snap.items.get(&cand_id).is_some_and(|item| {
                    ctx.sde.types.get(&item.type_id).is_some_and(|t| {
                        t.category_id == 18 || matches!(t.group_id, 100 | 549 | 1023)
                    })
                })
            });
            // Standings and security status check (`feat:npc.npc-standings-aggression`, `feat:npc.navy-npc`, `F-01`):
            // Resolve owner character for ships and player drones
            let owner_char_opt = if is_player_drone {
                ctx.db.with_snapshot(|snap| {
                    snap.items
                        .get(&cand_id)
                        .and_then(|item| snap.characters.get(&item.owner_id))
                        .map(|c| c.character_id)
                }).or_else(|| {
                    with_combat_runtime(ctx, |rt| {
                        rt.active_drones_in_space.get(&cand_id).map(|d| d.owner_char_id)
                    })
                })
            } else {
                ctx.db.with_snapshot(|snap| {
                    snap.characters
                        .values()
                        .find(|c| c.active_ship_id == cand_id)
                        .map(|c| c.character_id)
                })
            };

            if let Some(cid) = owner_char_opt {
                let is_edencom = matches!(npc.archetype.as_str(), "EDENCOM" | "Edencom");
                let is_navy_or_police = matches!(npc.archetype.as_str(), "Navy" | "FactionPolice" | "Police")
                    || matches!(npc.faction_id, 500001..=500004);
                let default_standing = if is_edencom || is_navy_or_police { 0.0 } else { -5.0 };
                let min_standing = if is_edencom || is_navy_or_police { 0.0 } else { 5.0 };

                let (standing, is_low_sec_criminal) = ctx.db.with_snapshot(|snap| {
                    let char_rec = snap.characters.get(&cid);
                    let s = char_rec
                        .and_then(|c| c.standings.get(&(npc.faction_id as i64)).copied())
                        .unwrap_or(default_standing);
                    let sec = char_rec.map(|c| c.security_status).unwrap_or(0.0);
                    let sys_sec = ctx
                        .sde
                        .solar_systems
                        .get(&solar_system_id)
                        .map(|sys| sys.security_status)
                        .unwrap_or(0.0);
                    let sec_threshold = faction_police_sec_threshold(sys_sec).unwrap_or(-5.0);
                    let is_crim = sec <= sec_threshold || sec <= -5.0;
                    (s, is_crim)
                });
                let now_unix = ctx.now_unix();
                let has_criminal_flag = ctx.crimewatch.read().ok().and_then(|cw| {
                    cw.states.get(&cid).map(|st| st.is_criminal(now_unix))
                }).unwrap_or(false);
                let is_criminal = is_low_sec_criminal || has_criminal_flag;

                let owner_active_ship_id = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).map(|c| c.active_ship_id)
                });
                let owner_already_threat = owner_active_ship_id.is_some_and(|osid| {
                    npc.aggression_threat.contains_key(&osid) || npc.threat_table.contains_key(&osid)
                });

                if standing >= min_standing
                    && !(is_navy_or_police && is_criminal)
                    && !owner_already_threat
                    && !npc.aggression_threat.contains_key(&cand_id)
                    && !npc.threat_table.contains_key(&cand_id)
                {
                    continue;
                }
            } else if !npc.aggression_threat.contains_key(&cand_id)
                && !npc.threat_table.contains_key(&cand_id)
            {
                continue;
            }
            let is_capital = ctx.db.with_snapshot(|snap| {
                snap.items.get(&cand_id).and_then(|i| {
                    ctx.sde.types.get(&i.type_id).map(|t| matches!(t.group_id, 485 | 547 | 1538 | 30 | 659))
                }).unwrap_or(false)
            });
            let base_threat = if is_capital { 200.0 } else if is_player_drone { 120.0 } else { 100.0 };
            let entry = npc.aggression_threat.entry(cand_id).or_insert(base_threat);
            if is_capital && *entry < 200.0 {
                *entry = 200.0;
            }
        }

        npc.threat_table = npc.aggression_threat.clone();

        // Pick highest-threat valid target on grid (breaking equal-threat ties deterministically by lowest ball ID)
        let next_target = npc
            .aggression_threat
            .iter()
            .max_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.0.cmp(a.0))
            })
            .map(|(&tid, _)| tid);
        if npc.target_id != next_target {
            npc.entropic_cycles = 0;
        }
        npc.target_id = next_target;

        let Some(target_id) = npc.target_id else {
            with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.insert(nid, npc);
            });
            continue;
        };
        let Some(target_ball) = ball_map.get(&target_id) else {
            npc.target_id = None;
            with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.insert(nid, npc);
            });
            continue;
        };
        let dist = npc_ball.position.distance(target_ball.position);

        // 3. Apply NPC EWAR & 4. Archetype Weapon Application synchronously on Ballpark
        {
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            let mut st_guard = sys_handle.actor_state.write().unwrap_or_else(|e| e.into_inner());
            let st = &mut *st_guard;
            if npc.web_speed_factor < 0.0 && dist <= npc.web_range {
                let mult = (1.0 + npc.web_speed_factor / 100.0).clamp(0.1, 1.0);
                let _ = bp.set_speed_fraction(target_id, (target_ball.speed_fraction.max(1.0)) * mult);
            }
            let can_scram = if npc.archetype == "Sleeper" {
                npc.sleeper_wh_class >= 3
            } else {
                true
            };
            if can_scram && npc.scram_strength > 0.0 && dist <= npc.scram_range {
                st.base_scramble_strengths.insert(target_id, npc.scram_strength as i32);
                if let Some(tcs) = bp.combat_states.get_mut(&target_id) {
                    tcs.warp_scramble_strength =
                        tcs.warp_scramble_strength.max(npc.scram_strength as i32);
                    tcs.is_mwd_mjd_disabled = true;
                }
                crate::system_actor::reconcile_tackle_and_bubbles(
                    &mut bp,
                    &mut st.active_module_cycles,
                    &st.base_scramble_strengths,
                    &st.base_core_strengths,
                );
            }
            if npc.neut_amount > 0.0 && dist <= npc.optimal_range {
                if let Some(tcs) = bp.combat_states.get_mut(&target_id) {
                    tcs.capacitor = (tcs.capacitor - npc.neut_amount).max(0.0);
                }
            }

            let mut cycle_dmg = npc.damage_vector;
            if npc.archetype == "Triglavian" {
                let mult = 1.0 + (npc.entropic_cycles as f64 * 0.10).min(1.50);
                cycle_dmg = cycle_dmg.scaled(mult);
                npc.entropic_cycles = npc.entropic_cycles.saturating_add(1);
            } else if npc.archetype == "Drifter"
                && npc.drifter_overshield_hp <= 0.0
                && !npc.drifter_doomsday_fired
            {
                npc.drifter_doomsday_fired = true;
                cycle_dmg = DamageVector::new(2_500.0, 2_500.0, 2_500.0, 2_500.0);
            } else if npc.archetype.eq_ignore_ascii_case("Edencom") {
                let candidates: Vec<(i64, DVec3)> = ball_map
                    .iter()
                    .filter(|(&other_id, _)| {
                        if other_id == nid || other_id == target_id || other_id >= 700_000_000 {
                            return false;
                        }
                        if npc.threat_table.contains_key(&other_id) {
                            return true;
                        }
                        is_candidate_hostile_to_edencom(ctx, &npc, other_id, target_id)
                    })
                    .map(|(&other_id, other_ball)| (other_id, other_ball.position))
                    .collect();
                let hits = crate::formulas::vorton_arc_targets_with_cap(
                    target_id,
                    target_ball.position,
                    &candidates,
                    10_000.0,
                    5,
                );
                for arc_id in hits.into_iter().skip(1) {
                    bp.apply_damage_to_ball(arc_id, npc.damage_vector.scaled(0.5));
                }
            } else if npc.archetype == "Sleeper" {
                let is_cap = ctx.db.with_snapshot(|snap| {
                    snap.items.get(&target_id).and_then(|i| {
                        ctx.sde.types.get(&i.type_id).map(|t| matches!(t.group_id, 485 | 547 | 1538 | 30 | 659))
                    }).unwrap_or(false)
                });
                if is_cap {
                    cycle_dmg = cycle_dmg.scaled(2.5);
                }
            } else if npc.archetype == "ZarzakhGateGun" {
                bp.apply_ecm_jam(target_id, 15.0);
                bp.clear_target_locks(target_id);
            }
            bp.apply_damage_to_ball(target_id, cycle_dmg);
            crate::system_actor::sync_ball_positions(
                &bp,
                &sys_handle.ball_positions,
                &sys_handle.scrambled_balls,
            );
        }
        if let Some(target_char_id) = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == target_id)
                .map(|c| c.character_id)
        }) {
            crate::sp6_lifecycle::record_npc_aggression_timer(ctx, target_char_id, ctx.now_unix());
        }

        with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(nid, npc);
        });
    }
}

pub async fn step_extended_npc_ai(ctx: &ServerContext, solar_system_id: u32) {
    step_extended_npc_ai_sync(ctx, solar_system_id);
}

/// Queues an NPC bounty kill into the 20-minute (`1,200s`) bounty tick accumulator (`feat:combat.bounty-payout-npc`)
/// and drops archetype-specific loot (Clone Soldier tags, Officer modules, Sleeper blue loot, Hauler minerals).
pub async fn destroy_specialized_npc_and_queue_bounty(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_ball_id: i64,
    killer_char_id: i64,
) -> (i64, f64, Vec<u32>) {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let pos = sys_handle
        .get_ball_snapshot(npc_ball_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);
    let _ = sys_handle.remove_ball(npc_ball_id).await;

    let npc_opt = with_mut_combat_runtime(ctx, |rt| rt.npc_entities.remove(&npc_ball_id));
    let (archetype, bounty_isk, tag_drop_opt, npc_type_id, faction_id) = match npc_opt {
        Some(n) => (n.archetype, n.bounty_isk, n.tag_drop_type_id, Some(n.type_id), n.faction_id),
        None => {
            let tid = ctx.db.with_snapshot(|snap| snap.items.get(&npc_ball_id).map(|i| i.type_id));
            ("BeltRat".to_string(), 50_000.0, None, tid, 0)
        }
    };

    if bounty_isk > 0.0 {
        with_mut_combat_runtime(ctx, |rt| {
            *rt.pending_bounty_ticks
                .entry((solar_system_id, killer_char_id))
                .or_insert(0.0) += bounty_isk;
        });
    }

    let wreck_type_id = if archetype == "Sleeper" {
        30495 // Sleeper Large Intermediate Wreck (`attr 901 = -15.0`)
    } else if archetype == "Drifter" {
        37531 // Drifter Response Battleship Wreck (`attr 901 = -20.0`)
    } else {
        26473 // Cruiser Wreck
    };

    let mut dropped_types = Vec::new();
    if let Some(tag_tid) = tag_drop_opt {
        dropped_types.push(tag_tid);
    }

    // Query SDE type materials and dogma attributes
    if let Some(tid) = npc_type_id {
        if let Some(mats) = ctx.sde.type_materials.get(&tid) {
            for m in mats {
                if !dropped_types.contains(&m.material_type_id) {
                    dropped_types.push(m.material_type_id);
                }
            }
        }
    }

    let sde_type = npc_type_id.and_then(|tid| ctx.sde.types.get(&tid));
    let effective_faction = sde_type
        .and_then(|t| t.faction_id)
        .or(if faction_id > 0 { Some(faction_id) } else { None });

    // Derive salvage and loot dynamically from SDE queries (ctx.sde.types, ctx.sde.groups, ctx.sde.npc_factions)
    match archetype.as_str() {
        "PochvenHypnosianSleeper" => {
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.name == "Interstellar Navigation Logs") {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Sleeper" => {
            let comp_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Sleeper Components")
                .map(|(&gid, _)| gid)
                .unwrap_or(880);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                t.group_id == comp_gid && (id == 30744 || t.name == "Neural Network Analyzer")
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
            let anc_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Ancient Salvage")
                .map(|(&gid, _)| gid)
                .unwrap_or(966);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                t.group_id == anc_gid && (id == 30259 || t.name == "Melted Nanoribbons")
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Officer" => {
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 14254 || (t.group_id == 52 && t.name.contains("Cormack"))
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Hauler" => {
            for min_name in &["Tritanium", "Pyerite"] {
                if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.name == *min_name) {
                    if !dropped_types.contains(&tid) {
                        dropped_types.push(tid);
                    }
                }
            }
        }
        "RogueDrone" => {
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 11732 || t.name.contains("Drone Alloy") || t.name == "Sheen Compound"
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "Triglavian" => {
            let data_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Triglavian Data")
                .map(|(&gid, _)| gid)
                .unwrap_or(1995);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 48121 || t.name == "Triglavian Survey Database" || (t.group_id == data_gid && t.name.contains("Survey Database"))
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
            let mat_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Abyssal Materials")
                .map(|(&gid, _)| gid)
                .unwrap_or(1996);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(&id, t)| {
                id == 48112 || t.name == "Zero-Point Condensate" || (t.group_id == mat_gid && t.name.contains("Zero-Point"))
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        "EDENCOM" | "Edencom" => {
            let salvage_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Salvaged Materials")
                .map(|(&gid, _)| gid)
                .unwrap_or(754);
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| {
                t.group_id == salvage_gid && t.name == "Alloyed Tritanium Bar"
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
        _ => {
            let salvage_gid = ctx
                .sde
                .groups
                .iter()
                .find(|(_, g)| g.name == "Salvaged Materials")
                .map(|(&gid, _)| gid)
                .unwrap_or(754);
            let faction_name = effective_faction
                .and_then(|fid| ctx.sde.npc_factions.get(&fid).map(|f| f.name.as_str()))
                .unwrap_or("");
            let salvage_name = if faction_name.contains("Caldari") || faction_name.contains("Guristas") {
                "Alloyed Tritanium Bar"
            } else if faction_name.contains("Minmatar") || faction_name.contains("Angel") {
                "Armor Plates"
            } else if faction_name.contains("Amarr") || faction_name.contains("Blood") || faction_name.contains("Sansha") {
                "Burned Logic Circuit"
            } else if faction_name.contains("Gallente") || faction_name.contains("Serpentis") {
                "Artificial Neural Network"
            } else {
                "Metal Scraps"
            };
            if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| {
                (t.group_id == salvage_gid && t.name == salvage_name) || t.name == salvage_name
            }) {
                if !dropped_types.contains(&tid) {
                    dropped_types.push(tid);
                }
            }
        }
    }

    let now_unix = ctx.now_unix();
    let wreck_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let wid = snap.next_entity_id;
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wid, now_unix, true);
        snap.items.insert(
            wid,
            InventoryItemRecord {
                item_id: wid,
                type_id: wreck_type_id,
                owner_id: killer_char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(wid, [pos.x, pos.y, pos.z]);
        for &ltid in &dropped_types {
            snap.next_entity_id += 1;
            let lid = snap.next_entity_id;
            snap.items.insert(
                lid,
                InventoryItemRecord {
                    item_id: lid,
                    type_id: ltid,
                    owner_id: killer_char_id,
                    location_id: wid,
                    flag: InventoryFlag::Cargo,
                    quantity: if ltid == 34 || ltid == 35 { 50_000 } else { 1 },
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        wid
    });

    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: wreck_id,
                type_id: wreck_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(wreck_type_id),
        )
        .await;
    crate::services::combat_crimewatch::combat_wrecks_killmails::register_container_lifetime(
        ctx,
        wreck_id,
        solar_system_id,
        wreck_type_id,
        killer_char_id,
        now_unix,
        false,
    );

    // Check if `npc_ball_id` was an encounter wave trigger (`feat:npc.npc-spawn-encounter`)
    step_encounter_wave_on_kill(ctx, solar_system_id, npc_ball_id, pos).await;

    if killer_char_id > 0 {
        ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(
            killer_char_id as u32,
            1,
            1,
            &ctx.quasar_gateway,
        );
        ctx.quasar_gateway.progression.career_goals.on_gameplay_event(
            killer_char_id as u32,
            "combat",
            1,
            &ctx.quasar_gateway,
        );
    }

    (wreck_id, bounty_isk, dropped_types)
}

/// Flushes the 20-minute (`1,200s`) batched NPC bounty tick for `(solar_system_id, char_id)`
/// applying the system's Dynamic Bounty System (`dbs_multiplier`), corporation tax, and K-space null-sec ESS split (`feat:combat.bounty-payout-npc`).
pub fn flush_npc_bounty_tick(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id: i64,
    corp_tax_rate: f64,
) -> (f64, f64, f64) {
    let raw_bounty = with_mut_combat_runtime(ctx, |rt| {
        rt.pending_bounty_ticks
            .remove(&(solar_system_id, char_id))
            .unwrap_or(0.0)
    });
    if raw_bounty <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let is_kspace_nullsec = sys_sec <= 0.0 && solar_system_id < 31_000_000;
    let dbs_mult = ctx
        .db
        .with_snapshot(|s| {
            s.system_states
                .get(&solar_system_id)
                .map(|st| st.dbs_multiplier)
        })
        .unwrap_or(1.35);

    let (pilot_isk, main_ess, reserve_ess) = if is_kspace_nullsec {
        let (p, _tax, m, r) =
            compute_dbs_and_ess_bounty_split(raw_bounty, dbs_mult, corp_tax_rate);
        (p, m, r)
    } else {
        let modified = raw_bounty * dbs_mult;
        (modified * (1.0 - corp_tax_rate.clamp(0.0, 1.0)), 0.0, 0.0)
    };

    ctx.db.with_mut_snapshot(|snap| {
        let pilot_cents = IskCents::from_f64_isk(pilot_isk);
        let main_cents = IskCents::from_f64_isk(main_ess);
        let reserve_cents = IskCents::from_f64_isk(reserve_ess);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = ch.balance.checked_add(pilot_cents).unwrap_or(ch.balance);
        }
        if is_kspace_nullsec {
            if let Some(st) = snap.system_states.get_mut(&solar_system_id) {
                st.ess_main_bank = st
                    .ess_main_bank
                    .checked_add(main_cents)
                    .unwrap_or(st.ess_main_bank);
                st.ess_reserve_bank = st
                    .ess_reserve_bank
                    .checked_add(reserve_cents)
                    .unwrap_or(st.ess_reserve_bank);
            }
        }
        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: jid,
            character_id: char_id,
            entry_type: "bounty_prizes".to_string(),
            amount: pilot_cents,
            balance_after: snap
                .characters
                .get(&char_id)
                .map(|c| c.balance)
                .unwrap_or(pilot_cents),
            reason: format!("20m Bounty Tick in system {solar_system_id} (DBS {dbs_mult:.2}x)"),
        });
    });

    (pilot_isk, main_ess, reserve_ess)
}

/// Initializes a multi-wave NPC encounter site where killing the trigger NPC spawns the next wave (`feat:npc.npc-spawn-encounter`).
pub async fn create_encounter_wave_site(
    ctx: &ServerContext,
    encounter_id: u32,
    solar_system_id: u32,
    npc_type_id: u32,
    max_waves: u32,
    pos: DVec3,
) -> Vec<i64> {
    let b1 = spawn_specialized_npc_entity(
        ctx,
        solar_system_id,
        npc_type_id,
        "BeltRat",
        encounter_id,
        pos,
    )
    .await;
    let b2 = spawn_specialized_npc_entity(
        ctx,
        solar_system_id,
        npc_type_id,
        "BeltRat",
        encounter_id,
        pos + DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    let wave_balls = vec![b1, b2];
    with_mut_combat_runtime(ctx, |rt| {
        rt.encounter_waves.insert(
            encounter_id,
            EncounterWaveDirector {
                encounter_id,
                solar_system_id,
                current_wave: 1,
                max_waves: max_waves.max(1),
                trigger_ball_id: b1,
                npc_type_id,
                active_wave_ball_ids: wave_balls.clone(),
                completed: false,
            },
        );
    });
    wave_balls
}

async fn step_encounter_wave_on_kill(
    ctx: &ServerContext,
    solar_system_id: u32,
    killed_ball_id: i64,
    pos: DVec3,
) {
    let triggered_enc: Option<(u32, u32, u32, u32)> = with_mut_combat_runtime(ctx, |rt| {
        let mut found = None;
        for enc in rt.encounter_waves.values_mut() {
            if enc.solar_system_id == solar_system_id && !enc.completed {
                enc.active_wave_ball_ids.retain(|&id| id != killed_ball_id);
                if killed_ball_id == enc.trigger_ball_id || enc.active_wave_ball_ids.is_empty() {
                    if enc.current_wave < enc.max_waves {
                        enc.current_wave += 1;
                        found = Some((
                            enc.encounter_id,
                            enc.current_wave,
                            enc.max_waves,
                            enc.npc_type_id,
                        ));
                    } else if enc.active_wave_ball_ids.is_empty() {
                        enc.completed = true;
                    }
                }
            }
        }
        found
    });

    if let Some((enc_id, _next_wave, _max_w, npc_tid)) = triggered_enc {
        let nb1 = spawn_specialized_npc_entity(
            ctx,
            solar_system_id,
            npc_tid,
            "BeltRat",
            enc_id,
            pos + DVec3::new(500.0, 0.0, 0.0),
        )
        .await;
        let nb2 = spawn_specialized_npc_entity(
            ctx,
            solar_system_id,
            npc_tid,
            "BeltRat",
            enc_id,
            pos + DVec3::new(1_500.0, 0.0, 0.0),
        )
        .await;
        with_mut_combat_runtime(ctx, |rt| {
            if let Some(enc) = rt.encounter_waves.get_mut(&enc_id) {
                enc.trigger_ball_id = nb1;
                enc.active_wave_ball_ids.extend([nb1, nb2]);
            }
        });
    }
}
