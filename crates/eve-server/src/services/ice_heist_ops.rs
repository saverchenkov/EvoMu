//! `ice_heist_ops` — Pirate Insurgency Ice Refinery Heist lifecycle (`feat:pve.insurgency-ice-heist`).
//!
//! - Spawns only in systems at corruption stage `3..=4`, with a 4-hour (`14_400`s) respawn
//!   cooldown persisted in `db` across downtime.
//! - Spawns web-immune tanker NPCs (`is_web_immune = true`, SDE `79909`) at the refinery and
//!   Upwell Security Thunderchild guards (SDE `79986`).
//! - Escorting tankers within `15,000` m steers them into the escorting side's scoring zone,
//!   incrementing `pirate_tankers_saved` / `empire_tankers_saved`; the first side to `10` tankers
//!   wins `150,000` LP for aligned militia pilots in system.
//! - Pirates within `25,000` m of the Upwell Thunderchild guards are destroyed.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::insurgency_services::{
    read_insurgency_runtime, with_insurgency_runtime, FACTION_ANGEL_CARTEL, FACTION_GURISTAS,
};
use crate::services::pve_incursion_sites::make_site_npc_ball;
use eve_db::PersistedIceHeistRecord as IceHeistRecord;
use eve_destiny::{CombatState, DVec3};
use eve_proto::{BridgeRpcError, PyRep};

pub const ICE_HEIST_RESPAWN_COOLDOWN_SECS: u64 = 14_400; // 4 hours
pub const ICE_HEIST_WIN_TANKERS: u32 = 10;
pub const ICE_HEIST_WIN_LP: i64 = 150_000;
pub const THUNDERCHILD_GUARD_TYPE_ID: u32 = 79_986; // Upwell Security Thunderchild
pub const ICE_TANKER_TYPE_ID: u32 = 79_909; // Automated HISC-99 Tanker
const THUNDERCHILD_GUARD_RANGE_M: f64 = 25_000.0;
pub const ICE_HEIST_REFINERY_POS: DVec3 = DVec3::new(150_000.0, 0.0, 150_000.0);
pub const ICE_HEIST_EMPIRE_ZONE: DVec3 = DVec3::new(162_000.0, 0.0, 150_000.0);
pub const ICE_HEIST_PIRATE_ZONE: DVec3 = DVec3::new(138_000.0, 0.0, 150_000.0);

fn militia_corp_for_faction(war_faction_id: Option<i64>) -> Option<i64> {
    match war_faction_id {
        Some(500_001) => Some(1_000_180),              // State Protectorate
        Some(500_002) => Some(1_000_182),              // Tribal Liberation Force
        Some(500_003) => Some(1_000_179),              // 24th Imperial Crusade
        Some(500_004) => Some(1_000_181),              // Federal Defense Union
        Some(FACTION_ANGEL_CARTEL) => Some(1_000_436), // Malakim Zealots
        Some(FACTION_GURISTAS) => Some(1_000_437),     // Commando Guri
        _ => None,
    }
}

fn is_winning_side_faction(war_faction_id: Option<i64>, pirate_side: bool) -> bool {
    if pirate_side {
        matches!(
            war_faction_id,
            Some(FACTION_ANGEL_CARTEL | FACTION_GURISTAS)
        )
    } else {
        matches!(war_faction_id, Some(500_001..=500_004))
    }
}

fn respawn_key(sys_id: u32) -> String {
    format!("ice_heist_respawn_until:{sys_id}")
}

/// Spawns an Ice Refinery Heist instance if corruption is 3–4 and the 4h respawn cooldown has elapsed.
pub async fn spawn_ice_heist_checked(
    ctx: &ServerContext,
    sys_id: u32,
) -> Result<i64, BridgeRpcError> {
    let stage = read_insurgency_runtime(ctx, |rt| {
        let pts = rt
            .campaigns
            .values()
            .find_map(|c| c.corruption_points.get(&sys_id).copied())
            .unwrap_or(0);
        super::insurgency_services::insurgency_stage(pts)
    });
    if !(3..=4).contains(&stage) {
        return Err(BridgeRpcError {
            code: "CorruptionStageOutOfRange".into(),
            message: format!(
                "Ice Refinery Heist only spawns at corruption stage 3-4 (current stage: {stage})"
            ),
        });
    }

    let now_unix = ctx.now_unix_secs();
    let respawn_until = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get(&respawn_key(sys_id))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    });
    if now_unix < respawn_until {
        return Err(BridgeRpcError {
            code: "IceHeistRespawnCooldown".into(),
            message: format!(
                "Ice Refinery Heist in system {sys_id} is on 4-hour respawn cooldown until {respawn_until}"
            ),
        });
    }

    let (instance_id, newly_created) = with_insurgency_runtime(ctx, |rt| {
        if let Some(existing) = rt
            .ice_heist_instances
            .iter()
            .find(|h| h.solar_system_id == sys_id && !h.completed)
        {
            return (existing.instance_id, false);
        }
        let id = (rt.ice_heist_instances.len() as i64) + 800_001;
        rt.ice_heist_instances.push(IceHeistRecord {
            instance_id: id,
            solar_system_id: sys_id,
            pirate_tankers_saved: 0,
            empire_tankers_saved: 0,
            completed: false,
        });
        (id, true)
    });

    if !newly_created {
        return Ok(instance_id);
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let tanker_id = 1_896_000_000 + instance_id * 10 + 1;
    let guard_id = 1_896_000_000 + instance_id * 10 + 2;

    let mut tanker_cs = CombatState::default();
    tanker_cs.shield_hp = 25_000.0;
    tanker_cs.max_shield_hp = 25_000.0;
    tanker_cs.armor_hp = 25_000.0;
    tanker_cs.max_armor_hp = 25_000.0;
    tanker_cs.structure_hp = 30_000.0;
    tanker_cs.max_structure_hp = 30_000.0;
    tanker_cs.warp_core_strength = 100;
    tanker_cs.is_web_immune = true;
    let mut tanker_ball = make_site_npc_ball(tanker_id, ICE_TANKER_TYPE_ID, ICE_HEIST_REFINERY_POS);
    tanker_ball.max_velocity = 2_500.0;
    tanker_ball.inertia = 0.25;
    tanker_ball.mass = 1_000_000.0;
    tanker_ball.speed_fraction = 0.0;
    let _ = sys.spawn_ball_with_combat(tanker_ball, tanker_cs).await;

    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(
                guard_id,
                THUNDERCHILD_GUARD_TYPE_ID,
                ICE_HEIST_REFINERY_POS + DVec3::new(0.0, 0.0, 40_000.0),
            ),
            ctx.build_type_combat_state(THUNDERCHILD_GUARD_TYPE_ID),
        )
        .await;

    Ok(instance_id)
}

/// Scores a tanker arrival in `pirate_side` or empire scoring zone, respawns the next refinery
/// tanker until `10` are saved, and awards `150,000` LP + 4h respawn cooldown to aligned pilots.
pub async fn escort_ice_heist_tanker(
    ctx: &ServerContext,
    _char_id: i64,
    instance_id: i64,
    pirate_side: bool,
) -> Result<PyRep, BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let (sys_id, pirate_saved, empire_saved, completed, just_won) =
        with_insurgency_runtime(ctx, |rt| {
            let Some(inst) = rt
                .ice_heist_instances
                .iter_mut()
                .find(|h| h.instance_id == instance_id)
            else {
                return Err(BridgeRpcError {
                    code: "InstanceNotFound".into(),
                    message: format!("Ice Heist instance {instance_id} not found"),
                });
            };
            if inst.completed {
                return Err(BridgeRpcError {
                    code: "InstanceAlreadyCompleted".into(),
                    message: format!("Ice Heist instance {instance_id} is already completed"),
                });
            }
            if pirate_side {
                inst.pirate_tankers_saved += 1;
            } else {
                inst.empire_tankers_saved += 1;
            }
            let won = inst.pirate_tankers_saved >= ICE_HEIST_WIN_TANKERS
                || inst.empire_tankers_saved >= ICE_HEIST_WIN_TANKERS;
            if won {
                inst.completed = true;
            }
            Ok((
                inst.solar_system_id,
                inst.pirate_tankers_saved,
                inst.empire_tankers_saved,
                inst.completed,
                won,
            ))
        })?;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let tanker_id = 1_896_000_000 + instance_id * 10 + 1;
    let guard_id = 1_896_000_000 + instance_id * 10 + 2;
    if completed {
        let _ = sys.remove_ball(tanker_id).await;
        let _ = sys.remove_ball(guard_id).await;
    } else {
        let _ = sys.set_ball_position(tanker_id, ICE_HEIST_REFINERY_POS).await;
        let _ = sys.set_movement_mode(tanker_id, eve_destiny::MovementMode::Stop).await;
    }

    let all_balls = sys.list_balls().await.unwrap_or_default();
    let mut lp_awarded = 0_i64;
    if just_won {
        let on_grid_ship_ids: std::collections::HashSet<i64> = all_balls
            .iter()
            .filter(|b| b.position.distance(ICE_HEIST_REFINERY_POS) <= 250_000.0)
            .map(|b| b.id)
            .collect();
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(
                respawn_key(sys_id),
                (now_unix + ICE_HEIST_RESPAWN_COOLDOWN_SECS).to_string(),
            );
            let eligible_pids: Vec<i64> = snap
                .characters
                .values()
                .filter(|c| {
                    c.solar_system_id == sys_id
                        && c.station_id.is_none()
                        && c.active_ship_id > 0
                        && on_grid_ship_ids.contains(&c.active_ship_id)
                        && is_winning_side_faction(c.war_faction_id, pirate_side)
                })
                .map(|c| c.character_id)
                .collect();
            for pid in eligible_pids {
                if let Some(ch) = snap.characters.get_mut(&pid) {
                    if let Some(corp_id) = militia_corp_for_faction(ch.war_faction_id) {
                        *ch.loyalty_points.entry(corp_id).or_insert(0) += ICE_HEIST_WIN_LP;
                        lp_awarded = ICE_HEIST_WIN_LP;
                    }
                }
            }
        });
    }

    Ok(PyRep::keyval([
        ("instanceID", PyRep::Integer(instance_id)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("pirateTankersSaved", PyRep::Integer(i64::from(pirate_saved))),
        ("empireTankersSaved", PyRep::Integer(i64::from(empire_saved))),
        ("completed", PyRep::Bool(completed)),
        ("lpAwarded", PyRep::Integer(lp_awarded)),
    ]))
}

/// Drives Pirate Insurgency Ice Refinery Heist spawns, tanker escort movement/scoring,
/// and Upwell Thunderchild guard pirate interception on the 1 Hz system tick.
pub async fn step_ice_heist_system(ctx: &ServerContext, sys_id: u32) {
    let has_active = read_insurgency_runtime(ctx, |rt| {
        rt.ice_heist_instances
            .iter()
            .any(|h| h.solar_system_id == sys_id && !h.completed)
    });
    if !has_active {
        let _ = spawn_ice_heist_checked(ctx, sys_id).await;
    }

    let Some(instance_id) = read_insurgency_runtime(ctx, |rt| {
        rt.ice_heist_instances
            .iter()
            .find(|h| h.solar_system_id == sys_id && !h.completed)
            .map(|h| h.instance_id)
    }) else {
        return;
    };

    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let tanker_id = 1_896_000_000 + instance_id * 10 + 1;
    let guard_id = 1_896_000_000 + instance_id * 10 + 2;
    let now_unix = ctx.now_unix_secs();

    if let Some(guard_ball) = all_balls.iter().find(|b| b.id == guard_id) {
        let guard_pos = guard_ball.position;
        for b in &all_balls {
            if b.id == guard_id || (b.position - guard_pos).length() > THUNDERCHILD_GUARD_RANGE_M {
                continue;
            }
            let is_pirate_ship = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == b.id && c.station_id.is_none())
                    .and_then(|c| c.war_faction_id)
                    .is_some_and(|wf| wf == FACTION_ANGEL_CARTEL || wf == FACTION_GURISTAS)
            });
            if is_pirate_ship {
                crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    0,
                    b.id,
                    b.type_id,
                    b.position,
                    now_unix,
                )
                .await;
            }
        }
    }

    if let Some(tanker_ball) = all_balls.iter().find(|b| b.id == tanker_id) {
        let tpos = tanker_ball.position;
        if tpos.distance(ICE_HEIST_EMPIRE_ZONE) <= 5_000.0 {
            let _ = escort_ice_heist_tanker(ctx, 0, instance_id, false).await;
        } else if tpos.distance(ICE_HEIST_PIRATE_ZONE) <= 5_000.0 {
            let _ = escort_ice_heist_tanker(ctx, 0, instance_id, true).await;
        } else {
            let mut nearest_pirate = f64::INFINITY;
            let mut nearest_empire = f64::INFINITY;
            ctx.db.with_snapshot(|snap| {
                for b in &all_balls {
                    let dist = b.position.distance(tpos);
                    if dist > 15_000.0 {
                        continue;
                    }
                    if let Some(wf) = snap
                        .characters
                        .values()
                        .find(|c| c.active_ship_id == b.id && c.station_id.is_none())
                        .and_then(|c| c.war_faction_id)
                    {
                        if matches!(wf, FACTION_ANGEL_CARTEL | FACTION_GURISTAS) {
                            nearest_pirate = nearest_pirate.min(dist);
                        } else if matches!(wf, 500_001..=500_004) {
                            nearest_empire = nearest_empire.min(dist);
                        }
                    }
                }
            });
            if nearest_pirate.is_finite() || nearest_empire.is_finite() {
                let dest = if nearest_pirate < nearest_empire {
                    ICE_HEIST_PIRATE_ZONE
                } else {
                    ICE_HEIST_EMPIRE_ZONE
                };
                let _ = sys.goto_point(tanker_id, dest).await;
            } else {
                let _ = sys.set_movement_mode(tanker_id, eve_destiny::MovementMode::Stop).await;
            }
        }
    }
}
