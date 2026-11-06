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
    }
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
            it.location_id == structure_id
                && it.flag == mod_item.flag
                && it.owner_id == st_owner_corp
                && it.item_id != mod_item.item_id
                && it.quantity > 0
                && !snap.is_item_locked_in_contract_or_job(it.item_id)
                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 8)
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

    let candidates: Vec<(i64, i64)> = if let Ok(bp) = sys_handle.ballpark.read() {
        bp.balls
            .values()
            .filter_map(|b| {
                if b.id == structure_id || b.position.distance(st_pos) > 250_000.0 {
                    return None;
                }
                let cs = bp.combat_states.get(&b.id)?;
                if cs.is_cloaked || cs.is_tethered || (cs.shield_hp + cs.armor_hp + cs.structure_hp) <= 0.0 {
                    return None;
                }
                let cid = bp.ball_slim_meta.get(&b.id).and_then(|m| m.char_id).or_else(|| {
                    ctx.db.with_snapshot(|s| s.characters.values().find(|c| c.active_ship_id == b.id).map(|c| c.character_id))
                })?;
                Some((b.id, cid))
            })
            .collect()
    } else {
        Vec::new()
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
                if let Some(ch_it) = snap.items.get_mut(&charge_iid) {
                    if ch_it.quantity > 1 {
                        ch_it.quantity -= 1;
                        ch_it.version += 1;
                    } else {
                        snap.items.remove(&charge_iid);
                    }
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
            ctx.notify(
                crate::router::NotificationTarget::Character(ctrl_char),
                "OnStructureTargetAdded",
                "charid",
                vec![PyRep::Integer(structure_id), PyRep::Integer(ship_id)],
            );
        }
        ctx.notify(
            crate::router::NotificationTarget::Character(target_char_id),
            "OnTarget",
            "charid",
            vec![PyRep::String("otheradd".into()), PyRep::Integer(structure_id)],
        );
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
                    ctx.notify(
                        crate::router::NotificationTarget::Character(target_char_id),
                        "OnDamageMessage",
                        "charid",
                        vec![
                            PyRep::String("hit".into()),
                            PyRep::Dict(vec![
                                (PyRep::String("attackerID".into()), PyRep::Integer(structure_id)),
                                (PyRep::String("damage".into()), PyRep::Real(dealt)),
                            ]),
                        ],
                    );
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
                    && (ext.access_profiles.get(&st.structure_id).is_some_and(|p| p.has_defense_config)
                        || !load_defense_settings(snap, st.structure_id).known_attackers.is_empty()
                        || structure_online_defense_volley_omni(ctx, snap, st.structure_id) > 0.0)
            })
            .map(|st| st.structure_id)
            .collect()
    });
    if sids.is_empty() {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
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
        return Err(err("TargetingFailed", "Structure ball not in space"));
    };
    let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.unwrap_or(None) else {
        return Err(err("TargetingFailed", format!("Target {target_id} not found on grid")));
    };
    if sys_handle
        .get_combat_state(target_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_tethered || (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0))
    {
        return Err(err("TargetInvulnerable", "Target is tethered or cloaked"));
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
        return Err(err(
            "TargetingFailed",
            format!("Target distance ({dist:.0} m) exceeds structure maxTargetRange ({max_range:.0} m)"),
        ));
    }
    let cur_locks = if let Ok(bp) = sys_handle.ballpark.read() {
        bp.target_locks.get(&structure_id).map(|v| v.len()).unwrap_or(0)
    } else {
        0
    };
    if cur_locks >= max_locks {
        return Err(err("MaxLockedTargets", "Structure maximum locked targets reached"));
    }
    let _ = sys_handle
        .add_target_lock(
            structure_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(max_range),
            },
        )
        .await;
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnStructureTargetAdded",
        "charid",
        vec![PyRep::Integer(structure_id), PyRep::Integer(target_id)],
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnTarget",
        "charid",
        vec![PyRep::String("add".into()), PyRep::Integer(target_id)],
    );
    if let Some(t_char) = ctx.db.with_snapshot(|s| {
        s.characters
            .values()
            .find(|c| c.active_ship_id == target_id)
            .map(|c| c.character_id)
    }) {
        ctx.notify(
            crate::router::NotificationTarget::Character(t_char),
            "OnTarget",
            "charid",
            vec![PyRep::String("otheradd".into()), PyRep::Integer(structure_id)],
        );
    }
    Ok(PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(target_id)])]))
}

fn consume_structure_charge_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    st_owner_corp: i64,
    module_id: i64,
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
            candidates.push((it.item_id, it.type_id, in_slot));
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
        return Err(err(
            "NoChargesLoaded",
            format!("Standup weapon requires ammo of charge group {req_charge_group}"),
        ));
    };
    if let Some(it) = snap.items.get_mut(&charge_iid) {
        if it.quantity > 1 {
            it.quantity -= 1;
            it.version = it.version.saturating_add(1);
        } else {
            snap.items.remove(&charge_iid);
        }
    }
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
        .ballpark
        .read()
        .ok()
        .and_then(|bp| {
            bp.target_locks
                .get(&structure_id)
                .map(|locks| locks.iter().any(|l| l.target_id == target_id && l.locked))
        })
        .unwrap_or(false);
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
        return Err(err(
            "CannotOverloadStructureModule",
            "Standup structure modules cannot be overloaded",
        ));
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
                .ok_or_else(|| err("ModuleNotFound", format!("Module {module_id} is not fitted to structure {structure_id}")))?;
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
            .is_some_and(|cs| cs.is_tethered)
        {
            return Err(err("TargetInvulnerable", "Cannot fire Standup weapons at a tethered target"));
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

    // Atomically check capacitor, per-module cycle cooldown, and consume 1 charge if required.
    let (charge_tid_opt, new_cap_val) = ctx.db.try_with_mut_snapshot(|snap| {
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
            return Err(err(
                "NotEnoughCapacitor",
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
            if let Ok(mut bp) = sys_handle.ballpark.write() {
                let candidates: Vec<(i64, u32, f64)> = bp
                    .balls
                    .values()
                    .filter(|b| b.id != structure_id && b.position.distance(primary_ball.position) <= aoe)
                    .map(|b| (b.id, b.type_id, b.position.distance(primary_ball.position)))
                    .collect();
                for (bid, b_type_id, dist_m) in candidates {
                    let is_valid = bp
                        .combat_states
                        .get(&bid)
                        .is_some_and(|cs| !cs.is_cloaked && !cs.is_tethered);
                    if !is_valid || ctx.db.with_snapshot(|s| s.structures.contains_key(&bid)) {
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
                        if let Some(cs) = bp.combat_states.get_mut(&bid) {
                            let d = cs.capacitor.min(neut_gj);
                            cs.capacitor = (cs.capacitor - d).max(0.0);
                            total_drained += d;
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
                            if let Some(cs) = bp.combat_states.get_mut(&bid) {
                                cs.ecm_jammed_remaining_s = jam_dur_s;
                            }
                            bp.target_locks.remove(&bid);
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

