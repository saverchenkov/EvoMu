//! Scene-scoped combat damage log and damage state notifications (`#1190`, `#1565`).

#![forbid(unsafe_code)]

use crate::outbound_sequencer::OutboundSceneScope;
use crate::router::notify_registry::{OnDamageMessage, OnDamageMessages};
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
    scope: Option<OutboundSceneScope>,
    hit_quality: &str,
    dmg_dict: PyRep,
) {
    ctx.emit_typed_in_scene(
        NotificationTarget::Character(recipient_char_id),
        scope,
        OnDamageMessage {
            damage_messages_args: dmg_dict.clone(),
        },
    );
    ctx.emit_typed_in_scene(
        NotificationTarget::Character(recipient_char_id),
        scope,
        OnDamageMessages {
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
    let combat_sys = ctx.db.with_snapshot(|snap| {
        let item_sys = |id: i64| {
            snap.items
                .get(&id)
                .and_then(|i| ctx.resolve_solar_system_location_id(i.location_id))
        };
        active_drone
            .map(|(_, _, sys)| sys)
            .or_else(|| item_sys(attacker_ship_id))
            .or_else(|| snap.structures.get(&attacker_ship_id).map(|s| s.solar_system_id))
            .or_else(|| ctx.space_char_sys_in_snapshot(snap, attacker_char_id))
            .or_else(|| active_fighter.and_then(|(_, _, iid)| item_sys(iid)))
            .or_else(|| item_sys(target_ball_id))
            .or_else(|| snap.structures.get(&target_ball_id).map(|s| s.solar_system_id))
    });
    let attacker_scope = Some(ctx.character_space_scene_scope(attacker_char_id, combat_sys));
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
    emit_damage_log_pair(ctx, attacker_char_id, attacker_scope, hit_quality, attacker_dict);
    emit_damage_state_change_in_scene(
        ctx,
        attacker_char_id,
        target_ball_id,
        shield_ratio,
        armor_ratio,
        hull_ratio,
        attacker_scope,
    );
    emit_combat_message_in_scene(
        ctx,
        attacker_char_id,
        if applied_dmg > 0.0 { "AttackHit" } else { "AttackMiss" },
        target_ball_id,
        applied_dmg,
        weapon_type_id,
        is_wrecking,
        attacker_scope,
    );
    // Resolve active craft owner/pilot (`#1565`): abandoned drones/fighters and stale unpiloted
    // items have no active controller; docked and out-of-system suppression is enforced downstream
    // on the wire by outbound scene scoping (`#1190`).
    let resolved_victim = if matches!(active_drone, Some((false, _, _)))
        || matches!(active_fighter, Some((false, _, _)))
    {
        None
    } else {
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
            Some(cid)
        })
    };
    if let Some(vic_id) = resolved_victim {
        let victim_scope = Some(ctx.character_space_scene_scope(vic_id, combat_sys));
        if vic_id != attacker_char_id {
            emit_damage_state_change_in_scene(
                ctx,
                vic_id,
                target_ball_id,
                shield_ratio,
                armor_ratio,
                hull_ratio,
                victim_scope,
            );
        }
        stream_fleet_watchlist_damage(ctx, vic_id, shield_ratio, armor_ratio, hull_ratio);
        if vic_id != attacker_char_id {
            emit_combat_message_in_scene(
                ctx,
                vic_id,
                if applied_dmg > 0.0 { "AttackedBy" } else { "AttackMissBy" },
                attacker_ship_id,
                applied_dmg,
                weapon_type_id,
                is_wrecking,
                victim_scope,
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
            emit_damage_log_pair(ctx, vic_id, victim_scope, hit_quality, vic_dict);
        }
    }
}
