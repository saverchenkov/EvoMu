//! Pending module cycle-end and repeating-pulse action execution split from
//! `fitting_module_cycles_helpers` so async futures stay cleanly within `clippy::large_futures` (`#1487`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::system_actor::{PendingCycleAction, SystemHandle};
use eve_db::InventoryFlag;
use eve_dogma::{DogmaItemState, FittedModule, ModuleState};
use eve_proto::PyRep;
use std::collections::HashMap;

use super::{
    emit_charge_sublocation_quantity_change, emit_godma_charge_sublocation_prime,
    run_pre_notify_instant_module_effects,
};

#[inline(never)]
pub(crate) async fn deactivate_repeating_cycle_after_action(
    ctx: &ServerContext,
    sys: &SystemHandle,
    owner_char_id: Option<i64>,
    ship_id: i64,
    module_id: i64,
    effect_name: &str,
) {
    let _ = deactivate_repeating_cycle_after_action_with_policy(
        ctx,
        sys,
        owner_char_id,
        ship_id,
        module_id,
        effect_name,
        false,
        None,
    )
    .await;
}

/// Stops a repeating module cycle after an action completes, optionally suppressing duplicate
/// `OnGodmaShipEffect(false)` notifications if the actor already removed the cycle. Returns the
/// resolved SDE effect ID when `OnGodmaShipEffect(false)` was emitted.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn deactivate_repeating_cycle_after_action_with_policy(
    ctx: &ServerContext,
    sys: &SystemHandle,
    owner_char_id: Option<i64>,
    ship_id: i64,
    module_id: i64,
    effect_name: &str,
    skip_notify_if_absent: bool,
    module_type_id_opt: Option<u32>,
) -> Option<i64> {
    let removed_from_actor = sys
        .remove_module_cycle(ship_id, module_id)
        .await
        .ok()
        .flatten()
        .is_some();
    let _ = crate::services::notify_emitters_space_dungeons::flush_module_ewar_end(
        ctx,
        owner_char_id.unwrap_or(0),
        Some(sys.solar_system_id),
        ship_id,
        module_id,
    );
    crate::services::tactical_modules::remove_projected_remote_assistance(ctx, ship_id, module_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&module_id);
        snap.module_active_charges.remove(&module_id);
        crate::router::ServerContext::clear_persisted_module_overload(snap, ship_id, module_id);
    });
    let cid = owner_char_id?;
    ctx.invalidate_and_push_fit(ship_id);
    if skip_notify_if_absent && !removed_from_actor {
        return None;
    }
    let resolved_effect_id = module_type_id_opt.and_then(|mod_type_id| {
        crate::router::binding::effect_id_for_module_sde(&ctx.sde, mod_type_id, Some(effect_name))
    });
    let godma_effect = if effect_name.starts_with("superWeapon") {
        let mod_type_id = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&module_id).map(|i| i.type_id))
            .unwrap_or(0);
        let eff_id = crate::router::binding::effect_id_for_module_sde(
            &ctx.sde,
            mod_type_id,
            Some(effect_name),
        )
        .unwrap_or(4489);
        PyRep::Integer(eff_id)
    } else if let Some(eff_id) = resolved_effect_id.filter(|&id| id > 0) {
        PyRep::Integer(eff_id)
    } else {
        PyRep::String(effect_name.to_string())
    };
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(cid),
        crate::router::notify_registry::OnGodmaShipEffect {
            item_id: module_id,
            effect: godma_effect,
            start_or_time: crate::router::notify_registry::GodmaEffectStartOrTime::Flag(false),
            start: None,
            active: None,
            environment: None,
            start_time: None,
            duration: None,
            repeat: None,
            error: None,
            actual_stop_time: None,
        },
    );
    Some(resolved_effect_id.unwrap_or(0))
}

#[allow(clippy::too_many_arguments)]
fn emit_salvage_stop_special_fx(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    wreck_id: i64,
    eff_id: i64,
    effect_name: &str,
) {
    let Some((sfx_guid, is_offensive)) = crate::router::binding::special_fx_guid_for_module_sde(
        &ctx.sde,
        module_type_id,
        eff_id,
        Some(effect_name),
    ) else {
        return;
    };
    let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id) else {
        return;
    };
    let now = ctx.now_system_sim_filetime_for_ship(char_id, ship_id);
    let sfx_notif = crate::router::notify_registry::OnSpecialFX {
        scene,
        ship_id,
        module_id: Some(module_id),
        module_type_id: Some(i64::from(module_type_id)),
        target_id: (wreck_id > 0).then_some(wreck_id),
        other_type_id: None,
        guid: sfx_guid,
        is_offensive,
        start: 0,
        active: 0,
        duration: 0.0,
        repeat: 0,
        start_time: now,
        time_from_start: 0.0,
        graphic_info: None,
    };
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        sfx_notif.clone(),
    );
    for (obs_cid, _obs_uid) in ctx.space_observer_chars_in_system(sys_id, char_id) {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(obs_cid),
            sfx_notif.clone(),
        );
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_salvage_end_action(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    wreck_id: i64,
    repeat: i32,
    effect_name: String,
    next_cycle_cap_deducted: bool,
    deducted_cap_gj: f64,
) {
    let res = crate::combat::drones_salvage_tractor::execute_wreck_salvage_cycle_for_module(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        Some(module_type_id),
        (repeat != 0).then_some(module_id),
    )
    .await;
    if repeat == 0 {
        drop(res);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&module_id);
            snap.module_active_charges.remove(&module_id);
        });
        ctx.invalidate_and_push_fit(ship_id);
        return;
    }
    let cycle_pre_removed = matches!(&res, Ok((_, true)));
    let keep_repeating = matches!(
        &res,
        Ok((rep, _))
            if rep.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(false)
                && rep.dict_get("reason").and_then(|v| v.as_str()) == Some("CycleRollFailed")
    );
    drop(res);
    if keep_repeating {
        let dur_s = ctx
            .sde
            .types
            .get(&module_type_id)
            .and_then(|t| t.default_attributes.get(&73).copied())
            .unwrap_or(10_000.0)
            / 1000.0;
        ctx.record_module_reactivation_delay(ship_id, module_id, dur_s);
        return;
    }
    if let Some(eff_id) = deactivate_repeating_cycle_after_action_with_policy(
        ctx,
        sys,
        Some(char_id),
        ship_id,
        module_id,
        &effect_name,
        !cycle_pre_removed,
        Some(module_type_id),
    )
    .await
    {
        emit_salvage_stop_special_fx(
            ctx,
            sys_id,
            char_id,
            ship_id,
            module_id,
            module_type_id,
            wreck_id,
            eff_id,
            &effect_name,
        );
    }
    if next_cycle_cap_deducted && deducted_cap_gj > 0.0 {
        if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
            let _ = sys
                .set_capacitor(ship_id, (cs.capacitor + deducted_cap_gj).min(cs.max_capacitor))
                .await;
        }
    }
}

/// Executes cycle-end wreck salvaging (`group 1122`) and auto-repeating (`repeat != 0`) AoE / tractor
/// cycle-start pulses queued by `step_ballpark_and_cycles` (`R-AOE-SALVAGE-CYCLE-01`).
pub(crate) async fn execute_pending_cycle_actions(ctx: &ServerContext, sys_id: u32) {
    let actions = ctx.systems.take_pending_cycle_actions(sys_id);
    execute_pending_cycle_actions_list(ctx, sys_id, actions).await;
}

pub(crate) fn emit_ship_charge_sublocation_diffs(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    diffs: &[(
        Option<eve_db::InventoryItemRecord>,
        Option<eve_db::InventoryItemRecord>,
    )],
) {
    let is_fitted_slot = |f: InventoryFlag| matches!(f as u16, 11..=34 | 125..=132);
    for (pre, post) in diffs {
        match (pre, post) {
            (Some(old_it), Some(new_it)) if is_fitted_slot(new_it.flag) => {
                if !is_fitted_slot(old_it.flag) {
                    emit_godma_charge_sublocation_prime(ctx, char_id, ship_id, new_it);
                }
                let old_qty = if old_it.flag == new_it.flag {
                    old_it.quantity
                } else {
                    0
                };
                if old_qty != new_it.quantity {
                    emit_charge_sublocation_quantity_change(
                        ctx,
                        char_id,
                        ship_id,
                        new_it.flag,
                        new_it.type_id,
                        old_qty,
                        new_it.quantity,
                    );
                }
            }
            (None, Some(new_it)) if is_fitted_slot(new_it.flag) => {
                emit_godma_charge_sublocation_prime(ctx, char_id, ship_id, new_it);
                emit_charge_sublocation_quantity_change(
                    ctx,
                    char_id,
                    ship_id,
                    new_it.flag,
                    new_it.type_id,
                    0,
                    new_it.quantity,
                );
            }
            (Some(old_it), None) if is_fitted_slot(old_it.flag) => {
                emit_charge_sublocation_quantity_change(
                    ctx,
                    char_id,
                    ship_id,
                    old_it.flag,
                    old_it.type_id,
                    old_it.quantity,
                    0,
                );
            }
            _ => {}
        }
    }
}

fn emit_cycle_weapon_log_pair(
    ctx: &ServerContext,
    recipient_cid: i64,
    msg_key: &str,
    dict: PyRep,
    scene: crate::router::notify_registry::BallparkScene,
) {
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(recipient_cid),
        crate::router::notify_registry::OnDamageMessage {
            scene,
            damage_messages_args: dict.clone(),
        },
    );
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(recipient_cid),
        crate::router::notify_registry::OnDamageMessages {
            scene,
            dmgmsgs: vec![PyRep::Tuple(vec![
                PyRep::String(msg_key.into()),
                dict,
            ])],
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_cycle_weapon_log_entries(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: Option<i64>,
    attacker_ship_id: i64,
    target_id: i64,
    target_char_id: Option<i64>,
    module_type_id: u32,
    hit_damage: Option<f64>,
) {
    let (damage, hit_quality, attacker_msg_key, victim_msg_key) = match hit_damage {
        Some(dmg) => (dmg, 3, "AttackHit", "AttackedBy"),
        None => (0.0, 0, "AttackMiss", "AttackMissBy"),
    };
    let owner_cid = attacker_char_id.unwrap_or(0);
    if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
        let dmg_dict = crate::system_actor::build_cycle_damage_message_dict(
            damage,
            attacker_ship_id,
            target_id,
            acid,
            module_type_id,
            hit_quality,
            "me",
        );
        if let Some(scene) = ctx.ballpark_scene_for_char(acid, sys_id) {
            emit_cycle_weapon_log_pair(ctx, acid, attacker_msg_key, dmg_dict, scene);
        }
    }
    if let Some(vcid) = target_char_id.filter(|&c| c > 0 && c != owner_cid) {
        let vic_dict = crate::system_actor::build_cycle_damage_message_dict(
            damage,
            attacker_ship_id,
            target_id,
            owner_cid,
            module_type_id,
            hit_quality,
            "other",
        );
        if let Some(scene) = ctx.ballpark_scene_for_char(vcid, sys_id) {
            emit_cycle_weapon_log_pair(ctx, vcid, victim_msg_key, vic_dict, scene);
        }
    }
}

fn build_lance_beam_damage_dict(
    applied_dmg: f64,
    attacker_ship_id: i64,
    victim_id: i64,
    attacker_char_id: i64,
    module_type_id: u32,
    attack_type: &str,
) -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
        (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
        (PyRep::String("attackerID".into()), PyRep::Integer(attacker_ship_id)),
        (PyRep::String("target".into()), PyRep::Integer(victim_id)),
        (PyRep::String("target_ID".into()), PyRep::Integer(victim_id)),
        (
            PyRep::String("owner".into()),
            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(attacker_char_id)]),
        ),
        (
            PyRep::String("weapon".into()),
            PyRep::Integer(module_type_id as i64),
        ),
        (PyRep::String("hitQuality".into()), PyRep::Integer(5)),
        (
            PyRep::String("attackType".into()),
            PyRep::String(attack_type.into()),
        ),
        (PyRep::String("isBanked".into()), PyRep::Bool(false)),
    ])
}

fn handle_lance_beam_fired_action(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: i64,
    attacker_ship_id: i64,
    attacker_ship_type_id: u32,
    module_type_id: u32,
    hits: Vec<(i64, f64)>,
) {
    let now_unix = ctx.now_unix_secs();
    let mut msg_tuples = Vec::new();
    for (victim_id, applied_dmg) in hits {
        crate::services::combat_crimewatch::record_killmail_assist_action(
            ctx,
            victim_id,
            attacker_char_id,
            attacker_ship_type_id,
            module_type_id,
            applied_dmg,
            false,
            now_unix,
        );
        crate::services::station_structure::structure_lifecycle_rpc::record_ship_damage_progression(
            ctx,
            sys_id,
            attacker_char_id,
            victim_id,
            applied_dmg,
        );
        let dmg_dict = build_lance_beam_damage_dict(
            applied_dmg,
            attacker_ship_id,
            victim_id,
            attacker_char_id,
            module_type_id,
            "me",
        );
        if let Some(attacker_scene) = ctx.ballpark_scene_for_char(attacker_char_id, sys_id) {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(attacker_char_id),
                crate::router::notify_registry::OnDamageMessage {
                    scene: attacker_scene,
                    damage_messages_args: dmg_dict.clone(),
                },
            );
            crate::services::notify_emitters_ess_bounties::emit_combat_message_in_scene(
                ctx,
                attacker_char_id,
                if applied_dmg > 0.0 {
                    "AttackHit"
                } else {
                    "AttackMiss"
                },
                victim_id,
                applied_dmg,
                module_type_id,
                false,
                attacker_scene,
            );
        }
        msg_tuples.push(PyRep::Tuple(vec![
            PyRep::String("AttackHit".into()),
            dmg_dict,
        ]));
        let victim_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == victim_id)
                .map(|c| c.character_id)
                .or_else(|| {
                    snap.items
                        .get(&victim_id)
                        .map(|i| i.owner_id)
                        .filter(|oid| snap.characters.contains_key(oid))
                })
        });
        if let Some(victim_char_id) = victim_char_opt {
            if let Some(victim_scene) = ctx.ballpark_scene_for_char(victim_char_id, sys_id) {
                crate::services::notify_emitters_ess_bounties::emit_combat_message_in_scene(
                    ctx,
                    victim_char_id,
                    if applied_dmg > 0.0 {
                        "AttackedBy"
                    } else {
                        "AttackMissBy"
                    },
                    attacker_ship_id,
                    applied_dmg,
                    module_type_id,
                    false,
                    victim_scene,
                );
                let vic_dict = build_lance_beam_damage_dict(
                    applied_dmg,
                    attacker_ship_id,
                    victim_id,
                    attacker_char_id,
                    module_type_id,
                    "other",
                );
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(victim_char_id),
                    crate::router::notify_registry::OnDamageMessage {
                        scene: victim_scene,
                        damage_messages_args: vic_dict.clone(),
                    },
                );
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(victim_char_id),
                    crate::router::notify_registry::OnDamageMessages {
                        scene: victim_scene,
                        dmgmsgs: vec![PyRep::Tuple(vec![
                            PyRep::String("AttackedBy".into()),
                            vic_dict,
                        ])],
                    },
                );
            }
        }
    }
    if !msg_tuples.is_empty() {
        if let Some(attacker_scene) = ctx.ballpark_scene_for_char(attacker_char_id, sys_id) {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(attacker_char_id),
                crate::router::notify_registry::OnDamageMessages {
                    scene: attacker_scene,
                    dmgmsgs: msg_tuples,
                },
            );
        }
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_cycle_weapon_damage_action(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    attacker_char_id: Option<i64>,
    attacker_ship_id: i64,
    attacker_ship_type_id: u32,
    target_id: i64,
    target_char_id: Option<i64>,
    module_type_id: u32,
    applied_dmg: f64,
    notified: bool,
) {
    if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
        crate::services::combat_crimewatch::record_killmail_assist_action(
            ctx,
            target_id,
            acid,
            attacker_ship_type_id,
            module_type_id,
            applied_dmg,
            false,
            ctx.now_unix_secs(),
        );
        crate::services::station_structure::structure_lifecycle_rpc::record_ship_damage_progression(
            ctx,
            sys_id,
            acid,
            target_id,
            applied_dmg,
        );
    }
    if !notified && applied_dmg > 0.0 {
        let owner_cid = attacker_char_id.unwrap_or(0);
        let (sr, ar, hr) = if let Ok(Some(t_cs)) = sys.get_combat_state(target_id).await {
            (
                (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
            if let Some(attacker_scene) = ctx.ballpark_scene_for_char(acid, sys_id) {
                crate::services::notify_emitters_combat_ess_progression::emit_damage_state_change_in_scene(
                    ctx,
                    acid,
                    target_id,
                    sr,
                    ar,
                    hr,
                    attacker_scene,
                );
            }
        }
        if let Some(vcid) = target_char_id.filter(|&c| c > 0 && c != owner_cid) {
            if let Some(victim_scene) = ctx.ballpark_scene_for_char(vcid, sys_id) {
                crate::services::notify_emitters_combat_ess_progression::emit_damage_state_change_in_scene(
                    ctx,
                    vcid,
                    target_id,
                    sr,
                    ar,
                    hr,
                    victim_scene,
                );
            }
        }
        emit_cycle_weapon_log_entries(
            ctx,
            sys_id,
            attacker_char_id,
            attacker_ship_id,
            target_id,
            target_char_id,
            module_type_id,
            Some(applied_dmg),
        );
    }
}

struct PreparedRepeatCycleEffects {
    mod_state: DogmaItemState,
    m_eff: FittedModule,
    empty_kw: HashMap<String, PyRep>,
}

/// Evaluates repeating-pulse Dogma state synchronously and drops `ShipFittingEngine` before any `.await`.
fn prepare_repeat_cycle_effects(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    is_overloaded: bool,
) -> Box<PreparedRepeatCycleEffects> {
    let fit_opt = ctx.fit(char_id, ship_id).ok();
    let mod_state = fit_opt
        .as_ref()
        .map(|f| {
            f.build_effective_module_state(module_id, module_type_id, is_overloaded, &ctx.sde)
        })
        .unwrap_or_else(|| DogmaItemState::from_sde(char_id, module_type_id, &ctx.sde));
    let m_eff = fit_opt
        .as_ref()
        .and_then(|f| f.modules.get(&module_id).cloned())
        .unwrap_or(FittedModule {
            item_id: module_id,
            type_id: module_type_id,
            flag_id: 27,
            state: if is_overloaded {
                ModuleState::Overloaded
            } else {
                ModuleState::Active
            },
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        });
    Box::new(PreparedRepeatCycleEffects {
        mod_state,
        m_eff,
        empty_kw: HashMap::new(),
    })
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn handle_repeat_cycle_start_action(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    target_id: Option<i64>,
    duration_s: f64,
    is_overloaded: bool,
    effect_name: String,
) {
    ctx.record_module_reactivation_delay(ship_id, module_id, duration_s);
    let Some(mod_type) = ctx.sde.types.get(&module_type_id) else {
        return;
    };
    let prepared = prepare_repeat_cycle_effects(
        ctx,
        char_id,
        ship_id,
        module_id,
        module_type_id,
        is_overloaded,
    );
    let pulse_failed = run_pre_notify_instant_module_effects(
        ctx,
        sys,
        sys_id,
        char_id,
        ship_id,
        module_id,
        &prepared.m_eff,
        mod_type,
        &prepared.mod_state,
        is_overloaded,
        target_id,
        duration_s,
        &prepared.empty_kw,
    )
    .await
    .is_err();
    drop(prepared);
    if pulse_failed {
        deactivate_repeating_cycle_after_action(
            ctx,
            sys,
            Some(char_id),
            ship_id,
            module_id,
            &effect_name,
        )
        .await;
    }
}

/// Applies overload state to the ship fitting synchronously so `ShipFittingEngine` is dropped before `.await`.
fn apply_sync_overload_state_fit(
    ctx: &ServerContext,
    char_id: Option<i64>,
    ship_id: i64,
    module_id: i64,
    is_overloaded: bool,
) -> Option<i64> {
    let cid = char_id
        .or_else(|| ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.owner_id)))?;
    let mut fit = ctx.fit(cid, ship_id).ok()?;
    if let Some(fm) = fit
        .modules
        .get_mut(&module_id)
        .filter(|m| m.state != ModuleState::Offline)
    {
        fm.state = if is_overloaded {
            ModuleState::Overloaded
        } else {
            ModuleState::Active
        };
    }
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.invalidate_and_push_fit(ship_id);
    Some(cid)
}

#[inline(never)]
async fn handle_sync_overload_state_action(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: Option<i64>,
    ship_id: i64,
    module_id: i64,
    is_overloaded: bool,
) {
    let Some(cid) = apply_sync_overload_state_fit(ctx, char_id, ship_id, module_id, is_overloaded)
    else {
        return;
    };
    crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(
        ctx, cid, sys_id, ship_id,
    )
    .await;
}

#[inline(never)]
async fn handle_invalidate_ship_fitting_action(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: Option<i64>,
    ship_id: i64,
) {
    ctx.invalidate_and_push_fit(ship_id);
    if let Some(cid) =
        char_id.or_else(|| ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.owner_id)))
    {
        let _ = ctx.fit(cid, ship_id);
        crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(
            ctx, cid, sys_id, ship_id,
        )
        .await;
    }
}

fn handle_auto_reload_started_action(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    module_ids: Vec<i64>,
    charge_type_id: u32,
    time_ms: f64,
    sim_deadline_s: f64,
    notified: bool,
) {
    let sim_now = ctx.systems.get_system_sim_elapsed(sys_id);
    ctx.systems
        .record_module_reload_sim_deadlines(&module_ids, sys_id, sim_now, sim_deadline_s);
    if !notified && char_id > 0 && !module_ids.is_empty() {
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnChargeBeingLoadedToModule {
                item_ids: module_ids,
                charge_type_id: i64::from(charge_type_id),
                time_ms,
            },
        );
    }
}

fn handle_no_charges_deactivated_action(
    ctx: &ServerContext,
    char_id: i64,
    module_type_id: u32,
    notified: bool,
) {
    if !notified && char_id > 0 {
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnRemoteMessage {
                msg_id: "NoCharges".into(),
                dict: Some(PyRep::Dict(vec![(
                    PyRep::String("type".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(4),
                        PyRep::Integer(i64::from(module_type_id)),
                    ]),
                )])),
                kw: None,
            },
        );
    }
}

fn handle_career_progress_event_action(
    ctx: &ServerContext,
    char_id: i64,
    source_ship_id: i64,
    target_id: i64,
    activity_kind: &'static str,
    sub_kind: &'static str,
    amount: f64,
) {
    match activity_kind {
        "ewar" => crate::services::quasar_career_goals::record_ewar_career_seconds(
            ctx, char_id, target_id, sub_kind, amount,
        ),
        "cap_warfare" => crate::services::quasar_career_goals::record_cap_warfare_career(
            ctx,
            char_id,
            source_ship_id,
            target_id,
            sub_kind,
            amount,
        ),
        "remote_repair" => {
            crate::services::quasar_career_goals::record_remote_repair_career(
                ctx, char_id, target_id, sub_kind, amount,
            );
        }
        _ => {}
    }
}

#[inline(never)]
pub(crate) async fn execute_pending_cycle_actions_list(
    ctx: &ServerContext,
    sys_id: u32,
    actions: Vec<PendingCycleAction>,
) {
    if actions.is_empty() {
        return;
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    for action in actions {
        match action {
            PendingCycleAction::SalvageEnd {
                char_id,
                ship_id,
                module_id,
                module_type_id,
                wreck_id,
                repeat,
                effect_name,
                next_cycle_cap_deducted,
                deducted_cap_gj,
            } => {
                handle_salvage_end_action(
                    ctx,
                    &sys,
                    sys_id,
                    char_id,
                    ship_id,
                    module_id,
                    module_type_id,
                    wreck_id,
                    repeat,
                    effect_name,
                    next_cycle_cap_deducted,
                    deducted_cap_gj,
                )
                .await;
            }
            PendingCycleAction::LanceBeamFired {
                attacker_char_id,
                attacker_ship_id,
                attacker_ship_type_id,
                module_id: _,
                module_type_id,
                hits,
            } => {
                handle_lance_beam_fired_action(
                    ctx,
                    sys_id,
                    attacker_char_id,
                    attacker_ship_id,
                    attacker_ship_type_id,
                    module_type_id,
                    hits,
                );
            }
            PendingCycleAction::CycleWeaponDamage {
                attacker_char_id,
                attacker_ship_id,
                attacker_ship_type_id,
                target_id,
                target_char_id,
                module_type_id,
                applied_dmg,
                notified,
            } => {
                handle_cycle_weapon_damage_action(
                    ctx,
                    &sys,
                    sys_id,
                    attacker_char_id,
                    attacker_ship_id,
                    attacker_ship_type_id,
                    target_id,
                    target_char_id,
                    module_type_id,
                    applied_dmg,
                    notified,
                )
                .await;
            }
            PendingCycleAction::CycleWeaponMiss {
                attacker_char_id,
                attacker_ship_id,
                target_id,
                target_char_id,
                module_type_id,
                notified,
            } => {
                if !notified {
                    emit_cycle_weapon_log_entries(
                        ctx,
                        sys_id,
                        attacker_char_id,
                        attacker_ship_id,
                        target_id,
                        target_char_id,
                        module_type_id,
                        None,
                    );
                }
            }
            PendingCycleAction::RepeatCycleStart {
                char_id,
                ship_id,
                module_id,
                module_type_id,
                target_id,
                duration_s,
                is_overloaded,
                effect_name,
            } => {
                handle_repeat_cycle_start_action(
                    ctx,
                    &sys,
                    sys_id,
                    char_id,
                    ship_id,
                    module_id,
                    module_type_id,
                    target_id,
                    duration_s,
                    is_overloaded,
                    effect_name,
                )
                .await;
            }
            PendingCycleAction::SyncOverloadState {
                char_id,
                ship_id,
                module_id,
                is_overloaded,
            } => {
                handle_sync_overload_state_action(
                    ctx,
                    sys_id,
                    char_id,
                    ship_id,
                    module_id,
                    is_overloaded,
                )
                .await;
            }
            PendingCycleAction::InvalidateShipFitting { char_id, ship_id } => {
                handle_invalidate_ship_fitting_action(ctx, sys_id, char_id, ship_id).await;
            }
            PendingCycleAction::CycleChargeItemMutations {
                char_id,
                ship_id,
                diffs,
            } => {
                emit_ship_charge_sublocation_diffs(ctx, char_id, ship_id, &diffs);
            }
            PendingCycleAction::AutoReloadStarted {
                char_id,
                module_ids,
                charge_type_id,
                time_ms,
                sim_deadline_s,
                notified,
                ..
            } => {
                handle_auto_reload_started_action(
                    ctx,
                    sys_id,
                    char_id,
                    module_ids,
                    charge_type_id,
                    time_ms,
                    sim_deadline_s,
                    notified,
                );
            }
            PendingCycleAction::NoChargesDeactivated {
                char_id,
                module_type_id,
                notified,
            } => {
                handle_no_charges_deactivated_action(ctx, char_id, module_type_id, notified);
            }
            PendingCycleAction::ShipCapacitorChanged {
                char_id,
                ship_id,
                new_cap,
                max_cap,
                old_cap,
                notified,
            } => {
                if !notified {
                    let _ = ctx.notify_ship_capacitor_change(
                        char_id,
                        ship_id,
                        new_cap,
                        max_cap,
                        Some(old_cap),
                    );
                }
            }
            PendingCycleAction::CareerProgressEvent {
                char_id,
                source_ship_id,
                target_id,
                activity_kind,
                sub_kind,
                amount,
            } => {
                handle_career_progress_event_action(
                    ctx,
                    char_id,
                    source_ship_id,
                    target_id,
                    activity_kind,
                    sub_kind,
                    amount,
                );
            }
        }
    }
}
