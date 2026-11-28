//! `structure_weapons_ops` — Upwell Standup weapon activation, Point Defense System,
//! Guided Bomb Launcher, Keepstar Standup Arcing Vorton Projector superweapon,
//! Standup Multirole/Anticapital Missiles, AoE Neut/ECM Burst Projectors, structure
//! target locking (`dogmaIM.AddTarget`), and automated grid defense retaliation.
#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::{
    structure_lifecycle_rpc::{apply_damage_structure_or_ball, ensure_structure_ball_in_space},
    structure_upwell_ops::{self as ops, load_ext_state, save_ext_state},
};
use crate::services::structure_acl_defense_rules::{
    evaluate_auto_defense_target, load_defense_settings, save_defense_settings,
};
use eve_db::{InventoryFlag, WorldStateSnapshot};
use eve_destiny::{DamageVector, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashSet;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

pub(crate) fn cannot_overload_structure_module_error() -> BridgeRpcError {
    err(
        "CannotOverloadStructureModule",
        "Standup structure modules cannot be overloaded",
    )
}

fn loaded_charge_sde_damage(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    st_owner_corp: i64,
    mod_item: &eve_db::InventoryItemRecord,
) -> Option<(i64, DamageVector)> {
    let mod_sde = ctx.sde.types.get(&mod_item.type_id)?;
    if !matches!(mod_sde.group_id, 1327 | 1328 | 1330 | 1562) {
        return None;
    }
    let dmg_mult = mod_sde.default_attributes.get(&64).copied().unwrap_or(1.0);
    let mut charge_candidates: Vec<(i64, u32)> = snap
        .items
        .values()
        .filter(|it| {
            let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(
                None,
                st_owner_corp,
                eve_db::ItemClassPolicy::ExactAssembledItem,
            );
            opts.reject_fitted = false;
            it.location_id == structure_id
                && it.flag == mod_item.flag
                && it.item_id != mod_item.item_id
                && it.quantity > 0
                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 8)
                && snap.validate_item_for_consumption(it, &opts).is_ok()
        })
        .map(|it| (it.item_id, it.type_id))
        .collect();
    charge_candidates.sort_by_key(|&(iid, _)| iid);
    let (charge_iid, charge_tid) = charge_candidates.first().copied()?;
    let csde = ctx.sde.types.get(&charge_tid)?;
    let em = csde.default_attributes.get(&114).copied().unwrap_or(0.0) * dmg_mult;
    let explosive = csde.default_attributes.get(&116).copied().unwrap_or(0.0) * dmg_mult;
    let kinetic = csde.default_attributes.get(&117).copied().unwrap_or(0.0) * dmg_mult;
    let thermal = csde.default_attributes.get(&118).copied().unwrap_or(0.0) * dmg_mult;
    let dmg = DamageVector {
        em,
        thermal,
        kinetic,
        explosive,
    };
    if dmg.total() <= 0.0 {
        return None;
    }
    Some((charge_iid, dmg))
}

pub fn structure_online_defense_volley_omni(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
) -> f64 {
    let owner_corp = snap
        .structures
        .get(&structure_id)
        .map(|s| s.owner_corp_id)
        .unwrap_or(0);
    let mut total = 0.0;
    for item in snap.items.values() {
        if item.location_id != structure_id || !(27..=34).contains(&(item.flag as u16)) {
            continue;
        }
        if !snap.module_online_states.get(&item.item_id).copied().unwrap_or(true) {
            continue;
        }
        if let Some((_, dmg)) = loaded_charge_sde_damage(ctx, snap, structure_id, owner_corp, item) {
            total += dmg.total();
        }
    }
    total
}

pub(crate) fn structure_cap_params(ctx: &ServerContext, st_type_id: u32) -> (f64, f64) {
    let st_sde = ctx.sde.types.get(&st_type_id);
    let max_cap = st_sde
        .and_then(|t| t.default_attributes.get(&482).copied())
        .filter(|&c| c > 0.0)
        .unwrap_or(600_000.0);
    let rech_s = (st_sde
        .and_then(|t| t.default_attributes.get(&55).copied())
        .unwrap_or(5_760_000.0)
        / 1_000.0)
        .max(1.0);
    (max_cap, rech_s)
}

pub(crate) async fn read_unified_structure_capacitor(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    structure_id: i64,
    st_type_id: u32,
    now_unix: u64,
) -> (f64, f64) {
    let (max_cap, rech_s) = structure_cap_params(ctx, st_type_id);
    let ext_entry = ctx
        .db
        .with_snapshot(|snap| load_ext_state(snap).structure_cap_state.get(&structure_id).copied());
    let cs_opt = sys_handle.get_combat_state(structure_id).await.ok().flatten();
    let cur_cap = match ext_entry {
        Some((prev_cap, prev_ts)) => {
            let dt = now_unix.saturating_sub(prev_ts) as f64;
            (prev_cap + (max_cap / rech_s) * dt).min(max_cap)
        }
        None => cs_opt
            .filter(|cs| cs.max_capacitor > 0.0)
            .map(|cs| cs.capacitor.min(max_cap))
            .unwrap_or(max_cap),
    };
    (cur_cap, max_cap)
}

fn is_structure_auto_defense_enabled(
    ctx: &ServerContext,
    ext: &ops::StructureExtendedState,
    structure_id: i64,
) -> bool {
    let eff_prof = crate::services::structure_profile_services::effective_profile_for_structure(
        ext,
        structure_id,
        Some(ctx.now_unix()),
    )
    .or_else(|| ext.access_profiles.get(&structure_id));
    if let Some(ds) = eff_prof.and_then(|p| p.defense_settings.as_ref()) {
        return ds.auto_defense_enabled;
    }
    if let Some(prof) = eff_prof.filter(|p| p.has_defense_config) {
        return prof.auto_defense_enabled;
    }
    true
}

pub async fn trigger_structure_auto_defense_on_grid(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    structure_id: i64,
    attacker_char_id: Option<i64>,
    now_unix: u64,
) {
    ops::sync_pending_profiles_in_ctx(ctx);
    let Some((st_pos, st_type_id, _st_owner_id, pilot_opt)) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(att_id) = attacker_char_id.filter(|&id| id > 0) {
            let mut def = load_defense_settings(snap, structure_id);
            def.record_attacker(att_id, now_unix);
            save_defense_settings(snap, &def);
        }
        let st = snap.structures.get(&structure_id)?;
        if st.solar_system_id != sys_id
            || !crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), st)
        {
            return None;
        }
        let ext = load_ext_state(snap);
        if !is_structure_auto_defense_enabled(ctx, &ext, structure_id) {
            return None;
        }
        let pilot_opt = ext.active_pilot_by_structure.get(&structure_id).copied();
        let ceo_id = snap
            .corporations
            .get(&st.owner_corp_id)
            .map(|c| c.ceo_id)
            .unwrap_or(0);
        Some((DVec3::from_array(st.position), st.type_id, ceo_id, pilot_opt))
    }) else {
        return;
    };

    ensure_structure_ball_in_space(ctx, sys_id, structure_id).await;
    let (seed_cap, _) = read_unified_structure_capacitor(ctx, sys_handle, structure_id, st_type_id, now_unix).await;

    let candidates: Vec<(i64, i64)> = {
        let view = sys_handle.view();
        view.balls
            .values()
            .filter_map(|b| {
                if b.id == structure_id || b.position.distance(st_pos) > 250_000.0 {
                    return None;
                }
                let cs = view.combat_state(b.id)?;
                if cs.blocks_targeted_effect() || (cs.shield_hp + cs.armor_hp + cs.structure_hp) <= 0.0 {
                    return None;
                }
                let cid = view.slim_meta(b.id).and_then(|m| m.char_id).or_else(|| {
                    ctx.db.with_snapshot(|s| s.characters.values().find(|c| c.active_ship_id == b.id).map(|c| c.character_id))
                })?;
                Some((b.id, cid))
            })
            .collect()
    };

    let (targets, per_target_dmg, updated_cap): (
        Vec<(i64, i64)>,
        std::collections::HashMap<i64, DamageVector>,
        Option<f64>,
    ) = ctx.db.with_mut_snapshot(|snap| {
        let mut matched: Vec<(i64, i64)> = candidates
            .into_iter()
            .filter(|&(_, cid)| evaluate_auto_defense_target(ctx, snap, structure_id, cid))
            .collect();
        matched.sort_by_key(|&(bid, _)| bid);
        let mut ext = load_ext_state(snap);
        ext.structure_auto_targets
            .insert(structure_id, matched.iter().map(|&(bid, _)| bid).collect());

        let mut per_target: std::collections::HashMap<i64, DamageVector> =
            std::collections::HashMap::new();
        let mut cap_out = None;
        let (st_type_id, st_owner_corp) = snap
            .structures
            .get(&structure_id)
            .map(|st| (st.type_id, st.owner_corp_id))
            .unwrap_or((35832, 0));
        if !matched.is_empty() {
            let (max_cap, rech_s) = structure_cap_params(ctx, st_type_id);
            let mut cur_cap = match ext.structure_cap_state.get(&structure_id).copied() {
                Some((prev_cap, prev_ts)) => {
                    let dt = now_unix.saturating_sub(prev_ts) as f64;
                    (prev_cap + (max_cap / rech_s) * dt).min(max_cap)
                }
                None => seed_cap.min(max_cap),
            };

            let mut mod_items: Vec<eve_db::InventoryItemRecord> = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == structure_id
                        && (27..=34).contains(&(it.flag as u16))
                        && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
                })
                .cloned()
                .collect();
            mod_items.sort_by_key(|m| m.item_id);

            for mod_item in mod_items {
                if ext
                    .standup_module_cycle_until_unix
                    .get(&mod_item.item_id)
                    .is_some_and(|&u| now_unix < u)
                {
                    continue;
                }
                let Some(mod_sde) = ctx.sde.types.get(&mod_item.type_id) else {
                    continue;
                };
                let cap_need = mod_sde.default_attributes.get(&6).copied().unwrap_or(0.0);
                if cur_cap + 1e-6 < cap_need {
                    continue;
                }
                let Some((charge_iid, dmg)) = loaded_charge_sde_damage(ctx, snap, structure_id, st_owner_corp, &mod_item) else {
                    continue;
                };
                let Some(c_owner) = snap.items.get(&charge_iid).map(|it| it.owner_id) else {
                    continue;
                };
                let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(
                    None,
                    c_owner,
                    eve_db::ItemClassPolicy::ExactAssembledItem,
                );
                opts.reject_fitted = false;
                if snap.consume_exact_item_quantity(charge_iid, 1, opts).is_err() {
                    continue;
                }
                cur_cap = (cur_cap - cap_need).max(0.0);
                ext.structure_cap_state.insert(structure_id, (cur_cap, now_unix));
                cap_out = Some(cur_cap);
                let cycle_secs = mod_sde
                    .default_attributes
                    .get(&51)
                    .or_else(|| mod_sde.default_attributes.get(&73))
                    .map(|&ms| (ms / 1_000.0).ceil() as u64)
                    .unwrap_or(10)
                    .max(1);
                ext.standup_module_cycle_until_unix
                    .insert(mod_item.item_id, now_unix.saturating_add(cycle_secs));
                let cursor = ext.structure_auto_shot_cursor.entry(structure_id).or_insert(0);
                let (target_ship_id, _) = matched[*cursor % matched.len()];
                *cursor = cursor.wrapping_add(1);
                let entry = per_target.entry(target_ship_id).or_default();
                entry.em += dmg.em;
                entry.thermal += dmg.thermal;
                entry.kinetic += dmg.kinetic;
                entry.explosive += dmg.explosive;
            }
        }
        save_ext_state(snap, &ext);
        (matched, per_target, cap_out)
    });
    if let Some(c) = updated_cap {
        let _ = sys_handle.set_capacitor(structure_id, c).await;
    }

    for (ship_id, target_char_id) in targets {
        let _ = sys_handle
            .add_target_lock(
                structure_id,
                TargetLock {
                    target_id: ship_id,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(300_000.0),
                },
            )
            .await;
        if let Some(ctrl_char) = pilot_opt {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(ctrl_char),
                crate::router::notify_registry::OnStructureTargetAdded {
                    sid: structure_id,
                    tid: ship_id,
                },
            );
        }
        if let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id) {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(target_char_id),
                crate::router::notify_registry::OnTarget {
                    scene,
                    arg0: PyRep::String("otheradd".into()),
                    arg1: PyRep::Integer(structure_id),
                    reason: None,
                },
            );
        }
        let target_dmg = per_target_dmg.get(&ship_id).copied().unwrap_or_default();
        if target_dmg.total() > 0.0 {
            if let Some(gunner_char) = pilot_opt {
                if crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                    ctx,
                    gunner_char,
                    structure_id,
                    sys_id,
                    ship_id,
                    false,
                )
                .is_err()
                {
                    continue;
                }
            }
            let pre_ball = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| (b.type_id, b.position));
            if let Ok(Some((dealt, destroyed, _))) = sys_handle
                .apply_damage_with_attacker(ship_id, target_dmg, pilot_opt.or(Some(structure_id)))
                .await
            {
                if dealt > 0.0 {
                    if let Some(gunner_char) = pilot_opt {
                        crate::services::station_structure::structure_lifecycle_rpc::record_ship_damage_progression(
                            ctx,
                            sys_id,
                            gunner_char,
                            ship_id,
                            dealt,
                        );
                    }
                    if let Some(scene) =
                        crate::router::notify_registry::BallparkScene::try_solar_system(sys_id)
                    {
                        ctx.emit_typed(
                            crate::router::NotificationTarget::Character(target_char_id),
                            crate::router::notify_registry::OnDamageMessage {
                                scene,
                                damage_messages_args: PyRep::Dict(vec![
                                    (PyRep::String("damage".into()), PyRep::Real(dealt)),
                                    (PyRep::String("source".into()), PyRep::Integer(structure_id)),
                                    (PyRep::String("attackerID".into()), PyRep::Integer(structure_id)),
                                    (PyRep::String("target".into()), PyRep::Integer(ship_id)),
                                    (PyRep::String("target_ID".into()), PyRep::Integer(ship_id)),
                                    (PyRep::String("weapon".into()), PyRep::Integer(35_894)),
                                    (PyRep::String("hitQuality".into()), PyRep::Integer(3)),
                                    (PyRep::String("attackType".into()), PyRep::String("other".into())),
                                    (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
                                    (PyRep::String("isBanked".into()), PyRep::Bool(false)),
                                ]),
                            },
                        );
                    }
                }
                if destroyed && !ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
                    let (b_tid, b_pos) = pre_ball.unwrap_or((0, DVec3::ZERO));
                    crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                        ctx,
                        sys_id,
                        pilot_opt.unwrap_or(structure_id),
                        ship_id,
                        b_tid,
                        b_pos,
                        now_unix,
                    )
                    .await;
                }
            }
        }
    }
}

pub async fn trigger_system_structures_auto_defense(ctx: &ServerContext, sys_id: u32) {
    let sids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        snap.structures
            .values()
            .filter(|st| {
                st.solar_system_id == sys_id
                    && crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), st)
                    && is_structure_auto_defense_enabled(ctx, &ext, st.structure_id)
                    && (ext
                        .access_profiles
                        .get(&st.structure_id)
                        .is_some_and(|p| p.has_defense_config || p.defense_settings.is_some())
                        || !load_defense_settings(snap, st.structure_id).known_attackers.is_empty()
                        || structure_online_defense_volley_omni(ctx, snap, st.structure_id) > 0.0)
            })
            .map(|st| st.structure_id)
            .collect()
    });
    if sids.is_empty() {
        return;
    }
    let Some(sys_handle) = ctx.systems.get_handle(sys_id) else {
        return;
    };
    let now_unix = ctx.now_unix();
    for sid in sids {
        trigger_structure_auto_defense_on_grid(ctx, &sys_handle, sys_id, sid, None, now_unix).await;
    }
}

pub async fn handle_structure_add_target(
    ctx: &ServerContext,
    _session: &SessionState,
    char_id: i64,
    structure_id: i64,
    target_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, st_type_id) = ctx.db.with_snapshot(|snap| {
        let st = snap
            .structures
            .get(&structure_id)
            .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if !crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), st) {
            return Err(err("StructureNotOperational", "Structure cannot lock targets in its current state"));
        }
        let ext = load_ext_state(snap);
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err(
                "NotControllingStructure",
                format!("Character {char_id} is not controlling structure {structure_id}"),
            ));
        }
        Ok((st.solar_system_id, st.type_id))
    })?;

    ensure_structure_ball_in_space(ctx, sys_id, structure_id).await;
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&target_id)) {
        ensure_structure_ball_in_space(ctx, sys_id, target_id).await;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, st_ball)) = sys_handle.get_ball_snapshot(structure_id).await.unwrap_or(None) else {
        return Err(BridgeRpcError::targeting_failed(
            Some(structure_id),
            Some(target_id),
            eve_proto::UserErrorKey::CantShipNotPresent,
            "structure_not_in_space",
            "Structure ball not in space",
        ));
    };
    let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.unwrap_or(None) else {
        return Err(BridgeRpcError::targeting_failed(
            Some(structure_id),
            Some(target_id),
            eve_proto::UserErrorKey::TargetCantValidate,
            "target_not_present",
            format!("Target {target_id} not found on grid"),
        ));
    };
    if sys_handle
        .get_combat_state(target_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|mut cs| {
            cs.sync_action_breaks();
            cs.blocks_targeted_effect()
        })
    {
        return Err(err("TargetInvulnerable", "Target is cloaked, tethered, or invulnerable"));
    }

    let st_sde = ctx.sde.types.get(&st_type_id);
    let max_range = st_sde
        .and_then(|t| t.default_attributes.get(&76).copied())
        .unwrap_or(380_000.0);
    let max_locks = st_sde
        .and_then(|t| t.default_attributes.get(&192).copied())
        .unwrap_or(7.0) as usize;
    let dist = ((st_ball.position - target_ball.position).length() - st_ball.radius - target_ball.radius).max(0.0);
    if dist > max_range {
        return Err(BridgeRpcError::targeting_too_far(
            Some(structure_id),
            Some(target_id),
            crate::services::inventory_fitting::resolve_target_display_name(ctx, target_id),
            st_type_id as i64,
            dist,
            max_range,
            format!("Target distance ({dist:.0} m) exceeds structure maxTargetRange ({max_range:.0} m)"),
        ));
    }
    let existing_locks = sys_handle.get_target_locks(structure_id).await.unwrap_or_default();
    if let Some(existing) = existing_locks.iter().find(|l| l.target_id == target_id) {
        let target_ids = if existing.locked {
            vec![PyRep::Integer(target_id)]
        } else {
            Vec::new()
        };
        return Ok(PyRep::Tuple(vec![
            PyRep::Integer(i64::from(!existing.locked)),
            PyRep::List(target_ids),
        ]));
    }
    if existing_locks.len() >= max_locks {
        return Err(err("MaxLockedTargets", "Structure maximum locked targets reached"));
    }
    let scan_res = st_sde
        .and_then(|t| t.default_attributes.get(&564).copied())
        .unwrap_or(40.0)
        .max(1.0);
    let raw_sig = sys_handle
        .get_combat_state(target_id)
        .await
        .ok()
        .flatten()
        .map(|cs| cs.base_signature_radius.unwrap_or(cs.signature_radius))
        .filter(|&s| s > 0.0)
        .unwrap_or(0.0)
        .max(target_ball.radius)
        .max(35.0);
    let target_sig = ctx.effective_target_signature_radius(target_id, raw_sig);
    let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig);
    let lock_time_ms = (lock_sec * 1_000.0).max(1.0);
    sys_handle
        .add_target_lock(
            structure_id,
            TargetLock {
                target_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
                max_range_m: Some(max_range),
            },
        )
        .await
        .map_err(|e| {
            let key = if e.contains("tethered") {
                eve_proto::UserErrorKey::DeniedTethered
            } else {
                eve_proto::UserErrorKey::UnableToTarget
            };
            BridgeRpcError::targeting_failed(Some(structure_id), Some(target_id), key, "lock_actor_rejected", e)
        })?;
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnStructureTargetAdded {
            sid: structure_id,
            tid: target_id,
        },
    );
    if let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id) {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnTarget {
                scene,
                arg0: PyRep::Integer(target_id),
                arg1: PyRep::String("locking".into()),
                reason: None,
            },
        );
        if let Some(t_char) = ctx.db.with_snapshot(|s| {
            s.characters
                .values()
                .find(|c| c.active_ship_id == target_id)
                .map(|c| c.character_id)
        }) {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(t_char),
                crate::router::notify_registry::OnTarget {
                    scene,
                    arg0: PyRep::Integer(structure_id),
                    arg1: PyRep::String("targetedBy".into()),
                    reason: None,
                },
            );
        }
    }
    Ok(PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())]))
}

fn consume_structure_charge_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    st_owner_corp: i64,
    module_id: i64,
    module_type_id: u32,
    slot_flag: InventoryFlag,
    req_charge_group: u32,
) -> Result<u32, BridgeRpcError> {
    let mut wrong_group_seen = false;
    let mut candidates: Vec<(i64, u32, bool)> = Vec::new();
    for it in snap.items.values() {
        if it.owner_id != st_owner_corp
            || it.quantity <= 0
            || it.item_id == module_id
            || snap.is_item_locked_in_contract_or_job(it.item_id)
        {
            continue;
        }
        let in_slot =
            it.location_id == module_id || (it.location_id == structure_id && it.flag == slot_flag);
        let in_ammo = it.location_id == structure_id
            && matches!(
                it.flag,
                InventoryFlag::SpecializedAmmoHold | InventoryFlag::Cargo
            );
        if !in_slot && !in_ammo {
            continue;
        }
        let Some(ct) = ctx.sde.types.get(&it.type_id) else {
            continue;
        };
        if ct.category_id != 8 {
            continue;
        }
        if ct.group_id == req_charge_group {
            let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(
                None,
                st_owner_corp,
                eve_db::ItemClassPolicy::ExactAssembledItem,
            );
            opts.reject_fitted = false;
            if snap.validate_item_for_consumption(it, &opts).is_ok() {
                candidates.push((it.item_id, it.type_id, in_slot));
            }
        } else if in_slot {
            wrong_group_seen = true;
        }
    }
    candidates.sort_by_key(|&(iid, _, in_slot)| (!in_slot, iid));
    let Some((charge_iid, charge_tid, _)) = candidates.first().copied() else {
        if wrong_group_seen {
            return Err(err(
                "InvalidChargeGroup",
                format!("Loaded charge does not match required charge group {req_charge_group}"),
            ));
        }
        return Err(BridgeRpcError::no_charges_loaded(
            module_type_id as i64,
            format!("Standup weapon requires ammo of charge group {req_charge_group}"),
        ));
    };
    let c_owner = snap
        .items
        .get(&charge_iid)
        .map(|it| it.owner_id)
        .ok_or_else(|| {
            BridgeRpcError::no_charges_loaded(
                module_type_id as i64,
                format!("Standup weapon requires ammo of charge group {req_charge_group}"),
            )
        })?;
    let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(
        None,
        c_owner,
        eve_db::ItemClassPolicy::ExactAssembledItem,
    );
    opts.reject_fitted = false;
    snap.consume_exact_item_quantity(charge_iid, 1, opts)
        .map_err(|e| BridgeRpcError::no_charges_loaded(module_type_id as i64, e.to_string()))?;
    Ok(charge_tid)
}

fn require_structure_locked_target(
    sys_handle: &crate::system_actor::SystemHandle,
    structure_id: i64,
    target_id_opt: Option<i64>,
) -> Result<i64, BridgeRpcError> {
    let target_id = target_id_opt
        .filter(|&id| id > 0)
        .ok_or_else(|| err("TargetRequired", "Standup targeted module requires a targetID"))?;
    let is_locked = sys_handle
        .view()
        .target_locks
        .get(&structure_id)
        .is_some_and(|locks| locks.iter().any(|l| l.target_id == target_id && l.locked));
    if !is_locked {
        return Err(err(
            "TargetNotLocked",
            format!("Structure {structure_id} does not have target {target_id} locked"),
        ));
    }
    Ok(target_id)
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_structure_module_activate(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    structure_id: i64,
    module_id: i64,
    _effect_name: &str,
    target_id_opt: Option<i64>,
    is_overload: bool,
) -> Result<PyRep, BridgeRpcError> {
    if is_overload {
        return Err(cannot_overload_structure_module_error());
    }
    let now_unix = ctx.now_unix();
    let (st, mod_type_id, mod_group_id, slot_flag, _owner_ally, _gunner_fleet_members) =
        ctx.db.with_snapshot(|snap| {
            let st = snap
                .structures
                .get(&structure_id)
                .cloned()
                .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
            if !crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), &st) {
                return Err(err(
                    "StructureNotOperational",
                    format!("Structure {structure_id} is in state '{}' and cannot activate modules", st.state),
                ));
            }
            let ext = load_ext_state(snap);
            if session.ship_id != Some(structure_id)
                || ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id)
            {
                return Err(err(
                    "NotControllingStructure",
                    format!("Character {char_id} must assume control of structure {structure_id} before activating Standup modules"),
                ));
            }
            let item = snap
                .items
                .get(&module_id)
                .filter(|i| i.location_id == structure_id && (11..=34).contains(&(i.flag as u16)))
                .ok_or_else(|| BridgeRpcError::module_not_found(module_id, Some(structure_id), None, false, format!("Module {module_id} is not fitted to structure {structure_id}")))?;
            if !snap.module_online_states.get(&module_id).copied().unwrap_or(true) {
                return Err(err("ModuleOffline", format!("Module {module_id} is offline")));
            }
            let mt = ctx
                .sde
                .types
                .get(&item.type_id)
                .ok_or_else(|| err("UnknownModuleType", "Unknown module type"))?;
            let owner_ally = snap
                .corporations
                .get(&st.owner_corp_id)
                .and_then(|c| c.alliance_id)
                .unwrap_or(0);
            let fleet_members: HashSet<i64> = session
                .fleet_id
                .and_then(|fid| snap.fleets.get(&fid))
                .map(|f| f.members.iter().copied().collect())
                .unwrap_or_default();
            Ok((st, item.type_id, mt.group_id, item.flag, owner_ally, fleet_members))
        })?;

    let sys_id = st.solar_system_id;
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let is_highsec = sys_sec >= 0.45 && sys_id < 31_000_000;

    if mod_group_id == 1333 && !matches!(st.type_id, 35834 | 40340) {
        return Err(err(
            "SuperweaponRequiresKeepstar",
            "Standup Arcing Vorton Projector can only be activated on a Keepstar",
        ));
    }
    if is_highsec && matches!(mod_group_id, 1328 | 1330 | 1331 | 1333) {
        return Err(err(
            "IllegalHighSecAoEWeapon",
            "Standup AoE weapons, Point Defense Batteries, Guided Bombs, Burst Projectors, and Superweapons cannot be activated in high-security space",
        ));
    }

    ensure_structure_ball_in_space(ctx, sys_id, structure_id).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let st_pos = DVec3::from_array(st.position);
    let st_radius = ctx
        .sde
        .types
        .get(&st.type_id)
        .map(|t| t.radius)
        .unwrap_or(10_000.0);
    let mod_sde = ctx.sde.types.get(&mod_type_id).unwrap();
    let cap_need = mod_sde.default_attributes.get(&6).copied().unwrap_or(0.0);
    let dmg_mult = mod_sde.default_attributes.get(&64).copied().unwrap_or(1.0);
    let req_charge_grp = mod_sde
        .default_attributes
        .get(&604)
        .copied()
        .map(|v| v as u32)
        .unwrap_or(0);

    // Pre-validate target & range BEFORE deducting capacitor or consuming ammo!
    let locked_target = if matches!(mod_group_id, 1327 | 1328 | 1331 | 1333 | 1562) {
        let tid = require_structure_locked_target(&sys_handle, structure_id, target_id_opt)?;
        let Some((_, tball)) = sys_handle.get_ball_snapshot(tid).await.unwrap_or(None) else {
            return Err(err("TargetNotFound", format!("Target {tid} is no longer on grid")));
        };
        if sys_handle
            .get_combat_state(tid)
            .await
            .ok()
            .flatten()
            .is_some_and(|mut cs| {
                cs.sync_action_breaks();
                cs.blocks_targeted_effect()
            })
        {
            return Err(err("TargetInvulnerable", "Cannot fire Standup weapons at a cloaked, tethered, or invulnerable target"));
        }
        let max_wep_range = match mod_group_id {
            1562 => 250_000.0,
            1327 => 350_000.0,
            1328 => 400_000.0,
            1331 | 1333 => mod_sde.default_attributes.get(&54).copied().unwrap_or(300_000.0),
            _ => 300_000.0,
        };
        let surf_dist = ((tball.position - st_pos).length() - st_radius - tball.radius).max(0.0);
        if surf_dist > max_wep_range {
            return Err(err(
                "OutOfRange",
                format!("Target {tid} is at {surf_dist:.0} m, exceeding weapon range {max_wep_range:.0} m"),
            ));
        }
        Some((tid, tball))
    } else {
        None
    };

    let (seed_cap, _) = read_unified_structure_capacitor(ctx, &sys_handle, structure_id, st.type_id, now_unix).await;

    // Atomically check structure state, module fit/online, capacitor, per-module cycle cooldown, and consume 1 charge if required.
    let (charge_tid_opt, new_cap_val) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(live_st) = snap.structures.get(&structure_id) else {
            return Err(err("StructureNotFound", format!("Structure {structure_id} not found")));
        };
        if live_st.state == "Destroyed" {
            return Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed")));
        }
        let Some(live_mod) = snap.items.get(&module_id) else {
            return Err(BridgeRpcError::module_not_found(module_id, Some(structure_id), None, false, format!("Module {module_id} no longer exists")));
        };
        if live_mod.location_id != structure_id || snap.module_online_states.get(&module_id) != Some(&true) {
            return Err(err("ModuleOffline", format!("Module {module_id} is no longer fitted and online on structure {structure_id}")));
        }
        let mut ext = load_ext_state(snap);
        if ext
            .standup_module_cycle_until_unix
            .get(&module_id)
            .is_some_and(|&u| now_unix < u)
        {
            return Err(err(
                "ModuleAlreadyCycling",
                format!("Module {module_id} is already cycling"),
            ));
        }
        let (max_cap, rech_s) = structure_cap_params(ctx, st.type_id);
        let cur_cap = match ext.structure_cap_state.get(&structure_id).copied() {
            Some((prev_cap, prev_ts)) => {
                let dt = now_unix.saturating_sub(prev_ts) as f64;
                (prev_cap + (max_cap / rech_s) * dt).min(max_cap)
            }
            None => seed_cap.min(max_cap),
        };
        if cur_cap + 1e-6 < cap_need {
            return Err(BridgeRpcError::not_enough_capacitor(
                Some(structure_id),
                &mod_sde.name,
                cap_need,
                cur_cap,
                format!("Structure capacitor {cur_cap:.0} GJ is below required {cap_need:.0} GJ"),
            ));
        }
        let ctid = if req_charge_grp > 0 {
            Some(consume_structure_charge_in_snap(
                ctx,
                snap,
                structure_id,
                st.owner_corp_id,
                module_id,
                live_mod.type_id,
                slot_flag,
                req_charge_grp,
            )?)
        } else {
            None
        };
        let next_cap = (cur_cap - cap_need).max(0.0);
        ext.structure_cap_state
            .insert(structure_id, (next_cap, now_unix));
        let cycle_secs = mod_sde
            .default_attributes
            .get(&51)
            .or_else(|| mod_sde.default_attributes.get(&73))
            .map(|&ms| (ms / 1_000.0).ceil() as u64)
            .unwrap_or(10)
            .max(1);
        ext.standup_module_cycle_until_unix
            .insert(module_id, now_unix.saturating_add(cycle_secs));
        save_ext_state(snap, &ext);
        Ok((ctid, next_cap))
    })?;
    let _ = sys_handle.set_capacitor(structure_id, new_cap_val).await;

    ctx.notify_godma_ship_effect(
        crate::router::NotificationTarget::Character(char_id),
        module_id,
        10,
        true,
        target_id_opt.unwrap_or(0),
        1,
    );

    match mod_group_id {
        // Standup Energy Neutralization / ECM Jammer Burst Projectors (`feat:structures.structure-weapons`)
        1331 => {
            let (primary_id, primary_ball) = locked_target.unwrap();
            let aoe = mod_sde.default_attributes.get(&2262).copied().unwrap_or(20_000.0);
            let neut_gj = mod_sde.default_attributes.get(&97).copied().unwrap_or(0.0);
            let jam_str = mod_sde.default_attributes.get(&238).copied().unwrap_or(0.0);
            let optimal_range = mod_sde.default_attributes.get(&54).copied().filter(|&r| r > 0.0).unwrap_or(aoe);
            let falloff_range = mod_sde.default_attributes.get(&158).copied().unwrap_or(0.0);
            let jam_dur_s = (mod_sde.default_attributes.get(&73).copied().unwrap_or(20_000.0) / 1_000.0).max(1.0);
            let mut affected = Vec::new();
            let mut total_drained = 0.0;
            let mut jammed_count = 0usize;
            let candidates: Vec<(i64, u32, f64)> = {
                let view = sys_handle.view();
                view.balls
                    .values()
                    .filter(|b| b.id != structure_id && b.position.distance(primary_ball.position) <= aoe)
                    .filter(|b| {
                        view.combat_state(b.id)
                            .is_some_and(|cs| !cs.blocks_targeted_effect())
                    })
                    .map(|b| (b.id, b.type_id, b.position.distance(primary_ball.position)))
                    .collect()
            };
            for (bid, b_type_id, dist_m) in candidates {
                if ctx.db.with_snapshot(|s| s.structures.contains_key(&bid)) {
                    continue;
                }
                if crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                    ctx,
                    char_id,
                    structure_id,
                    sys_id,
                    bid,
                    b_type_id == 670,
                )
                .is_err()
                {
                    continue;
                }
                affected.push(bid);
                if neut_gj > 0.0 {
                    let drained_bits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let drained_ref = std::sync::Arc::clone(&drained_bits);
                    if sys_handle
                        .modify_combat_state(bid, move |cs| {
                            let d = cs.capacitor.min(neut_gj);
                            cs.capacitor = (cs.capacitor - d).max(0.0);
                            drained_ref.store(d.to_bits(), std::sync::atomic::Ordering::Relaxed);
                        })
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        total_drained +=
                            f64::from_bits(drained_bits.load(std::sync::atomic::Ordering::Relaxed));
                    }
                }
                if jam_str > 0.0 {
                    let mut best_sensor_idx = 0usize;
                    let mut target_sensor_strength = 0.0;
                    if let Some(t_type) = ctx.sde.types.get(&b_type_id) {
                        for (idx, s_attr) in [208u16, 209, 210, 211].into_iter().enumerate() {
                            let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                            if val > target_sensor_strength {
                                target_sensor_strength = val;
                                best_sensor_idx = idx;
                            }
                        }
                    }
                    if target_sensor_strength <= 0.0 {
                        target_sensor_strength = 12.0;
                    }
                    let jam_attr_id = 238u16 + (best_sensor_idx as u16);
                    let eff_jam_str = mod_sde
                        .default_attributes
                        .get(&jam_attr_id)
                        .copied()
                        .filter(|&v| v > 0.0)
                        .unwrap_or(jam_str);
                    let jam_chance = eve_dogma::compute_ecm_jam_chance(
                        eff_jam_str,
                        target_sensor_strength,
                        dist_m,
                        optimal_range,
                        falloff_range,
                    );
                    let roll = super::inventory_fitting::csprng_unit_f64(ctx);
                    if roll < jam_chance && jam_chance > 0.0 {
                        if sys_handle.apply_ecm_jam(bid, jam_dur_s).await.unwrap_or(false) {
                            jammed_count += 1;
                        }
                    }
                }
            }
            Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(primary_id)),
                ("aoeRadius", PyRep::Real(aoe)),
                ("targetsHit", PyRep::Integer(affected.len() as i64)),
                ("capacitorDrained", PyRep::Real(total_drained)),
                ("ecmJammed", PyRep::Bool(jammed_count > 0)),
            ]))
        }

        // Standup Multirole & Anticapital Missile Launchers (`feat:structures.structure-weapons`)
        1327 | 1562 => {
            let (primary_id, primary_ball) = locked_target.unwrap();
            crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                ctx,
                char_id,
                structure_id,
                sys_id,
                primary_id,
                primary_ball.type_id == 670,
            )?;
            let ctid = charge_tid_opt.unwrap_or(37847);
            let csde = ctx.sde.types.get(&ctid);
            let exp_rad = csde
                .and_then(|t| t.default_attributes.get(&654).copied())
                .unwrap_or(if mod_group_id == 1327 { 15_000.0 } else { 200.0 })
                .max(1.0);
            let sig = sys_handle
                .get_combat_state(primary_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.signature_radius)
                .filter(|&s| s > 1.0)
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&primary_ball.type_id)
                        .and_then(|t| t.default_attributes.get(&552).copied())
                })
                .unwrap_or(100.0);
            let factor = (sig / exp_rad).clamp(0.04, 1.0);
            let em = csde.and_then(|t| t.default_attributes.get(&114).copied()).unwrap_or(300.0) * dmg_mult * factor;
            let th = csde.and_then(|t| t.default_attributes.get(&118).copied()).unwrap_or(300.0) * dmg_mult * factor;
            let ki = csde.and_then(|t| t.default_attributes.get(&117).copied()).unwrap_or(300.0) * dmg_mult * factor;
            let ex = csde.and_then(|t| t.default_attributes.get(&116).copied()).unwrap_or(300.0) * dmg_mult * factor;
            let outcome = apply_damage_structure_or_ball(
                ctx,
                &sys_handle,
                sys_id,
                char_id,
                primary_id,
                DamageVector {
                    em,
                    thermal: th,
                    kinetic: ki,
                    explosive: ex,
                },
                10.0,
                now_unix,
            )
            .await?;
            let (dealt, destroyed) = outcome.map(|(d, dest, _)| (d, dest)).unwrap_or((0.0, false));
            if destroyed && !ctx.db.with_snapshot(|s| s.structures.contains_key(&primary_id)) {
                crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    char_id,
                    primary_id,
                    primary_ball.type_id,
                    primary_ball.position,
                    now_unix,
                )
                .await;
            }
            Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(primary_id)),
                ("chargeTypeID", PyRep::Integer(i64::from(ctid))),
                ("damageDealt", PyRep::Real(dealt)),
            ]))
        }

        _ => Err(err(
            "UnsupportedStructureModule",
            format!("Module group {mod_group_id} cannot be activated on structure {structure_id}"),
        )),
    }
}

#[cfg(test)] #[path = "structure_weapons_ops_tests.rs"]
mod structure_weapons_ops_tests;

#[cfg(test)]
mod r3_weapon_charge_policy_tests {
    use super::*;

    #[test]
    fn standup_charges_skip_lower_id_bpc_and_reject_bpc_only_slots() {
        let mut ctx = ServerContext::new();
        let mod_tid = 999_801_u32;
        let charge_tid = 999_802_u32;
        {
            let reg = std::sync::Arc::make_mut(&mut ctx.sde);
            let mut mod_rec = reg.types.values().next().unwrap().clone();
            mod_rec.type_id = mod_tid;
            mod_rec.group_id = 1327;
            mod_rec.category_id = 66;
            mod_rec.default_attributes.clear();
            mod_rec.default_attributes.insert(604, 83.0);
            mod_rec.default_attributes.insert(64, 1.0);
            reg.types.insert(mod_tid, mod_rec);

            let mut ch_rec = reg.types.values().next().unwrap().clone();
            ch_rec.type_id = charge_tid;
            ch_rec.group_id = 83;
            ch_rec.category_id = 8;
            ch_rec.default_attributes.clear();
            ch_rec.default_attributes.insert(114, 250.0);
            reg.types.insert(charge_tid, ch_rec);
        }

        let sid = 10_000_997_100_i64;
        let corp_id = 98_752_i64;
        let mod_id = 980_001_i64;
        let bpc_charge_id = 980_002_i64;
        let good_charge_id = 980_003_i64;
        let mod_rec = eve_db::InventoryItemRecord {
            item_id: mod_id,
            type_id: mod_tid,
            owner_id: corp_id,
            location_id: sid,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(mod_id, mod_rec.clone());
            snap.items.insert(
                bpc_charge_id,
                eve_db::InventoryItemRecord {
                    item_id: bpc_charge_id,
                    type_id: charge_tid,
                    owner_id: corp_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 5,
                    singleton: false,
                    is_blueprint_copy: true,
                    ..Default::default()
                },
            );

            // BPC-only slot must not return damage or succeed consumption.
            assert!(loaded_charge_sde_damage(&ctx, snap, sid, corp_id, &mod_rec).is_none());
            assert!(consume_structure_charge_in_snap(
                &ctx,
                snap,
                sid,
                corp_id,
                mod_id,
                mod_tid,
                InventoryFlag::HiSlot0,
                83,
            )
            .is_err());
            assert_eq!(snap.items[&bpc_charge_id].quantity, 5);

            // With higher-ID valid sibling, skips BPC and consumes valid charge.
            snap.items.insert(
                good_charge_id,
                eve_db::InventoryItemRecord {
                    item_id: good_charge_id,
                    type_id: charge_tid,
                    owner_id: corp_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 3,
                    singleton: false,
                    ..Default::default()
                },
            );
            let (chosen_iid, dmg) =
                loaded_charge_sde_damage(&ctx, snap, sid, corp_id, &mod_rec).expect("valid charge");
            assert_eq!(chosen_iid, good_charge_id);
            assert!((dmg.em - 250.0).abs() < 1e-6);

            let consumed_tid = consume_structure_charge_in_snap(
                &ctx,
                snap,
                sid,
                corp_id,
                mod_id,
                mod_tid,
                InventoryFlag::HiSlot0,
                83,
            )
            .expect("consumes valid charge");
            assert_eq!(consumed_tid, charge_tid);
            assert_eq!(snap.items[&bpc_charge_id].quantity, 5);
            assert_eq!(snap.items[&good_charge_id].quantity, 2);
        });
    }
}

