//! `combat_weapons_targeting` — Weapon Attribute Queries, Signature Resolution,
//! and Vorton Arc Chaining (`#1541` Track G1b split to keep `combat_weapons.rs < 2,500L`).

#![forbid(unsafe_code)]

use super::*;

pub(crate) fn weapon_mod_has_attr(
    mod_state: &eve_dogma::DogmaItemState,
    mod_type: &eve_sde::InvType,
    aid: u16,
) -> bool {
    mod_state.base_attributes.contains_key(&aid)
        || mod_type.default_attributes.contains_key(&aid)
        || mod_state.modifiers.iter().any(|m| m.target_attribute_id == aid)
}

pub(crate) fn weapon_mod_attr_nonneg(
    mod_state: &eve_dogma::DogmaItemState,
    mod_type: &eve_sde::InvType,
    sde: &eve_sde::SdeRegistry,
    aid: u16,
    fallback: f64,
) -> f64 {
    if weapon_mod_has_attr(mod_state, mod_type, aid) {
        if mod_state.base_attributes.contains_key(&aid) {
            return mod_state.get_attribute(aid, sde).max(0.0);
        }
        if let Some(&v) = mod_type.default_attributes.get(&aid) {
            return v.max(0.0);
        }
        return mod_state.get_attribute(aid, sde).max(0.0);
    }
    fallback
}

pub(crate) fn weapon_mod_attr_pos(
    mod_state: &eve_dogma::DogmaItemState,
    mod_type: &eve_sde::InvType,
    sde: &eve_sde::SdeRegistry,
    aid: u16,
    fallback: f64,
) -> f64 {
    if weapon_mod_has_attr(mod_state, mod_type, aid) {
        let v = mod_state.get_attribute(aid, sde);
        if v > 0.0 {
            return v;
        }
    }
    mod_type
        .default_attributes
        .get(&aid)
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(fallback)
}

pub(crate) async fn resolve_target_effective_signature(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    target_ball_id: i64,
    target_ball: &eve_destiny::Ball,
    player_target_info: Option<(i64, i64)>,
) -> f64 {
    let raw_sig = if let Some((vcid, _)) = player_target_info {
        evaluate_effective_signature_radius(ctx, vcid, target_ball_id, sys_id).await
    } else if let Ok(Some(t_cs)) = sys_handle.get_combat_state(target_ball_id).await {
        t_cs.base_signature_radius.unwrap_or(if t_cs.signature_radius > 0.0 {
            t_cs.signature_radius
        } else {
            target_ball.radius.max(35.0)
        })
    } else {
        target_ball.radius.max(35.0)
    };
    ctx.effective_target_signature_radius(target_ball_id, raw_sig)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_vorton_arc_secondary_hits(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle,
    attacker_ball: &eve_destiny::Ball,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    target_ball_id: i64,
    target_ball: &eve_destiny::Ball,
    target_sig: f64,
    base_dmg: DamageVector,
    cycle_secs: f64,
    now_unix: u64,
    sys_sec: f64,
) -> (Vec<i64>, DamageVector) {
    let mut vorton_secondary_targets = Vec::new();
    let final_dmg = {
        let arc_range = weapon_mod_attr_nonneg(mod_state, mod_type, &ctx.sde, 3036, 10_000.0);
        let max_secondary = if weapon_mod_has_attr(mod_state, mod_type, 3037) {
            let total = weapon_mod_attr_nonneg(mod_state, mod_type, &ctx.sde, 3037, 10.0) as usize;
            total.saturating_sub(1)
        } else if weapon_mod_has_attr(mod_state, mod_type, 2104) {
            weapon_mod_attr_nonneg(mod_state, mod_type, &ctx.sde, 2104, 9.0) as usize
        } else {
            9
        };
        let exp_vel = weapon_mod_attr_pos(mod_state, mod_type, &ctx.sde, 653, 190.0);
        let exp_rad = weapon_mod_attr_pos(mod_state, mod_type, &ctx.sde, 654, 43.0);
        let drf = weapon_mod_attr_pos(mod_state, mod_type, &ctx.sde, 1353, 0.40);
        let hits = if max_secondary == 0 {
            Vec::new()
        } else {
            let all_balls = sys_handle.list_balls().await.unwrap_or_default();
            let candidate_distances: Vec<(i64, f64)> = all_balls
                .into_iter()
                .filter(|b| {
                    b.id != ship_id
                        && b.id != target_ball_id
                        && super::combat_missiles_aoe::is_combatant_ship_or_entity_ball(
                            ctx, b.id, b.type_id,
                        )
                })
                .map(|b| (b.id, target_ball.surface_distance(&b)))
                .collect();
            crate::formulas::vorton_arc_targets_from_distances(
                target_ball_id,
                &candidate_distances,
                arc_range,
                max_secondary,
            )
        };
        let safety = char_safety_level(ctx, char_id).unwrap_or(SafetyLevel::Red);
        let c_stage =
            crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
        let mut had_secondary_pvp_hit = false;
        for arc_id in hits.into_iter().skip(1) {
            if sys_handle
                .get_combat_state(arc_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.blocks_targeted_effect())
            {
                continue;
            }
            if let Ok(Some((_, arc_ball))) = sys_handle.get_ball_snapshot(arc_id).await {
                let (arc_type_id, arc_pos, arc_radius, arc_speed) = (
                    arc_ball.type_id,
                    arc_ball.position,
                    arc_ball.radius,
                    arc_ball.velocity.length(),
                );
                let arc_vic_opt = ctx.db.with_snapshot(|snap| {
                    super::resolve_pvp_target_info_in_snap(snap, Some(sys_id), arc_id, true, false)
                        .map(|(vcid, _)| vcid)
                });
                if let Some(vcid) = arc_vic_opt {
                    if vcid != char_id {
                        let is_legal = ctx.db.with_snapshot(|snap| {
                            is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                || has_limited_engagement_in_snap(snap, vcid, char_id, now_unix)
                        }) || {
                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                            let vst = cw.get_or_create(vcid);
                            vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                        };
                        if !is_legal {
                            let is_pod = arc_type_id == 670;
                            let concord_suppressed = !is_pod
                                && crate::services::combat_npc_police::is_concord_suppressed_in_system(
                                    ctx, sys_id,
                                );
                            let would_be_crim =
                                (sys_sec >= 0.45 && !concord_suppressed) || (sys_sec > 0.0 && is_pod);
                            let would_be_susp = !would_be_crim && sys_sec > 0.0;
                            if (safety == SafetyLevel::Green && (would_be_crim || would_be_susp))
                                || (safety == SafetyLevel::Yellow && would_be_crim)
                            {
                                continue;
                            }
                        }
                    }
                }
                let arc_factor = missile_damage_factor(
                    arc_radius.max(35.0),
                    arc_speed,
                    exp_rad,
                    exp_vel,
                    drf,
                );
                if let Some((arc_app, arc_destroyed, _)) =
                    crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                        ctx,
                        sys_handle,
                        sys_id,
                        char_id,
                        arc_id,
                        base_dmg.scaled(arc_factor),
                        cycle_secs,
                        now_unix,
                    )
                    .await
                    .ok()
                    .flatten()
                {
                    vorton_secondary_targets.push(arc_id);
                    record_killmail_assist_action(
                        ctx,
                        arc_id,
                        char_id,
                        attacker_ball.type_id,
                        m.type_id,
                        arc_app,
                        false,
                        now_unix,
                    );
                    if let Some(vcid) = arc_vic_opt.filter(|&v| v != char_id) {
                        record_killmail_assist_action(
                            ctx,
                            vcid,
                            char_id,
                            attacker_ball.type_id,
                            m.type_id,
                            arc_app,
                            false,
                            now_unix,
                        );
                        let is_legal = ctx.db.with_snapshot(|snap| {
                            is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                || has_limited_engagement_in_snap(snap, vcid, char_id, now_unix)
                        }) || {
                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                            let vst = cw.get_or_create(vcid);
                            vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                        };
                        let is_arc_pod = arc_type_id == 670;
                        let is_arc_mobile =
                            crate::services::mobile_deployables::is_mobile_structure(
                                &ctx.sde,
                                arc_type_id,
                            );
                        let is_new_agg = ctx
                            .crimewatch
                            .read()
                            .map(|cw| cw.get_aggression(char_id, vcid, now_unix).is_none())
                            .unwrap_or(true);
                        {
                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                            cw.system_corruption_stages.insert(sys_id, c_stage);
                            let eff_sec = if is_arc_mobile {
                                sys_sec.min(0.40)
                            } else {
                                sys_sec
                            };
                            let _ = cw.handle_combat_aggression_with_pod(
                                char_id, vcid, ship_id, sys_id, eff_sec, is_legal, is_arc_pod,
                                now_unix,
                            );
                            cw.get_or_create(char_id).pvp_timer_until_unix =
                                now_unix.saturating_add(900);
                            cw.get_or_create(vcid).pvp_timer_until_unix =
                                now_unix.saturating_add(900);
                        }
                        if !is_legal && sys_sec > 0.0 && !is_arc_mobile && is_new_agg {
                            crate::services::combat_crimewatch::crimewatch_timers::apply_illegal_pvp_sec_status_loss(
                                ctx, char_id, vcid, sys_id, sys_sec, is_arc_pod, now_unix,
                            );
                        }
                        crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                            ctx, char_id, vcid, now_unix,
                        );
                        had_secondary_pvp_hit = true;
                    }
                    if arc_destroyed {
                        super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                            ctx,
                            sys_id,
                            char_id,
                            arc_id,
                            arc_type_id,
                            arc_pos,
                            now_unix,
                        )
                        .await;
                    }
                }
            }
        }
        if had_secondary_pvp_hit {
            crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(
                ctx, char_id, now_unix,
            );
        }
        let p_factor = missile_damage_factor(
            target_sig,
            target_ball.velocity.length(),
            exp_rad,
            exp_vel,
            drf,
        );
        base_dmg.scaled(p_factor)
    };
    (vorton_secondary_targets, final_dmg)
}
