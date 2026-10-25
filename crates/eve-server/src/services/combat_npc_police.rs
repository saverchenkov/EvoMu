//! `combat_npc_police` — High-security Faction Police responses to outlaws and criminal pilots (`feat:npc.navy-npc`, `feat:crimewatch.faction-police`).
//!
//! Below security threshold (<= -5.0) in high-sec (system security >= 0.45):
//! - Faction Police NPC ships spawn on grid targeting the outlaw ship.
//! - Docking is refused to outlaw ships.
//! - Pods/capsules at <= -5.0 are NEVER engaged by Faction Police, and capsules are allowed to dock.
//! - Pilots with security status > -5.0 (and standing > -5.0) are NOT engaged.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity;
use crate::services::combat_crimewatch::crimewatch_timers::faction_police_sec_threshold;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use eve_destiny::DamageVector;
use eve_sde::SolarSystemId;
use glam::DVec3;

/// Resolves the authentic Faction Police ship type ID for a given empire faction.
pub fn police_ship_type_for_faction(faction_id: u32) -> u32 {
    match faction_id {
        500_001 => 9970,  // Caldari Police 3rd Lieutenant
        500_004 => 9984,  // Gallente Police Captain
        500_003 => 3768,  // Amarr Police Frigate
        500_002 => 10046, // Minmatar Police / Navy Captain
        _ => 9970,
    }
}

/// Evaluates whether an outlaw ship in highsec is refused docking permissions (`feat:npc.navy-npc`).
pub fn is_outlaw_ship_docking_refused(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    char_id: i64,
    ship_id: i64,
) -> bool {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec < 0.45 {
        return false;
    }
    let (char_sec, is_capsule) = ctx.db.with_snapshot(|snap| {
        let sec = snap.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0);
        let is_pod = snap.items.get(&ship_id).map(|i| {
            i.type_id == 670 || ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 29).unwrap_or(false)
        }).unwrap_or(false);
        (sec, is_pod)
    });
    if is_capsule {
        return false; // Capsules are allowed to dock
    }
    let threshold = faction_police_sec_threshold(sys_sec).unwrap_or(-5.0);
    char_sec <= threshold || char_sec <= -5.0
}

/// Checks the ballpark for outlaw pilots (sec status <= -5.0) in high-sec ships and spawns Faction Police (`feat:npc.navy-npc`).
pub async fn check_and_spawn_faction_police(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> Vec<i64> {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec < 0.45 {
        return Vec::new();
    }
    let threshold = faction_police_sec_threshold(sys_sec).unwrap_or(-5.0);
    let sys_faction = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            ctx.sde
                .constellations
                .get(&sys.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| ctx.sde.regions.get(&sys.region_id).and_then(|r| r.faction_id))
        })
        .unwrap_or(500_001);
    let police_type = police_ship_type_for_faction(sys_faction);

    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let ball_ids = handle
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|b| b.id)
        .collect::<Vec<_>>();

    let mut spawned_police = Vec::new();

    for bid in ball_ids {
        let snap_opt = handle.get_ball_snapshot(bid).await.ok().flatten();
        let Some((_, ball)) = snap_opt else { continue; };

        // Exclude capsules (type 670 / group 29) and non-ships
        if ball.type_id == 670 || ctx.sde.types.get(&ball.type_id).map(|t| t.group_id == 29).unwrap_or(false) {
            continue;
        }

        // Find character owner
        let owner_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == bid)
                .map(|c| (c.character_id, c.security_status))
        });
        let Some((_char_id, sec_status)) = owner_char_opt else { continue; };

        // Only engage outlaws below threshold or <= -5.0
        if sec_status > threshold && sec_status > -5.0 {
            continue;
        }

        // Check if Faction Police already targets this ship on grid
        let already_targeted = with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.values().any(|n| {
                n.archetype == "FactionPolice"
                    && n.solar_system_id == solar_system_id
                    && (n.target_id == Some(bid) || n.aggression_threat.contains_key(&bid))
            })
        });
        if already_targeted {
            continue;
        }

        // Break gate cloak on outlaw
        let _ = handle.break_gate_cloak(bid).await;

        // Spawn Faction Police NPC ship on grid 3,000m from target
        let police_pos = ball.position + DVec3::new(3_000.0, 0.0, 0.0);
        let police_ball_id = spawn_specialized_npc_entity(
            ctx,
            solar_system_id,
            police_type,
            "FactionPolice",
            sys_faction,
            police_pos,
        )
        .await;

        // Configure police combat properties and lock target
        with_mut_combat_runtime(ctx, |rt| {
            if let Some(npc) = rt.npc_entities.get_mut(&police_ball_id) {
                npc.faction_id = sys_faction;
                npc.target_id = Some(bid);
                npc.aggression_threat.insert(bid, 500.0);
                npc.threat_table.insert(bid, 500.0);
                npc.optimal_range = 25_000.0;
                npc.orbit_range = 5_000.0;
                npc.damage_vector = DamageVector {
                    em: 50.0,
                    thermal: 50.0,
                    kinetic: 50.0,
                    explosive: 50.0,
                };
                npc.web_speed_factor = 0.50;
                npc.web_range = 15_000.0;
                npc.scram_strength = 2.0;
                npc.scram_range = 15_000.0;
            }
        });

        // Add ballpark lock
        let _ = handle
            .add_target_lock(
                police_ball_id,
                eve_destiny::TargetLock {
                    target_id: bid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;

        spawned_police.push(police_ball_id);
    }

    spawned_police
}
