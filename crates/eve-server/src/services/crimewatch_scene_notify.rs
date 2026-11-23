//! Scene-scoped combat damage log and damage state notifications (`#1190`, `#1565`).

#![forbid(unsafe_code)]

use crate::router::notify_registry::{BallparkScene, OnDamageMessage, OnDamageMessages};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::with_combat_runtime;
use crate::services::fleet_social::stream_fleet_watchlist_damage;
use crate::services::notify_emitters_combat_ess_progression::emit_damage_state_change_in_scene;
use crate::services::notify_emitters_ess_bounties::emit_combat_message_in_scene;
use eve_proto::PyRep;

#[allow(clippy::too_many_arguments)]
fn build_damage_log_dict(
    attacker_char_id: i64,
    attacker_ship_id: i64,
    target_ball_id: i64,
    weapon_type_id: u32,
    applied_dmg: f64,
    is_wrecking: bool,
    hit_quality_code: i64,
    attack_type: &str,
) -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
        (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
        (PyRep::String("attackerID".into()), PyRep::Integer(attacker_ship_id)),
        (PyRep::String("target".into()), PyRep::Integer(target_ball_id)),
        (PyRep::String("target_ID".into()), PyRep::Integer(target_ball_id)),
        (
            PyRep::String("owner".into()),
            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(attacker_char_id)]),
        ),
        (PyRep::String("weapon".into()), PyRep::Integer(weapon_type_id as i64)),
        (PyRep::String("hitQuality".into()), PyRep::Integer(hit_quality_code)),
        (PyRep::String("attackType".into()), PyRep::String(attack_type.into())),
        (PyRep::String("isWrecking".into()), PyRep::Bool(is_wrecking)),
        (PyRep::String("isBanked".into()), PyRep::Bool(false)),
    ])
}

fn emit_damage_log_pair(
    ctx: &ServerContext,
    recipient_char_id: i64,
    scene: BallparkScene,
    hit_quality: &str,
    dmg_dict: PyRep,
) {
    ctx.emit_typed(
        NotificationTarget::Character(recipient_char_id),
        OnDamageMessage {
            scene,
            damage_messages_args: dmg_dict.clone(),
        },
    );
    ctx.emit_typed(
        NotificationTarget::Character(recipient_char_id),
        OnDamageMessages {
            scene,
            dmgmsgs: vec![PyRep::Tuple(vec![PyRep::String(hit_quality.into()), dmg_dict])],
        },
    );
}

/// Emits `OnDamageMessage`, `OnDamageMessages`, and `OnDamageStateChange` notifications to both attacker and victim (`feat:combat.damage-log`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_combat_log_and_damage_state(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_opt: Option<i64>,
    attacker_ship_id: i64,
    target_ball_id: i64,
    weapon_type_id: u32,
    applied_dmg: f64,
    is_wrecking: bool,
    shield_ratio: f64,
    armor_ratio: f64,
    hull_ratio: f64,
    combat_sys_id: Option<u32>,
) {
    let hit_quality = if applied_dmg <= 0.0 {
        "miss"
    } else if is_wrecking {
        "wrecking"
    } else {
        "hit"
    };
    let hit_quality_code = if applied_dmg <= 0.0 {
        0
    } else if is_wrecking {
        6
    } else {
        3
    };
    let active_drone = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.get(&target_ball_id).map(|d| {
            (
                !d.is_abandoned && d.owner_char_id > 0,
                d.owner_char_id,
                d.solar_system_id,
            )
        })
    });
    let active_fighter = ctx.active_fighters.read().ok().and_then(|fs| {
        fs.get(&target_ball_id)
            .map(|sq| (!sq.is_abandoned && sq.owner_char_id > 0, sq.owner_char_id, sq.item_id))
    });
    let combat_sys = combat_sys_id.filter(|&id| id != 0).or_else(|| {
        ctx.db.with_snapshot(|snap| {
            let item_sys = |id: i64| {
                snap.items
                    .get(&id)
                    .and_then(|i| ctx.resolve_solar_system_location_id(i.location_id))
                    .filter(|&s| s > 0)
            };
            active_drone
                .map(|(_, _, sys)| sys)
                .filter(|&s| s > 0)
                .or_else(|| item_sys(attacker_ship_id))
                .or_else(|| {
                    snap.structures
                        .get(&attacker_ship_id)
                        .map(|s| s.solar_system_id)
                        .filter(|&s| s > 0)
                })
                .or_else(|| ctx.space_char_sys_in_snapshot(snap, attacker_char_id).filter(|&s| s > 0))
        })
    });
    if let Some(att_sys) = combat_sys {
        if let Some(attacker_scene) = match ctx.active_session_scene_epoch_for_char(attacker_char_id)
        {
            Some(ep) => BallparkScene::try_with_epoch(att_sys, ep),
            None => BallparkScene::try_solar_system(att_sys),
        } {
            let attacker_dict = build_damage_log_dict(
                attacker_char_id,
                attacker_ship_id,
                target_ball_id,
                weapon_type_id,
                applied_dmg,
                is_wrecking,
                hit_quality_code,
                "me",
            );
            emit_damage_log_pair(ctx, attacker_char_id, attacker_scene, hit_quality, attacker_dict);
            emit_damage_state_change_in_scene(
                ctx,
                attacker_char_id,
                target_ball_id,
                shield_ratio,
                armor_ratio,
                hull_ratio,
                attacker_scene,
            );
            emit_combat_message_in_scene(
                ctx,
                attacker_char_id,
                if applied_dmg > 0.0 { "AttackHit" } else { "AttackMiss" },
                target_ball_id,
                applied_dmg,
                weapon_type_id,
                is_wrecking,
                attacker_scene,
            );
        }
    }
    // Resolve active craft owner/pilot (`#1565`) only when the victim is currently undocked in
    // `combat_sys` (active structure gunners count as undocked in the structure's system).
    // Skipping already-docked or out-of-system victims avoids cluttering `pending_notifications`;
    // post-enqueue dock/jump transitions remain gated downstream on the outbound wire (`#1190`).
    let resolved_victim = if matches!(active_drone, Some((false, _, _)))
        || matches!(active_fighter, Some((false, _, _)))
    {
        None
    } else if let Some(sys) = combat_sys {
        ctx.db.with_snapshot(|snap| {
            let cid = match (active_drone, active_fighter) {
                (Some((true, oid, _)), _) | (_, Some((true, oid, _))) => Some(oid),
                _ => victim_char_opt.or_else(|| {
                    snap.characters
                        .get(&target_ball_id)
                        .or_else(|| snap.characters.values().find(|c| c.active_ship_id == target_ball_id))
                        .map(|c| c.character_id)
                }),
            }?;
            let vic = snap.characters.get(&cid)?;
            let active_craft = active_drone.is_some()
                || active_fighter.is_some()
                || target_ball_id == vic.character_id
                || target_ball_id == vic.active_ship_id;
            if !active_craft && snap.items.contains_key(&target_ball_id) {
                return None;
            }
            if ctx.space_char_sys_in_snapshot(snap, cid) != Some(sys) {
                return None;
            }
            let hit_piloted_ship = active_drone.is_none()
                && active_fighter.is_none()
                && (target_ball_id == vic.character_id || target_ball_id == vic.active_ship_id);
            Some((cid, sys, hit_piloted_ship))
        })
    } else {
        None
    };
    if let Some((vic_id, vic_sys, hit_piloted_ship)) = resolved_victim {
        let victim_scene_opt = BallparkScene::try_from_outbound_scope(
            ctx.character_space_scene_scope(vic_id, Some(vic_sys)),
            vic_sys,
        );
        if vic_id != attacker_char_id {
            if let Some(victim_scene) = victim_scene_opt {
                emit_damage_state_change_in_scene(
                    ctx,
                    vic_id,
                    target_ball_id,
                    shield_ratio,
                    armor_ratio,
                    hull_ratio,
                    victim_scene,
                );
            }
        }
        if hit_piloted_ship {
            stream_fleet_watchlist_damage(ctx, vic_id, shield_ratio, armor_ratio, hull_ratio);
        }
        if vic_id != attacker_char_id {
            if let Some(victim_scene) = victim_scene_opt {
                emit_combat_message_in_scene(
                    ctx,
                    vic_id,
                    if applied_dmg > 0.0 { "AttackedBy" } else { "AttackMissBy" },
                    attacker_ship_id,
                    applied_dmg,
                    weapon_type_id,
                    is_wrecking,
                    victim_scene,
                );
                let vic_dict = build_damage_log_dict(
                    attacker_char_id,
                    attacker_ship_id,
                    target_ball_id,
                    weapon_type_id,
                    applied_dmg,
                    is_wrecking,
                    hit_quality_code,
                    "other",
                );
                emit_damage_log_pair(ctx, vic_id, victim_scene, hit_quality, vic_dict);
            }
        }
    }
}
