//! Helper routines for Round-6 Track 1 P1 combat, wormhole, and fighter fixes (`#363`..`#369`).

use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

use super::combat_crimewatch::{combat_missiles_aoe, combat_wrecks_killmails};
use crate::router::ServerContext;
use crate::system_actor::SystemHandle;

/// Validates fighter MWD SDE ability presence, cooldown, and charge count, then activates MWD (`#364`).
pub(crate) async fn validate_and_activate_fighter_mwd(
    ctx: &ServerContext,
    char_id: i64,
    sys_handle: &SystemHandle,
    fighter_ids: &[i64],
) -> Result<(), BridgeRpcError> {
    use crate::system_actor::{FighterAbilityActorOp, FighterMwdSpec};

    if fighter_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "NoFightersSelected".into(),
            message: "MWD movement command requires at least one fighter squadron".into(),
            client_error: None,
        });
    }
    let now_unix = ctx.now_unix();
    let specs: Vec<FighterMwdSpec> = {
        let view = sys_handle.view();
        let active = &view.fighters.active_fighters;
        let mut specs = Vec::with_capacity(fighter_ids.len());
        for &fid in fighter_ids {
            let Some(f) = active.get(&fid) else {
                return Err(BridgeRpcError {
                    code: "FighterNotFound".into(),
                    message: format!("Fighter squadron {fid} not found or not launched"),
                    client_error: None,
                });
            };
            if f.owner_char_id != char_id || f.is_abandoned {
                return Err(BridgeRpcError {
                    code: "FighterNotOwned".into(),
                    message: format!("Fighter squadron {fid} is not controlled by pilot"),
                    client_error: None,
                });
            }
            let ftype_id = f.type_id;
            let Some(slots) = ctx.sde.fighter_abilities_by_type.get(&ftype_id) else {
                return Err(BridgeRpcError {
                    code: "UnsupportedFighterAbility".into(),
                    message: format!(
                        "Fighter type {ftype_id} has no defined fighter abilities in SDE"
                    ),
                    client_error: None,
                });
            };
            let Some(mwd_slot) = slots.iter().find(|s| matches!(s.ability_id, 4 | 9 | 13)) else {
                return Err(BridgeRpcError {
                    code: "UnsupportedFighterAbility".into(),
                    message: format!(
                        "Fighter type {ftype_id} does not support MicroWarpdrive ability"
                    ),
                    client_error: None,
                });
            };
            let slot_id = mwd_slot.slot_index as u32;
            let cooldown_secs = mwd_slot
                .cooldown_seconds
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&ftype_id)
                        .and_then(|t| t.default_attributes.get(&2157).copied())
                        .map(|ms| ms / 1_000.0)
                })
                .unwrap_or(20.0)
                .max(1.0) as u64;
            let speed_bonus_pct = ctx
                .sde
                .types
                .get(&ftype_id)
                .and_then(|t| {
                    t.default_attributes
                        .get(&2151)
                        .or_else(|| t.default_attributes.get(&2124))
                        .or_else(|| t.default_attributes.get(&2224))
                        .copied()
                })
                .unwrap_or(400.0);
            specs.push(FighterMwdSpec {
                fighter_ball_id: fid,
                cooldown_secs,
                slot_id,
                max_charges: mwd_slot.charges.as_ref().map(|c| c.charge_count),
                speed_bonus_pct,
            });
        }
        specs
    };

    sys_handle
        .mutate_fighter_ability_state(FighterAbilityActorOp::ActivateMwdBatch {
            char_id,
            specs,
            now_unix,
        })
        .await
        .map_err(|e| e.into_bridge_err())?;

    Ok(())
}

fn has_combat_state_invuln_or_tether(cs: &CombatState) -> bool {
    cs.blocks_area_effect()
}

/// Checks whether a target entity is currently invulnerable or tethered (`#366`, `#1486`).
#[cfg(test)]
pub(crate) fn is_target_invulnerable_or_tethered(
    _ctx: &ServerContext,
    _target_id: i64,
    cs: &CombatState,
    _now_unix: u64,
) -> bool {
    has_combat_state_invuln_or_tether(cs)
}

/// System-scoped check whether a target entity is currently invulnerable or tethered (`#366`, `#1486`).
pub(crate) fn is_target_invulnerable_or_tethered_in_system(
    _ctx: &ServerContext,
    _solar_system_id: u32,
    _target_id: i64,
    cs: &CombatState,
    _now_unix: u64,
) -> bool {
    has_combat_state_invuln_or_tether(cs)
}

/// Advances all active Breacher Pod DoT effects with tether/invuln checks and killmail/notification emission (`#366`).
pub(crate) async fn step_active_breacher_dots(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    solar_system_id: u32,
    dt: f64,
) {
    let dots = sys_handle.active_breacher_dots_snapshot().await;
    for mut dot in dots {
        let tid = dot.target_ball_id;
        let cs = match sys_handle.get_combat_state(tid).await {
            Ok(Some(cs)) => cs,
            Ok(None) => {
                let _ = sys_handle.remove_active_breacher_dot(tid).await;
                continue;
            }
            Err(_) => continue,
        };
        let now_unix = ctx.now_unix();
        if is_target_invulnerable_or_tethered_in_system(ctx, solar_system_id, tid, &cs, now_unix) {
            dot.remaining_s -= dt;
            if dot.remaining_s <= 1e-6 {
                let _ = sys_handle.remove_active_breacher_dot(tid).await;
            } else {
                let _ = sys_handle.upsert_active_breacher_dot(dot).await;
            }
            continue;
        }

        let total_max_hp = (cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp).max(1.0);
        let dps = crate::formulas::breacher_pod_tick_damage(
            dot.flat_cap_dps,
            dot.pct_cap_fraction,
            total_max_hp,
        );
        let step_s = dt.min(dot.remaining_s).max(0.0);
        let raw_dmg = dps * step_s;
        dot.tick_damage = dps;

        let (applied_dmg, destroyed_by_dot, ball_snap_opt) = sys_handle
            .apply_true_damage(tid, raw_dmg)
            .await
            .ok()
            .flatten()
            .map(|(app, dest, b)| (app, dest, Some(b)))
            .unwrap_or((0.0, false, None));
        let (shield_ratio, armor_ratio, hull_ratio) = if destroyed_by_dot {
            (0.0, 0.0, 0.0)
        } else if let Ok(Some(after_cs)) = sys_handle.get_combat_state(tid).await {
            let sr = if after_cs.max_shield_hp > 0.0 {
                (after_cs.shield_hp / after_cs.max_shield_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let ar = if after_cs.max_armor_hp > 0.0 {
                (after_cs.armor_hp / after_cs.max_armor_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let hr = if after_cs.max_structure_hp > 0.0 {
                (after_cs.structure_hp / after_cs.max_structure_hp).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (sr, ar, hr)
        } else {
            (0.0, 0.0, 0.0)
        };

        if applied_dmg > 0.0 && dot.source_char_id > 0 {
            let (attacker_ship_type, victim_owner_char) = ctx.db.with_snapshot(|snap| {
                let s_type = snap.items.get(&dot.source_ship_id).map(|i| i.type_id).unwrap_or(0);
                let v_char = snap
                    .characters
                    .values()
                    .find(|c| c.active_ship_id == tid)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.items
                            .get(&tid)
                            .and_then(|it| snap.characters.contains_key(&it.owner_id).then_some(it.owner_id))
                    });
                (s_type, v_char)
            });
            combat_wrecks_killmails::record_killmail_assist_action(
                ctx,
                tid,
                dot.source_char_id,
                attacker_ship_type,
                dot.weapon_type_id.max(1),
                applied_dmg,
                false,
                now_unix,
            );
            crate::services::station_structure::structure_lifecycle_rpc::record_ship_damage_progression(
                ctx,
                solar_system_id,
                dot.source_char_id,
                tid,
                applied_dmg,
            );
            super::combat_crimewatch::emit_combat_log_and_damage_state(
                ctx,
                dot.source_char_id,
                victim_owner_char,
                dot.source_ship_id,
                tid,
                dot.weapon_type_id,
                applied_dmg,
                false,
                shield_ratio,
                armor_ratio,
                hull_ratio,
                Some(solar_system_id),
            );
        }

        dot.remaining_s -= dt;
        if destroyed_by_dot || dot.remaining_s <= 1e-6 {
            let _ = sys_handle.remove_active_breacher_dot(tid).await;
        } else {
            let _ = sys_handle.upsert_active_breacher_dot(dot.clone()).await;
        }
        if destroyed_by_dot {
            let (ball_type_id, ball_pos) = ball_snap_opt
                .map(|b| (b.type_id, b.position))
                .unwrap_or_else(|| {
                    let stid = ctx
                        .db
                        .with_snapshot(|snap| snap.items.get(&tid).map(|i| i.type_id).unwrap_or(0));
                    (stid, DVec3::ZERO)
                });
            combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                ctx,
                solar_system_id,
                dot.source_char_id,
                tid,
                ball_type_id,
                ball_pos,
                now_unix,
            )
            .await;
        }
    }
}

/// Applies captured pre-jump ship combat state onto the newly spawned destination state (`#368`).
pub(crate) fn apply_preserved_combat_state(dest: &mut CombatState, prev_opt: Option<CombatState>) {
    let Some(prev) = prev_opt else {
        return;
    };
    let s_frac = (prev.shield_hp / prev.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
    let a_frac = (prev.armor_hp / prev.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
    let h_frac = (prev.structure_hp / prev.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
    let c_frac = (prev.capacitor / prev.max_capacitor.max(1.0)).clamp(0.0, 1.0);
    dest.shield_hp = (dest.max_shield_hp * s_frac).min(dest.max_shield_hp);
    dest.armor_hp = (dest.max_armor_hp * a_frac).min(dest.max_armor_hp);
    dest.structure_hp = (dest.max_structure_hp * h_frac).min(dest.max_structure_hp);
    dest.capacitor = (dest.max_capacitor * c_frac).min(dest.max_capacitor);
    dest.active_breacher_dot = prev.active_breacher_dot;
    dest.entropic_spool_cycles = prev.entropic_spool_cycles;
}

/// Spawns a distinct exit wormhole ball in `dest_sys_id` and records its reverse link (`#368`).
pub(crate) async fn spawn_distinct_exit_wormhole(
    ctx: &ServerContext,
    new_handle: &SystemHandle,
    old_sys: u32,
    dest_sys_id: u32,
    entry_wh_ball_id: i64,
    entry_wh_pos: DVec3,
    dest_sig_pos: DVec3,
) -> i64 {
    let mut exit_wh_id = crate::services::wormhole_lifecycle_ops::wormhole_exit_item_id_for_signature(
        dest_sys_id,
        &format!("K162-{entry_wh_ball_id}"),
    );
    if exit_wh_id == entry_wh_ball_id {
        exit_wh_id = entry_wh_ball_id.wrapping_add(1);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&dest_sys_id) {
            if st.wormhole_destination_system_id.is_none() {
                st.wormhole_destination_system_id = Some(old_sys);
                st.wormhole_remaining_mass_kg = st.wormhole_remaining_mass_kg.max(1_000_000_000.0);
                st.signature_position.get_or_insert(entry_wh_pos.to_array());
            }
        }
    });
    let _ = new_handle
        .spawn_ball(Ball {
            id: exit_wh_id,
            type_id: 26272,
            position: dest_sig_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    exit_wh_id
}

/// Parses fighter IDs, ability slot ID, and finite 3D coordinates for point-targeted abilities (`#365`).
pub(crate) fn parse_fighter_point_target_args(
    args: &[PyRep],
) -> Result<(Vec<i64>, u32, DVec3), BridgeRpcError> {
    let rpc = crate::router::RpcArgs::positional(args);
    let fighter_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items
            .iter()
            .filter_map(|i| i.as_i64())
            .filter(|&id| id > 0)
            .collect(),
        Some(other) => other.as_i64().filter(|&id| id > 0).into_iter().collect(),
        None => Vec::new(),
    };
    if fighter_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "FighterNotFound".into(),
            message: "ActivateFighterAbilitySlotsOnPoint requires at least one valid fighterID".into(),
            client_error: None,
        });
    }
    let slot_id = rpc.req_u32(1, "slotID").map_err(|e| BridgeRpcError {
        code: "InvalidFighterAbilitySlot".into(),
        message: e.message,
        client_error: None,
    })?;
    let target_point = rpc.req_finite_vec3(2).map_err(|e| BridgeRpcError {
        code: "InvalidTargetPoint".into(),
        message: e.message,
        client_error: None,
    })?;
    Ok((fighter_ids, slot_id, target_point))
}

#[cfg(test)]
#[path = "r6_t1_p1_helpers_cov_unit_tests.rs"]
mod r6_t1_p1_helpers_cov_unit_tests;


