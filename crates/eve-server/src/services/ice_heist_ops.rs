//! `ice_heist_ops` — Pirate Insurgency Ice Refinery Heist lifecycle (`feat:pve.insurgency-ice-heist`).
//!
//! - Spawns only in systems at corruption stage `3..=4`, with a 4-hour (`14_400`s) respawn
//!   cooldown persisted in `db` across downtime.
//! - Spawns web-immune tanker NPCs (`is_ewar_immune = true`) at the refinery and Upwell Thunderchild guards.
//! - Escorting tankers to each side's scoring zone increments `pirate_tankers_saved` / `empire_tankers_saved`;
//!   the first side to `10` tankers wins `150,000` LP (persisted in `CharacterRecord::loyalty_points`).
//! - Pirates within `25,000` m of the Upwell Thunderchild guards are destroyed.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::insurgency_services::{
    read_insurgency_runtime, with_insurgency_runtime, FACTION_ANGEL_CARTEL, FACTION_GURISTAS,
};
use crate::services::pve_incursion_sites::make_site_npc_ball;
use eve_db::PersistedIceHeistRecord as IceHeistRecord;
use eve_destiny::{CombatState, DVec3};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub const ICE_HEIST_RESPAWN_COOLDOWN_SECS: u64 = 14_400; // 4 hours
pub const ICE_HEIST_WIN_TANKERS: u32 = 10;
pub const ICE_HEIST_WIN_LP: i64 = 150_000;
pub const THUNDERCHILD_GUARD_TYPE_ID: u32 = 54732;
pub const ICE_TANKER_TYPE_ID: u32 = 32811;
const THUNDERCHILD_GUARD_RANGE_M: f64 = 25_000.0;

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

    let instance_id = with_insurgency_runtime(ctx, |rt| {
        if let Some(existing) = rt
            .ice_heist_instances
            .iter()
            .find(|h| h.solar_system_id == sys_id && !h.completed)
        {
            return existing.instance_id;
        }
        let id = (rt.ice_heist_instances.len() as i64) + 800_001;
        rt.ice_heist_instances.push(IceHeistRecord {
            instance_id: id,
            solar_system_id: sys_id,
            pirate_tankers_saved: 0,
            empire_tankers_saved: 0,
            completed: false,
        });
        id
    });

    // Spawn web-immune tanker NPC ball + Upwell Thunderchild guard ball in space
    let sys = ctx.systems.get_or_spawn(sys_id);
    let tanker_id = 1_896_000_000 + instance_id * 10 + 1;
    let guard_id = 1_896_000_000 + instance_id * 10 + 2;
    let refinery_pos = DVec3::new(150_000.0, 0.0, 150_000.0);

    let mut tanker_cs = CombatState::default();
    tanker_cs.shield_hp = 25_000.0;
    tanker_cs.max_shield_hp = 25_000.0;
    tanker_cs.armor_hp = 25_000.0;
    tanker_cs.max_armor_hp = 25_000.0;
    tanker_cs.structure_hp = 30_000.0;
    tanker_cs.max_structure_hp = 30_000.0;
    tanker_cs.warp_core_strength = 100;
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(tanker_id, ICE_TANKER_TYPE_ID, refinery_pos),
            tanker_cs,
        )
        .await;

    let _ = sys
        .spawn_ball(make_site_npc_ball(
            guard_id,
            THUNDERCHILD_GUARD_TYPE_ID,
            refinery_pos + DVec3::new(5_000.0, 0.0, 0.0),
        ))
        .await;

    Ok(instance_id)
}

/// Escorts a tanker to the caller's side scoring zone, enforces Upwell Thunderchild guard destruction
/// of nearby pirates, and awards 150,000 LP + 4h respawn cooldown when a side reaches 10 tankers.
pub async fn escort_ice_heist_tanker(
    ctx: &ServerContext,
    char_id: i64,
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

    // Destroy any pirate ships within 25km of the Upwell Thunderchild guards
    let sys = ctx.systems.get_or_spawn(sys_id);
    let guard_id = 1_896_000_000 + instance_id * 10 + 2;
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let mut destroyed_pirates = Vec::new();
    if let Some(guard_ball) = all_balls.iter().find(|b| b.id == guard_id) {
        let guard_pos = guard_ball.position;
        for b in &all_balls {
            if b.id == guard_id || (b.position - guard_pos).length() > THUNDERCHILD_GUARD_RANGE_M {
                continue;
            }
            let is_pirate_ship = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == b.id)
                    .and_then(|c| c.war_faction_id)
                    .is_some_and(|wf| wf == FACTION_ANGEL_CARTEL || wf == FACTION_GURISTAS)
            });
            if is_pirate_ship {
                let _ = sys.remove_ball(b.id).await;
                destroyed_pirates.push(b.id);
            }
        }
    }

    let mut lp_awarded = 0_i64;
    if just_won {
        lp_awarded = ICE_HEIST_WIN_LP;
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(
                respawn_key(sys_id),
                (now_unix + ICE_HEIST_RESPAWN_COOLDOWN_SECS).to_string(),
            );
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let corp_id = ch.war_faction_id.unwrap_or(1_000_137);
                *ch.loyalty_points.entry(corp_id).or_insert(0) += ICE_HEIST_WIN_LP;
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
        (
            "destroyedPiratesNearGuards",
            PyRep::List(destroyed_pirates.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

pub async fn dispatch_ice_heist_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    match method {
        "SpawnIceHeistInstance" => {
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let id = spawn_ice_heist_checked(ctx, sys_id).await?;
            Ok(PyRep::Integer(id))
        }
        "EscortIceHeistTanker" => {
            let instance_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let pirate_side = args
                .get(1)
                .and_then(|a| match a {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(i) => Some(*i != 0),
                    _ => None,
                })
                .unwrap_or(false);
            escort_ice_heist_tanker(ctx, char_id, instance_id, pirate_side).await
        }
        _ => Err(BridgeRpcError::user_error("UnknownMethod")),
    }
}
