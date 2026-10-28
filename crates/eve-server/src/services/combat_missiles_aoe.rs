//! `combat_missiles_aoe` — In-flight Missile Physics, DRF, Smartbombs, Bombs, Defender Missiles & Breacher Pods (`C-2`).

#![forbid(unsafe_code)]

use super::*;

pub(crate) fn is_combatant_ship_or_entity_ball(ctx: &ServerContext, ball_id: i64, ball_type_id: u32) -> bool {
    crate::services::tactical_modules::is_combatant_ship_or_entity_ball(
        Some(&ctx.sde),
        Some(&ctx.db),
        ball_id,
        ball_type_id,
    ) || (1_895_000_000..1_900_000_000).contains(&ball_id)
}

/// Returns `Some(char_id)` only if `ball_id` is an active capsuleer ship (`category_id == 6`
/// and `ch.active_ship_id == ball_id`). Non-ship balls (drones, wrecks, jetcans, corpses)
/// return `None` so they never trigger `execute_death_pipeline`.
pub(crate) fn active_ship_victim_char_id(ctx: &ServerContext, ball_id: i64) -> Option<i64> {
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&ball_id)?;
        let cat_id = ctx
            .sde
            .types
            .get(&item.type_id)
            .map(|t| t.category_id)
            .unwrap_or(0);
        if cat_id != 6 {
            return None;
        }
        snap.characters
            .values()
            .find(|c| c.active_ship_id == ball_id)
            .map(|c| c.character_id)
    })
}

/// Cleans up a destroyed non-ship ball in space (drone, wreck, jetcan, container, or corpse)
/// without invoking the capsuleer ship death pipeline, failing any courier contracts inside
/// and deleting all nested descendant items so no orphans remain (`feat:lifecycle.courier-package-destroyed`).
pub(crate) async fn cleanup_destroyed_non_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.remove_ball(ball_id).await;
    with_mut_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.remove(&ball_id);
        rt.active_drone_miners.remove(&ball_id);
        rt.container_lifetimes.remove(&ball_id);
    });
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if snap.items.remove(&ball_id).is_some() {
            snap.item_positions.remove(&ball_id);
            snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
            snap.sp6_lifecycle.ship_combat_hp.remove(&ball_id);
            let mut to_visit = vec![ball_id];
            let mut all_descendants = Vec::new();
            while let Some(parent_id) = to_visit.pop() {
                let children: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == parent_id && i.item_id != parent_id)
                    .map(|i| i.item_id)
                    .collect();
                for cid in children {
                    if !all_descendants.contains(&cid) {
                        all_descendants.push(cid);
                        to_visit.push(cid);
                    }
                }
            }
            let courier_cids: Vec<i64> = snap
                .contracts
                .values()
                .filter(|c| {
                    c.contract_type == "Courier"
                        && c.status == "InProgress"
                        && c.item_ids.iter().any(|iid| all_descendants.contains(iid))
                })
                .map(|c| c.contract_id)
                .collect();
            for cid in courier_cids {
                snap.fail_courier_contract(cid, now_unix);
            }
            for cid in all_descendants {
                snap.items.remove(&cid);
                snap.item_positions.remove(&cid);
            }
        }
    });
}

/// Routes a ball destroyed by missile, smartbomb, or bomb to the capsuleer death pipeline,
/// the NPC destruction & bounty pipeline, or non-ship space object cleanup.
pub(crate) async fn handle_destroyed_ball_from_missile_or_aoe(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: i64,
    ball_id: i64,
    ball_type_id: u32,
    ball_pos: DVec3,
    now_unix: u64,
) {
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&ball_id)) {
        return;
    }
    if let Some(vcid) = active_ship_victim_char_id(ctx, ball_id) {
        let _ = crate::combat::death_pipeline::execute_death_pipeline(
            ctx,
            sys_id,
            vcid,
            ball_id,
            attacker_char_id,
            ball_pos,
            now_unix,
        )
        .await;
    } else if with_combat_runtime(ctx, |rt| {
        rt.npc_entities.contains_key(&ball_id) || rt.destroyed_npc_cache.contains_key(&ball_id)
    }) || ctx.living_world.get_convoy(ball_id).is_some()
        || ctx
            .sde
            .types
            .get(&ball_type_id)
            .is_some_and(|t| t.category_id == 11)
    {
        let _ = super::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
            ctx,
            sys_id,
            ball_id,
            attacker_char_id,
        )
        .await;
    } else {
        cleanup_destroyed_non_ship_ball(ctx, sys_id, ball_id).await;
    }
}

/// (`feat:combat.missile-flight-physics-destruction`, `feat:modules.breacher-pods`).
pub async fn step_combat_system(
    ctx: &ServerContext,
    solar_system_id: u32,
    dt: f64,
) -> Vec<InFlightMissileState> {
    crate::services::combat_weapons_spool::step_system_entropic_spools(ctx, solar_system_id, dt).await;
    step_missiles_and_breachers(ctx, solar_system_id, dt).await
}

pub async fn step_missiles_and_breachers(
    ctx: &ServerContext,
    solar_system_id: u32,
    dt: f64,
) -> Vec<InFlightMissileState> {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let missile_ids: Vec<i64> = with_combat_runtime(ctx, |rt| {
        rt.in_flight_missiles
            .values()
            .filter(|m| m.solar_system_id == solar_system_id && !m.destroyed && !m.expired && !m.impacted)
            .map(|m| m.missile_ball_id)
            .collect()
    });

    let mut completed = Vec::new();
    for mid in missile_ids {
        let state_opt = with_combat_runtime(ctx, |rt| rt.in_flight_missiles.get(&mid).cloned());
        let Some(mut mstate) = state_opt else {
            continue;
        };

        // Check if missile ball was destroyed in Ballpark
        let missile_ball_opt = sys_handle.get_ball_snapshot(mid).await.ok().flatten();
        let missile_cs_opt = sys_handle.get_combat_state(mid).await.ok().flatten();
        if missile_ball_opt.is_none() || missile_cs_opt.as_ref().is_some_and(|cs| cs.structure_hp <= 0.0) {
            mstate.destroyed = true;
            mstate.applied_damage = 0.0;
            let _ = sys_handle.remove_ball(mid).await;
            with_mut_combat_runtime(ctx, |rt| {
                rt.in_flight_missiles.insert(mid, mstate.clone());
            });
            completed.push(mstate);
            continue;
        }
        let (_, mut m_ball) = missile_ball_opt.unwrap();
        let Some((_, t_ball)) = sys_handle
            .get_ball_snapshot(mstate.target_ball_id)
            .await
            .ok()
            .flatten()
        else {
            mstate.expired = true;
            let _ = sys_handle.remove_ball(mid).await;
            with_mut_combat_runtime(ctx, |rt| {
                rt.in_flight_missiles.insert(mid, mstate.clone());
            });
            completed.push(mstate);
            continue;
        };

        let to_target = t_ball.position - m_ball.position;
        let raw_len = to_target.length();
        let dist = if mstate.target_ball_id >= 1_000_000_000_000 { (raw_len - t_ball.radius).max(0.0) } else { raw_len };
        let dir = if raw_len > 1e-3 {
            to_target / raw_len
        } else {
            DVec3::X
        };
        let target_radial_vel = t_ball.velocity.dot(dir).max(0.0);
        let closing_speed = (mstate.max_velocity - target_radial_vel).max(0.0);
        let step_time = dt.min(mstate.remaining_flight_s);
        let dist_covered = closing_speed * step_time;

        if dist <= dist_covered && closing_speed > 0.0 {
            // Missile impacts target within this step!
            mstate.impacted = true;
            mstate.remaining_flight_s = (mstate.remaining_flight_s - (dist / closing_speed)).max(0.0);
            let _ = sys_handle.remove_ball(mid).await;

            let raw_sig = if let Ok(Some(t_cs)) = sys_handle.get_combat_state(mstate.target_ball_id).await {
                t_cs.base_signature_radius.unwrap_or(if t_cs.signature_radius > 0.0 { t_cs.signature_radius } else { t_ball.radius.max(35.0) })
            } else {
                t_ball.radius.max(35.0)
            };
            let target_sig = ctx.effective_target_signature_radius(mstate.target_ball_id, raw_sig);
            let target_speed = if t_ball.velocity.length() > 0.0 {
                t_ball.velocity.length()
            } else {
                t_ball.max_velocity * t_ball.speed_fraction
            };
            let factor = missile_damage_factor(
                target_sig,
                target_speed,
                mstate.explosion_radius,
                mstate.explosion_velocity,
                mstate.drf,
            );
            let scaled = mstate.base_damage.scaled(factor);
            if let Ok(Some((applied, destroyed, _))) = crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(ctx, &sys_handle, mstate.solar_system_id, mstate.attacker_char_id, mstate.target_ball_id, scaled, mstate.flight_time_s.max(1.0), ctx.now_unix()).await {
                mstate.applied_damage = applied;
                let victim_char_opt = ctx.db.with_snapshot(|snap| {
                    snap.characters.values().find(|c| c.active_ship_id == mstate.target_ball_id).map(|c| c.character_id)
                        .or_else(|| snap.items.get(&mstate.target_ball_id).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)))
                });
                let now_unix = ctx.now_unix();
                if applied > 0.0 {
                    if let Some(vcid) = victim_char_opt.filter(|&v| v != mstate.attacker_char_id) {
                        crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                            ctx,
                            mstate.attacker_char_id,
                            vcid,
                            now_unix,
                        );
                    } else if mstate.attacker_char_id > 0 {
                        crate::sp6_lifecycle::record_npc_aggression_timer(ctx, mstate.attacker_char_id, now_unix);
                    }
                    super::combat_npc_ai::record_npc_threat_event(ctx, mstate.target_ball_id, mstate.attacker_ship_id, applied, false, false);
                }
                if let Ok(Some(t_cs)) = sys_handle.get_combat_state(mstate.target_ball_id).await {
                    let sr = (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
                    let ar = (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
                    let hr = (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
                    emit_combat_log_and_damage_state(
                        ctx,
                        mstate.attacker_char_id,
                        None,
                        mstate.attacker_ship_id,
                        mstate.target_ball_id,
                        mstate.missile_type_id,
                        applied,
                        false,
                        sr,
                        ar,
                        hr,
                    );
                }
                if destroyed {
                    handle_destroyed_ball_from_missile_or_aoe(
                        ctx,
                        mstate.solar_system_id,
                        mstate.attacker_char_id,
                        mstate.target_ball_id,
                        t_ball.type_id,
                        t_ball.position,
                        now_unix,
                    )
                    .await;
                } else if mstate.is_breacher_pod && mstate.breacher_dot_duration_s > 0.0 {
                    let total_target_hp = if let Ok(Some(t_cs)) =
                        sys_handle.get_combat_state(mstate.target_ball_id).await
                    {
                        (t_cs.max_shield_hp + t_cs.max_armor_hp + t_cs.max_structure_hp).max(1.0)
                    } else {
                        10_000.0
                    };
                    let tick_dps = breacher_pod_tick_damage(
                        total_target_hp,
                        mstate.breacher_pct_cap,
                        mstate.breacher_flat_cap,
                    );
                    with_mut_combat_runtime(ctx, |rt| {
                        rt.active_breacher_dots.insert(
                            mstate.target_ball_id,
                            ActiveBreacherDot {
                                target_ball_id: mstate.target_ball_id,
                                source_char_id: mstate.attacker_char_id,
                                source_ship_id: mstate.attacker_ship_id,
                                weapon_type_id: mstate.launcher_type_id,
                                charge_type_id: mstate.missile_type_id,
                                flat_cap_dps: mstate.breacher_flat_cap,
                                pct_cap_fraction: mstate.breacher_pct_cap,
                                remaining_s: mstate.breacher_dot_duration_s,
                                tick_damage: tick_dps,
                            },
                        );
                    });
                }
            }
            with_mut_combat_runtime(ctx, |rt| {
                rt.in_flight_missiles.insert(mid, mstate.clone());
            });
            completed.push(mstate);
        } else {
            m_ball.position += dir * (mstate.max_velocity * step_time);
            let _ = sys_handle.set_ball_position(mid, m_ball.position).await;
            mstate.remaining_flight_s -= step_time;
            if mstate.remaining_flight_s <= 1e-6 {
                // Missile ran out of flight time before reaching target (outrun/out of range) -> 0 damage!
                mstate.expired = true;
                mstate.applied_damage = 0.0;
                let _ = sys_handle.remove_ball(mid).await;
                with_mut_combat_runtime(ctx, |rt| {
                    rt.in_flight_missiles.insert(mid, mstate.clone());
                });
                completed.push(mstate);
            } else {
                with_mut_combat_runtime(ctx, |rt| {
                    rt.in_flight_missiles.insert(mid, mstate.clone());
                });
            }
        }
    }

    // Step active Breacher Pod DoTs (non-stacking per target) ONLY for targets in `solar_system_id` (#208)
    let dot_targets: Vec<i64> = with_combat_runtime(ctx, |rt| {
        rt.active_breacher_dots.keys().copied().collect()
    });
    for tid in dot_targets {
        let dot_opt = with_combat_runtime(ctx, |rt| rt.active_breacher_dots.get(&tid).cloned());
        if let Some(mut dot) = dot_opt {
            if let Ok(Some(mut cs)) = sys_handle.get_combat_state(tid).await {
                let mut destroyed_by_dot = false;
                let total_max_hp = (cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp).max(1.0);
                let dps = breacher_pod_tick_damage(total_max_hp, dot.pct_cap_fraction, dot.flat_cap_dps);
                let step_s = dt.min(dot.remaining_s).max(0.0);
                let raw_dmg = dps * step_s;
                dot.tick_damage = dps;
                let mut rem = raw_dmg;
                if cs.shield_hp > 0.0 {
                    let take = cs.shield_hp.min(rem);
                    cs.shield_hp -= take;
                    rem -= take;
                }
                if rem > 0.0 && cs.armor_hp > 0.0 {
                    let take = cs.armor_hp.min(rem);
                    cs.armor_hp -= take;
                    rem -= take;
                }
                if rem > 0.0 && cs.structure_hp > 0.0 {
                    let take = cs.structure_hp.min(rem);
                    cs.structure_hp -= take;
                }
                if cs.structure_hp <= 1e-6 {
                    cs.structure_hp = 0.0;
                    destroyed_by_dot = true;
                }
                let _ = sys_handle.set_combat_state(tid, cs).await;
                dot.remaining_s -= dt;
                with_mut_combat_runtime(ctx, |rt| {
                    if destroyed_by_dot || dot.remaining_s <= 1e-6 {
                        rt.active_breacher_dots.remove(&tid);
                    } else {
                        rt.active_breacher_dots.insert(tid, dot.clone());
                    }
                });
                if destroyed_by_dot {
                    let (ball_type_id, ball_pos) = sys_handle
                        .get_ball_snapshot(tid)
                        .await
                        .ok()
                        .flatten()
                        .map(|(_, b)| (b.type_id, b.position))
                        .unwrap_or_else(|| {
                            let stid = ctx.db.with_snapshot(|snap| {
                                snap.items.get(&tid).map(|i| i.type_id).unwrap_or(0)
                            });
                            (stid, DVec3::ZERO)
                        });
                    handle_destroyed_ball_from_missile_or_aoe(
                        ctx,
                        solar_system_id,
                        dot.source_char_id,
                        tid,
                        ball_type_id,
                        ball_pos,
                        ctx.now_unix(),
                    )
                    .await;
                }
            }
        }
    }

    // Complete any self-destruct countdowns in `solar_system_id` whose timer has elapsed (`feat:ships.self-destruct-countdown`)
    let now_unix = ctx.now_unix();
    let due_countdowns: Vec<SelfDestructCountdown> = with_mut_combat_runtime(ctx, |rt| {
        let due_ids: Vec<i64> = rt
            .self_destruct_countdowns
            .values()
            .filter(|cd| cd.solar_system_id == solar_system_id && now_unix >= cd.detonate_at_unix)
            .map(|cd| cd.ship_id)
            .collect();
        due_ids
            .into_iter()
            .filter_map(|sid| rt.self_destruct_countdowns.remove(&sid))
            .collect()
    });
    for cd in due_countdowns {
        let pos = sys_handle
            .get_ball_snapshot(cd.ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);
        let (ship_type_id, _vic_corp, _vic_all) = ctx.db.with_snapshot(|snap| {
            let stid = snap.items.get(&cd.ship_id).map(|i| i.type_id).unwrap_or(670);
            let (corp, all) = snap
                .characters
                .get(&cd.char_id)
                .map(|c| (c.corporation_id, c.alliance_id))
                .unwrap_or((1_000_009, None));
            (stid, corp, all)
        });
        if let Ok(death) = crate::combat::death_pipeline::execute_self_destruct(
            ctx,
            solar_system_id,
            cd.char_id,
            cd.ship_id,
            pos,
            now_unix,
        )
        .await
        {
            let recent_assists: Vec<AssistLogEntry> = with_mut_combat_runtime(ctx, |rt| {
                let mut valid = Vec::new();
                for key in [cd.ship_id, cd.char_id] {
                    if let Some(list) = rt.aggression_assist_log.remove(&key) {
                        for e in list {
                            if now_unix.saturating_sub(e.last_action_unix)
                                <= KILLMAIL_ASSIST_WINDOW_SECS
                                && !valid
                                    .iter()
                                    .any(|v: &AssistLogEntry| v.attacker_char_id == e.attacker_char_id)
                            {
                                valid.push(e);
                            }
                        }
                    }
                }
                valid
            });
            if !recent_assists.is_empty() {
                let final_blow_char_id = recent_assists
                    .last()
                    .map(|a| a.attacker_char_id)
                    .unwrap_or(cd.char_id);
                let hull_cs = ctx.build_type_combat_state(ship_type_id);
                let hull_hp = (hull_cs.max_shield_hp + hull_cs.max_armor_hp + hull_cs.max_structure_hp).max(1.0);
                let mut km_attackers = Vec::new();
                for a in &recent_assists {
                    km_attackers.push(KillmailAttackerEntry {
                        character_id: a.attacker_char_id,
                        corporation_id: a.corporation_id,
                        alliance_id: a.alliance_id,
                        ship_type_id: a.ship_type_id,
                        weapon_type_id: a.weapon_type_id,
                        damage_done: a.damage_done,
                        final_blow: a.attacker_char_id == final_blow_char_id,
                        security_status: 0.0,
                    });
                }
                let assist_dmg: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
                let km_hash = compute_killmail_esi_hash(
                    cd.char_id,
                    final_blow_char_id,
                    ship_type_id,
                    now_unix,
                );
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(km) = snap.killmails.get_mut(&death.killmail_id) {
                        km.attacker_character_id = final_blow_char_id;
                    }
                });
                with_mut_combat_runtime(ctx, |rt| {
                    if let Some(dkm) = rt.detailed_killmails.get_mut(&death.killmail_id) {
                        dkm.victim_damage_taken = assist_dmg.max(hull_hp);
                        dkm.attackers = km_attackers;
                        dkm.killmail_hash = km_hash;
                    }
                });
            }
        }
    }

    completed
}


#[allow(clippy::too_many_arguments)]
pub async fn activate_aoe_or_defender_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_sec: f64,
    now_unix: u64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    attacker_ball: &eve_destiny::Ball,
    target_id_opt: Option<i64>,
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    activate_aoe_or_defender_module_with_cap(
        ctx,
        char_id,
        ship_id,
        sys_id,
        sys_sec,
        now_unix,
        module_id,
        m,
        mod_type,
        attacker_ball,
        target_id_opt,
        kwargs,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn activate_aoe_or_defender_module_with_cap(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_sec: f64,
    now_unix: u64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    attacker_ball: &eve_destiny::Ball,
    target_id_opt: Option<i64>,
    _kwargs: &HashMap<String, PyRep>,
    deduct_capacitor: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
            // 2. Smartbomb (`group_id == 72`, `feat:modules.smartbomb`, `feat:combat.smartbomb-aoe-damage`)
            // parity: feat:crimewatch.safety-setting-action-block, feat:crimewatch.limited-engagement-duel
            if mod_type.group_id == 72 {
                let safety = with_combat_runtime(ctx, |rt| {
                    rt.safety_levels.get(&char_id).copied().unwrap_or(SafetyLevel::Red)
                });
                if sys_sec > 0.0 && safety != SafetyLevel::Red {
                    return Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel")));
                }
                let is_overloaded = m.state == eve_dogma::ModuleState::Overloaded;
                let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
                let mod_state_opt = fit_opt.as_ref().map(|f| f.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde));
                let emp_range = mod_state_opt.as_ref().map(|st| st.get_attribute(99, &ctx.sde)).filter(|&r| r > 0.0)
                    .unwrap_or_else(|| mod_type.default_attributes.get(&99).copied().unwrap_or(3_000.0));
                let sb_dmg = mod_state_opt.as_ref().map(|st| DamageVector {
                    em: st.get_attribute(114, &ctx.sde), explosive: st.get_attribute(116, &ctx.sde),
                    kinetic: st.get_attribute(117, &ctx.sde), thermal: st.get_attribute(118, &ctx.sde),
                }).filter(|d| d.total() > 0.0).unwrap_or_else(|| DamageVector {
                    em: mod_type.default_attributes.get(&114).copied().unwrap_or(0.0),
                    explosive: mod_type.default_attributes.get(&116).copied().unwrap_or(0.0),
                    kinetic: mod_type.default_attributes.get(&117).copied().unwrap_or(0.0),
                    thermal: mod_type.default_attributes.get(&118).copied().unwrap_or(0.0),
                });
                if deduct_capacitor {
                    let cap_need = mod_state_opt
                        .as_ref()
                        .map(|st| st.get_attribute(6, &ctx.sde))
                        .filter(|&c| c > 0.0)
                        .unwrap_or_else(|| mod_type.default_attributes.get(&6).copied().unwrap_or(30.0));
                    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
                        if cs.capacitor < cap_need {
                            return Some(Err(BridgeRpcError {
                                code: "NotEnoughCapacitor".into(),
                                message: format!("Smartbomb requires {cap_need:.1} GJ capacitor (have {:.1} GJ)", cs.capacitor),
                            }));
                        }
                        cs.capacitor -= cap_need;
                        let _ = sys_handle.set_combat_state(ship_id, cs).await;
                    }
                }

                // Destroy/damage any in-flight missiles, scanner probes, and drones within smartbomb range (`feat:combat.smartbomb-aoe-damage`)
                let mut destroyed_missile_ids = Vec::new();
                let mut destroyed_probe_ids = Vec::new();
                let mut destroyed_drone_ids = Vec::new();
                let in_flight_ids: Vec<i64> = with_combat_runtime(ctx, |rt| {
                    rt.in_flight_missiles
                        .values()
                        .filter(|m| m.solar_system_id == sys_id && !m.destroyed && !m.expired && !m.impacted)
                        .map(|m| m.missile_ball_id)
                        .collect()
                });
                for mid in in_flight_ids {
                    if let Ok(Some((_, m_ball))) = sys_handle.get_ball_snapshot(mid).await {
                        if attacker_ball.position.distance(m_ball.position) <= emp_range {
                            let mut destroyed_now = false;
                            with_mut_combat_runtime(ctx, |rt| {
                                if let Some(ms) = rt.in_flight_missiles.get_mut(&mid) {
                                    ms.hp -= sb_dmg.total();
                                    if ms.hp <= 0.0 {
                                        ms.hp = 0.0;
                                        ms.destroyed = true;
                                        ms.applied_damage = 0.0;
                                        destroyed_now = true;
                                    }
                                }
                            });
                            if destroyed_now {
                                let _ = sys_handle.remove_ball(mid).await;
                                destroyed_missile_ids.push(mid);
                            }
                        }
                    }
                }
                // Apply AoE damage to all uncloaked dynamic ship/NPC/drone/probe balls within emp_range (including fleetmates!)
                let all_balls = sys_handle.list_balls().await.unwrap_or_default();
                let mut hit_ball_ids = Vec::new();
                let mut illegal_capsuleer_hits: Vec<(i64, bool)> = Vec::new();
                for b in all_balls {
                    let is_probe_b = ctx.sde.types.get(&b.type_id).is_some_and(|t| matches!(t.group_id, 479 | 492 | 548));
                    let is_drone_b = ctx.sde.types.get(&b.type_id).and_then(|t| ctx.sde.groups.get(&t.group_id)).is_some_and(|g| g.category_id == 18);
                    let is_targetable = is_combatant_ship_or_entity_ball(ctx, b.id, b.type_id) || is_drone_b || is_probe_b;
                    if b.id == ship_id || !is_targetable || destroyed_missile_ids.contains(&b.id) {
                        continue;
                    }
                    if attacker_ball.position.distance(b.position) <= emp_range {
                        if sys_handle.get_combat_state(b.id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
                            continue;
                        }
                        let victim_char_opt = ctx.db.with_snapshot(|snap| {
                            snap.characters.values().find(|c| c.active_ship_id == b.id).map(|c| c.character_id)
                                .or_else(|| snap.items.get(&b.id).map(|i| i.owner_id).filter(|&oid| snap.characters.contains_key(&oid)))
                        })
                        .or_else(|| with_combat_runtime(ctx, |rt| rt.active_drones_in_space.get(&b.id).map(|d| d.owner_char_id)))
                        .or_else(|| ctx.active_fighters.read().ok().and_then(|af| af.get(&b.id).map(|f| f.owner_char_id)))
                        .or_else(|| ctx.active_scanner_probes.read().ok().and_then(|ap| ap.get(&b.id).map(|p| p.owner_char_id)));
                        if let Some(vcid) = victim_char_opt {
                            if crate::services::space_destiny::is_location_change_invulnerable(ctx, vcid, now_unix) {
                                continue;
                            }
                        }
                        if let Ok(Some((applied, destroyed, _))) = crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(ctx, &sys_handle, sys_id, char_id, b.id, sb_dmg, 5.0, now_unix).await {
                            hit_ball_ids.push(b.id);
                            if ctx.active_fighters.read().is_ok_and(|af| af.contains_key(&b.id)) {
                                crate::services::combat_crimewatch::apply_damage_to_fighter_squadron(ctx, sys_id, b.id, sb_dmg).await;
                            }
                            record_killmail_assist_action(ctx, b.id, char_id, attacker_ball.type_id, m.type_id, applied, false, now_unix);
                            if destroyed {
                                if is_probe_b && !destroyed_probe_ids.contains(&b.id) {
                                    destroyed_probe_ids.push(b.id);
                                }
                                if is_drone_b {
                                    with_mut_combat_runtime(ctx, |rt| { rt.active_drones_in_space.remove(&b.id); });
                                    destroyed_drone_ids.push(b.id);
                                }
                            }
                            if let Some(vcid) = victim_char_opt.filter(|&v| v != char_id) {
                                record_killmail_assist_action(ctx, vcid, char_id, attacker_ball.type_id, m.type_id, applied, false, now_unix);
                                if applied > 0.0 {
                                    crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(ctx, char_id, vcid, now_unix);
                                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                    cw.get_or_create(char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
                                    cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
                                }
                                let v_suspect_or_crim = {
                                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                    let st = cw.get_or_create(vcid);
                                    st.is_suspect(now_unix) || st.is_criminal(now_unix)
                                };
                                let is_legal = ctx.db.with_snapshot(|snap| {
                                    is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                }) || v_suspect_or_crim || with_combat_runtime(ctx, |rt| {
                                    rt.has_limited_engagement_with(vcid, char_id, now_unix)
                                });
                                if v_suspect_or_crim || with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(char_id, vcid, now_unix)) {
                                    with_mut_combat_runtime(ctx, |rt| {
                                        rt.add_limited_engagement(char_id, vcid, now_unix);
                                        rt.add_limited_engagement(vcid, char_id, now_unix);
                                    });
                                }
                                if !is_legal {
                                    illegal_capsuleer_hits.push((vcid, b.type_id == 670));
                                }
                            }
                            if let Ok(Some(t_cs)) = sys_handle.get_combat_state(b.id).await {
                                emit_combat_log_and_damage_state(
                                    ctx, char_id, victim_char_opt, ship_id, b.id, m.type_id, applied, false,
                                    (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                                    (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                                    (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                                );
                            }
                            if applied > 0.0 {
                                super::combat_npc_ai::record_npc_threat_event(ctx, b.id, ship_id, applied, false, false);
                            }
                            if destroyed {
                                handle_destroyed_ball_from_missile_or_aoe(ctx, sys_id, char_id, b.id, b.type_id, b.position, now_unix).await;
                            }
                        }
                    }
                }

                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .trigger_weapon_timer(char_id, now_unix);
                let mut concord_triggered = false;
                for (vcid, is_pod) in illegal_capsuleer_hits {
                    if sys_sec > 0.0 {
                        if sys_sec >= 0.45 {
                            concord_triggered = true;
                        }
                        {
                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                            let _ = cw.handle_combat_aggression(
                                char_id,
                                vcid,
                                ship_id,
                                sys_id,
                                sys_sec,
                                false,
                                now_unix,
                            );
                            if sys_sec < 0.45 && is_pod {
                                let st = cw.get_or_create(char_id);
                                st.criminal_flag_until_unix = now_unix.saturating_add(900);
                                st.suspect_flag_until_unix = 0;
                            }
                        }
                        emit_crimewatch_timer_notifications(ctx, char_id, now_unix);
                        let old_sec = ctx.db.with_snapshot(|s| {
                            s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0)
                        });
                        let loss = crate::combat::crimewatch::compute_sec_status_loss(old_sec, is_pod, sys_sec);
                        let new_sec = (old_sec - loss).clamp(-10.0, 10.0);
                        ctx.db.with_mut_snapshot(|s| {
                            if let Some(ch) = s.characters.get_mut(&char_id) {
                                ch.security_status = new_sec;
                            }
                        });
                    }
                }

                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("empFieldRange", PyRep::Real(emp_range)),
                    (
                        "hitBallIDs",
                        PyRep::List(hit_ball_ids.into_iter().map(PyRep::Integer).collect()),
                    ),
                    (
                        "destroyedMissileIDs",
                        PyRep::List(destroyed_missile_ids.into_iter().map(PyRep::Integer).collect()),
                    ),
                    (
                        "destroyedProbeIDs",
                        PyRep::List(destroyed_probe_ids.into_iter().map(PyRep::Integer).collect()),
                    ),
                    (
                        "destroyedDroneIDs",
                        PyRep::List(destroyed_drone_ids.into_iter().map(PyRep::Integer).collect()),
                    ),
                    ("concordTriggered", PyRep::Bool(concord_triggered)),
                ])));
            }

            // 3. Bomb Launcher (`group_id == 862`, `feat:modules.bomb-launcher`, `feat:combat.bomb-damage-void-lockbreaker`)
            if mod_type.group_id == 862 {
                if sys_sec >= 0.45 {
                    return Some(Err(BridgeRpcError {
                        code: "BombDisallowedInHighSec".into(),
                        message: format!(
                            "Bomb launchers cannot be activated in high-security space (system {sys_id} security {sys_sec:.2})"
                        ),
                    }));
                }
                let safety = with_combat_runtime(ctx, |rt| {
                    rt.safety_levels
                        .get(&char_id)
                        .copied()
                        .unwrap_or(SafetyLevel::Red)
                });
                if sys_sec > 0.0 && safety == SafetyLevel::Green {
                    return Some(Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel")));
                }

                let loaded_bomb_opt = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .is_some_and(|t| matches!(t.group_id, 90 | 863 | 864))
                        })
                        .map(|i| (i.item_id, i.type_id))
                });
                let Some((bomb_item_id, bomb_type_id)) = loaded_bomb_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "NoAmmoLoaded".into(),
                        message: "Bomb Launcher has no bomb charge loaded".into(),
                    }));
                };
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(bi) = snap.items.get_mut(&bomb_item_id) {
                        if bi.quantity > 1 {
                            bi.quantity -= 1;
                            bi.version += 1;
                        } else {
                            snap.items.remove(&bomb_item_id);
                        }
                    }
                });

                let bomb_sde = &ctx.sde.types[&bomb_type_id];
                let bomb_vel = bomb_sde.default_attributes.get(&37).copied().unwrap_or(2_500.0);
                let bomb_delay_s =
                    bomb_sde.default_attributes.get(&281).copied().unwrap_or(12_000.0) / 1_000.0;
                let flight_dist = bomb_vel * bomb_delay_s; // 30,000 m (30 km)
                let exp_radius = bomb_sde.default_attributes.get(&654).copied().unwrap_or(400.0).max(1.0);
                let raw_blast_radius = bomb_sde.default_attributes.get(&99).copied().unwrap_or(15_000.0);
                let blast_radius = raw_blast_radius.max(exp_radius);
                let bomb_dmg = DamageVector {
                    em: bomb_sde.default_attributes.get(&114).copied().unwrap_or(0.0),
                    explosive: bomb_sde.default_attributes.get(&116).copied().unwrap_or(0.0),
                    kinetic: bomb_sde.default_attributes.get(&117).copied().unwrap_or(0.0),
                    thermal: bomb_sde.default_attributes.get(&118).copied().unwrap_or(0.0),
                };
                let neut_amount_base = bomb_sde.default_attributes.get(&97).copied().unwrap_or(0.0);
                let is_lockbreaker = bomb_sde.group_id == 863;

                let dir = if attacker_ball.velocity.length() > 1e-3 {
                    attacker_ball.velocity.normalize()
                } else if let Some(tid) = target_id_opt {
                    if let Ok(Some((_, tb))) = sys_handle.get_ball_snapshot(tid).await {
                        let d = tb.position - attacker_ball.position;
                        if d.length() > 1e-3 {
                            d.normalize()
                        } else {
                            DVec3::X
                        }
                    } else {
                        DVec3::X
                    }
                } else {
                    DVec3::X
                };
                let detonation_pos = attacker_ball.position + dir * flight_dist;

                let all_balls = sys_handle.list_balls().await.unwrap_or_default();
                let mut hit_targets = Vec::new();
                let mut total_dmg_applied = 0.0;
                let mut total_cap_neutralized = 0.0;
                let mut locks_broken_targets = Vec::new();

                for b in all_balls {
                    if !is_combatant_ship_or_entity_ball(ctx, b.id, b.type_id) {
                        continue;
                    }
                    if b.position.distance(detonation_pos) <= blast_radius {
                        let bomb_vic = ctx.db.with_snapshot(|snap| {
                            snap.characters.values().find(|c| c.active_ship_id == b.id).map(|c| c.character_id)
                                .or_else(|| snap.items.get(&b.id).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)))
                        })
                        .or_else(|| ctx.active_fighters.read().ok().and_then(|af| af.get(&b.id).map(|f| f.owner_char_id)));
                        if let Some(vcid) = bomb_vic {
                            if crate::services::space_destiny::is_location_change_invulnerable(ctx, vcid, now_unix) {
                                continue;
                            }
                        }
                        let target_sig = if let Ok(Some(cs)) = sys_handle.get_combat_state(b.id).await {
                            if cs.signature_radius > 0.0 {
                                cs.signature_radius
                            } else {
                                b.radius.max(35.0)
                            }
                        } else {
                            b.radius.max(35.0)
                        };
                        let sig_factor = (target_sig / exp_radius).min(1.0);
                        let scaled_dmg = bomb_dmg.scaled(sig_factor);
                        if scaled_dmg.total() > 0.0 {
                            if let Ok(Some((app, destroyed, _))) = crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(ctx, &sys_handle, sys_id, char_id, b.id, scaled_dmg, 12.0, now_unix).await {
                                if ctx.active_fighters.read().is_ok_and(|af| af.contains_key(&b.id)) {
                                    crate::services::combat_crimewatch::apply_damage_to_fighter_squadron(ctx, sys_id, b.id, scaled_dmg).await;
                                }
                                total_dmg_applied += app;
                                hit_targets.push(b.id);
                                record_killmail_assist_action(
                                    ctx,
                                    b.id,
                                    char_id,
                                    attacker_ball.type_id,
                                    bomb_type_id,
                                    app,
                                    false,
                                    now_unix,
                                );
                                if app > 0.0 {
                                    if let Some(vcid) = bomb_vic.filter(|&v| v != char_id) {
                                        record_killmail_assist_action(
                                            ctx,
                                            vcid,
                                            char_id,
                                            attacker_ball.type_id,
                                            bomb_type_id,
                                            app,
                                            false,
                                            now_unix,
                                        );
                                        crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(ctx, char_id, vcid, now_unix);
                                        {
                                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                            cw.get_or_create(char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
                                            cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
                                        }
                                        let v_suspect_or_crim = {
                                            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                            let st = cw.get_or_create(vcid);
                                            st.is_suspect(now_unix) || st.is_criminal(now_unix)
                                        };
                                        let is_legal = ctx.db.with_snapshot(|snap| {
                                            is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                        }) || v_suspect_or_crim || with_combat_runtime(ctx, |rt| {
                                            rt.has_limited_engagement_with(vcid, char_id, now_unix)
                                        });
                                        if v_suspect_or_crim || with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(char_id, vcid, now_unix)) {
                                            with_mut_combat_runtime(ctx, |rt| {
                                                rt.add_limited_engagement(char_id, vcid, now_unix);
                                                rt.add_limited_engagement(vcid, char_id, now_unix);
                                            });
                                        }
                                        if !is_legal {
                                            if sys_sec > 0.0 {
                                                let is_pod = b.type_id == 670;
                                                {
                                                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                                                    let _ = cw.handle_combat_aggression(
                                                        char_id,
                                                        vcid,
                                                        ship_id,
                                                        sys_id,
                                                        sys_sec,
                                                        false,
                                                        now_unix,
                                                    );
                                                    if sys_sec < 0.45 && is_pod {
                                                        let st = cw.get_or_create(char_id);
                                                        st.criminal_flag_until_unix = now_unix.saturating_add(900);
                                                        st.suspect_flag_until_unix = 0;
                                                    }
                                                }
                                                let old_sec = ctx.db.with_snapshot(|s| {
                                                    s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0)
                                                });
                                                let loss = crate::combat::crimewatch::compute_sec_status_loss(old_sec, is_pod, sys_sec);
                                                let new_sec = (old_sec - loss).clamp(-10.0, 10.0);
                                                ctx.db.with_mut_snapshot(|s| {
                                                    if let Some(ch) = s.characters.get_mut(&char_id) {
                                                        ch.security_status = new_sec;
                                                    }
                                                });
                                            }
                                        }
                                    }
                                }
                                if let Ok(Some(t_cs)) = sys_handle.get_combat_state(b.id).await {
                                    emit_combat_log_and_damage_state(
                                        ctx,
                                        char_id,
                                        bomb_vic,
                                        ship_id,
                                        b.id,
                                        bomb_type_id,
                                        app,
                                        false,
                                        (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                                        (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                                        (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                                    );
                                }
                                if app > 0.0 {
                                    super::combat_npc_ai::record_npc_threat_event(ctx, b.id, ship_id, app, false, false);
                                }
                                if destroyed {
                                    handle_destroyed_ball_from_missile_or_aoe(
                                        ctx,
                                        sys_id,
                                        char_id,
                                        b.id,
                                        b.type_id,
                                        b.position,
                                        ctx.now_unix(),
                                    )
                                    .await;
                                }
                            }
                        }
                        if neut_amount_base > 0.0 {
                            if let Ok(Some(mut cs)) = sys_handle.get_combat_state(b.id).await {
                                let drain = (neut_amount_base * sig_factor).min(cs.capacitor);
                                cs.capacitor = (cs.capacitor - drain).max(0.0);
                                total_cap_neutralized += drain;
                                let _ = sys_handle.set_combat_state(b.id, cs).await;
                            }
                        }
                        if is_lockbreaker {
                            let cleared = sys_handle.clear_target_locks(b.id).await.unwrap_or(0);
                            if cleared > 0 {
                                locks_broken_targets.push(b.id);
                            }
                            if let Some(vcid) = ctx.db.with_snapshot(|snap| {
                                snap.characters
                                    .values()
                                    .find(|c| c.active_ship_id == b.id)
                                    .map(|c| c.character_id)
                            }) {
                                ctx.notify(
                                    NotificationTarget::Character(vcid),
                                    "OnTargetClear",
                                    "charid",
                                    vec![],
                                );
                            }
                        }
                    }
                }

                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .trigger_weapon_timer(char_id, now_unix);
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("bombTypeID", PyRep::Integer(bomb_type_id as i64)),
                    ("flightDistanceMeters", PyRep::Real(flight_dist)),
                    ("blastRadiusMeters", PyRep::Real(blast_radius)),
                    ("explosionRadiusMeters", PyRep::Real(exp_radius)),
                    ("totalDamageApplied", PyRep::Real(total_dmg_applied)),
                    ("totalCapNeutralized", PyRep::Real(total_cap_neutralized)),
                    (
                        "hitTargetIDs",
                        PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect()),
                    ),
                    (
                        "locksBrokenTargetIDs",
                        PyRep::List(locks_broken_targets.into_iter().map(PyRep::Integer).collect()),
                    ),
                ])));
            }

            // 4. Defender Launcher (`group_id == 512`, `feat:modules.missile-defender`, `feat:combat.missile-flight-physics-destruction`)
            if mod_type.group_id == 512 {
                let loaded_def_opt = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 88)
                        })
                        .map(|i| (i.item_id, i.type_id))
                });
                let Some((def_item_id, def_type_id)) = loaded_def_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "NoAmmoLoaded".into(),
                        message: "Defender Launcher has no Defender Missile loaded".into(),
                    }));
                };
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(di) = snap.items.get_mut(&def_item_id) {
                        if di.quantity > 1 {
                            di.quantity -= 1;
                            di.version += 1;
                        } else {
                            snap.items.remove(&def_item_id);
                        }
                    }
                });
                let intercepted_missile_id = with_mut_combat_runtime(ctx, |rt| {
                    let cand_id = target_id_opt
                        .filter(|tid| rt.in_flight_missiles.contains_key(tid))
                        .or_else(|| {
                            rt.in_flight_missiles
                                .values()
                                .find(|ms| {
                                    ms.solar_system_id == sys_id
                                        && !ms.destroyed
                                        && !ms.expired
                                        && !ms.impacted
                                })
                                .map(|ms| ms.missile_ball_id)
                        });
                    if let Some(mid) = cand_id {
                        if let Some(ms) = rt.in_flight_missiles.get_mut(&mid) {
                            ms.hp = 0.0;
                            ms.destroyed = true;
                            ms.applied_damage = 0.0;
                        }
                    }
                    cand_id
                });
                if let Some(mid) = intercepted_missile_id {
                    let _ = sys_handle.remove_ball(mid).await;
                }
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("defenderTypeID", PyRep::Integer(def_type_id as i64)),
                    (
                        "interceptedMissileID",
                        intercepted_missile_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                ])));
            }

    None
}

#[allow(clippy::too_many_arguments)]
pub async fn activate_missile_or_breacher_launcher(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    now_unix: u64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    fit: &eve_dogma::ShipFittingEngine,
    charge_type_id: u32,
    charge_type: &eve_sde::InvType,
    attacker_ball: &eve_destiny::Ball,
    target_ball_id: i64,
    base_dmg: DamageVector,
    is_fof_missile: bool,
    is_breacher_pod: bool,
    has_explicit_target: bool,
) -> Result<PyRep, BridgeRpcError> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                let missile_vel = {
                    let v = fit.get_charge_effective_attribute(module_id, 37, &ctx.sde);
                    if v > 0.0 { v } else { charge_type.default_attributes.get(&37).copied().unwrap_or(3_750.0) }
                };
                let flight_time_s = {
                    let v = fit.get_charge_effective_attribute(module_id, 281, &ctx.sde);
                    let ms = if v > 0.0 { v } else { charge_type.default_attributes.get(&281).copied().unwrap_or(5_000.0) };
                    ms / 1_000.0
                };
                let missile_hp = charge_type.default_attributes.get(&9).copied().unwrap_or(60.0);
                let exp_vel = {
                    let v = fit.get_charge_effective_attribute(module_id, 653, &ctx.sde);
                    if v > 0.0 { v } else { charge_type.default_attributes.get(&653).copied().unwrap_or(170.0) }
                };
                let exp_rad = {
                    let v = fit.get_charge_effective_attribute(module_id, 654, &ctx.sde);
                    if v > 0.0 { v } else { charge_type.default_attributes.get(&654).copied().unwrap_or(40.0) }
                };
                let drf = {
                    let v = fit.get_charge_effective_attribute(module_id, 1353, &ctx.sde);
                    if v > 0.0 { v } else { charge_type.default_attributes.get(&1353).copied().unwrap_or(0.604) }
                };
                let dot_dur_s = charge_type.default_attributes.get(&5735).copied().unwrap_or(40_000.0) / 1_000.0;
                let dot_flat = charge_type.default_attributes.get(&5736).copied().unwrap_or(200.0);
                let dot_pct = charge_type.default_attributes.get(&5737).copied().unwrap_or(0.6) / 100.0;

                let missile_ball_id = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    snap.next_entity_id
                });
                let _ = sys_handle
                    .spawn_ball_with_combat(
                        eve_destiny::Ball {
                            id: missile_ball_id,
                            type_id: charge_type_id,
                            position: attacker_ball.position,
                            velocity: DVec3::ZERO,
                            max_velocity: missile_vel,
                            inertia: 0.1,
                            mass: charge_type.mass.max(1.0),
                            radius: 5.0,
                            speed_fraction: 1.0,
                            mode: eve_destiny::MovementMode::Follow {
                                target_id: target_ball_id,
                                range: 0.0,
                            },
                        },
                        eve_destiny::CombatState {
                            shield_hp: 0.0,
                            max_shield_hp: 0.0,
                            armor_hp: 0.0,
                            max_armor_hp: 0.0,
                            structure_hp: missile_hp,
                            max_structure_hp: missile_hp,
                            signature_radius: 5.0,
                            ..eve_destiny::CombatState::default()
                        },
                    )
                    .await;

                let in_flight = InFlightMissileState {
                    missile_ball_id,
                    solar_system_id: sys_id,
                    attacker_char_id: char_id,
                    attacker_ship_id: ship_id,
                    launcher_module_id: module_id,
                    launcher_type_id: m.type_id,
                    missile_type_id: charge_type_id,
                    target_ball_id,
                    max_velocity: missile_vel,
                    flight_time_s,
                    remaining_flight_s: flight_time_s,
                    hp: missile_hp,
                    max_hp: missile_hp,
                    base_damage: base_dmg,
                    explosion_radius: exp_rad,
                    explosion_velocity: exp_vel,
                    drf,
                    is_fof: is_fof_missile,
                    is_breacher_pod,
                    breacher_dot_duration_s: dot_dur_s,
                    breacher_flat_cap: dot_flat,
                    breacher_pct_cap: dot_pct,
                    destroyed: false,
                    expired: false,
                    impacted: false,
                    applied_damage: 0.0,
                };
                with_mut_combat_runtime(ctx, |rt| {
                    rt.in_flight_missiles.insert(missile_ball_id, in_flight);
                });

                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .trigger_weapon_timer(char_id, now_unix);
                let defer_impact = is_fof_missile && has_explicit_target;
                if defer_impact {
                    return Ok(PyRep::keyval([
                        ("moduleID", PyRep::Integer(module_id)),
                        ("missileBallID", PyRep::Integer(missile_ball_id)),
                        ("missileTypeID", PyRep::Integer(charge_type_id as i64)),
                        ("targetID", PyRep::Integer(target_ball_id)),
                        ("maxVelocity", PyRep::Real(missile_vel)),
                        ("flightTimeSeconds", PyRep::Real(flight_time_s)),
                        ("missileHP", PyRep::Real(missile_hp)),
                        ("inFlight", PyRep::Bool(true)),
                    ]));
                }

                // Simulate full missile flight immediately when deferImpact is false
                let _ = step_missiles_and_breachers(ctx, sys_id, flight_time_s + 0.01).await;
                let resolved_ms = with_combat_runtime(ctx, |rt| {
                    rt.in_flight_missiles.get(&missile_ball_id).cloned()
                })
                .unwrap();

                let mut dot_tick_applied = 0.0;
                if is_breacher_pod && resolved_ms.impacted {
                    let _ = step_missiles_and_breachers(ctx, sys_id, 1.0).await;
                    dot_tick_applied = with_combat_runtime(ctx, |rt| {
                        rt.active_breacher_dots
                            .get(&target_ball_id)
                            .map(|d| d.tick_damage)
                            .unwrap_or(0.0)
                    });
                }

                let total_applied = resolved_ms.applied_damage + dot_tick_applied;
                record_killmail_assist_action(
                    ctx,
                    target_ball_id,
                    char_id,
                    attacker_ball.type_id,
                    m.type_id,
                    total_applied,
                    false,
                    now_unix,
                );
                return Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("targetID", PyRep::Integer(target_ball_id)),
                    ("missileBallID", PyRep::Integer(missile_ball_id)),
                    ("missileTypeID", PyRep::Integer(charge_type_id as i64)),
                    ("appliedDamage", PyRep::Real(total_applied)),
                    ("impacted", PyRep::Bool(resolved_ms.impacted)),
                    ("expired", PyRep::Bool(resolved_ms.expired)),
                    ("destroyedInFlight", PyRep::Bool(resolved_ms.destroyed)),
                    ("isFoF", PyRep::Bool(is_fof_missile)),
                    ("isBreacherPod", PyRep::Bool(is_breacher_pod)),
                    ("breacherTickDPS", PyRep::Real(dot_tick_applied)),
                ]));
}
