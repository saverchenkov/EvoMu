//! `notify_emitters_batch9` — Server-side V24 notification emitters & handlers for Batch 9 (Part 1:
//! Skills/Injectors, Login/Milestones/Redeeming/Crates/Character, Diplomacy/Fittings/Agents/Offers/NES).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cosmetics_ui_ops::{load_cosmetics_ui_state, save_cosmetics_ui_state};
use crate::services::notify_emitters_batch3::{
    emit_alpha_injection_available, emit_community_fittings_updated,
    emit_datacore_bought, emit_milestone_timer_started,
};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, SavedFittingRecord,
    SkillHistoryEntry, TrainedSkillRecord, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Clears the Alpha injector ready notification latch when an Alpha injector is consumed.
pub fn clear_alpha_avail_notified(snap: &mut WorldStateSnapshot, char_id: i64) {
    let mut map: HashMap<i64, bool> = snap
        .service_state_blobs
        .get("alpha_avail_notified")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if map.remove(&char_id).is_some() {
        if let Ok(json) = serde_json::to_string(&map) {
            snap.service_state_blobs
                .insert("alpha_avail_notified".into(), json);
        }
    }
}

/// Emits `OnAlphaInjectionAvailable()` (`0` args, `id_type = "charid"`) once per ready window for Alpha clones.
// parity: notify:OnAlphaInjectionAvailable
pub fn maybe_emit_alpha_injection_available(
    ctx: &ServerContext,
    char_id: i64,
    is_alpha: bool,
    next_available: i64,
) {
    if !is_alpha || next_available != 0 {
        return;
    }
    let should_emit = ctx.db.with_mut_snapshot(|snap| {
        let mut map: HashMap<i64, bool> = snap
            .service_state_blobs
            .get("alpha_avail_notified")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if map.get(&char_id).copied().unwrap_or(false) {
            return false;
        }
        map.insert(char_id, true);
        if let Ok(json) = serde_json::to_string(&map) {
            snap.service_state_blobs
                .insert("alpha_avail_notified".into(), json);
        }
        true
    });
    if should_emit {
        emit_alpha_injection_available(ctx, char_id);
    }
}

/// Emits `OnNonDiminishingInjectionsAdded(amount)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnNonDiminishingInjectionsAdded
pub fn emit_non_diminishing_injections_added(ctx: &ServerContext, char_id: i64, added_qty: i64) {
    if added_qty <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNonDiminishingInjectionsAdded",
        "charid",
        vec![PyRep::Integer(added_qty)],
    );
}

/// Emits `OnNonDiminishingInjectionsRemoved()` (`0` args, `id_type = "charid"`).
// parity: notify:OnNonDiminishingInjectionsRemoved
pub fn emit_non_diminishing_injections_removed(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNonDiminishingInjectionsRemoved",
        "charid",
        vec![],
    );
}

/// `nonDiminishingInjectionMgr.RevokeNonDiminishingInjections(targetCharID)`: GM/Admin revocation
/// of consumable non-diminishing skill injectors, emitting `OnNonDiminishingInjectionsRemoved`.
// parity: notify:OnNonDiminishingInjectionsRemoved
pub fn handle_revoke_non_diminishing_injections(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_id) = ServerContext::require_character(session)?;
    if !session.has_gm_role() && (session.role & eve_db::service_roles::ROLE_ADMIN) == 0 {
        return Err(err(
            "InsufficientRole",
            "RevokeNonDiminishingInjections requires GM or Admin role",
        ));
    }
    let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_id);
    let (removed_qty, remaining_qty) = ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&target_char_id) {
            return Err(err(
                "CharacterNotFound",
                format!("Character {target_char_id} not found"),
            ));
        }
        let matching_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|it| {
                it.owner_id == target_char_id
                    && it.quantity > 0
                    && super::implant_plugging::is_non_diminishing_injector(&ctx.sde, it.type_id)
            })
            .map(|it| it.item_id)
            .collect();
        let mut removed = 0_i64;
        for iid in matching_ids {
            if let Some(it) = snap.items.remove(&iid) {
                removed = removed.saturating_add(i64::from(it.quantity));
            }
        }
        let rem: i64 = snap
            .items
            .values()
            .filter(|it| {
                it.owner_id == target_char_id
                    && it.quantity > 0
                    && super::implant_plugging::is_non_diminishing_injector(&ctx.sde, it.type_id)
            })
            .map(|it| i64::from(it.quantity))
            .sum();
        Ok((removed, rem))
    })?;
    if removed_qty > 0 && remaining_qty == 0 {
        emit_non_diminishing_injections_removed(ctx, target_char_id);
    }
    Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(target_char_id)),
        ("removedQuantity", PyRep::Integer(removed_qty)),
        ("remainingQuantity", PyRep::Integer(remaining_qty)),
    ]))
}

/// Emits `OnSkillBundleInjected(numberOfSkills, sp)` (`2` args, `id_type = "charid"`).
// parity: notify:OnSkillBundleInjected
pub fn emit_skill_bundle_injected(ctx: &ServerContext, char_id: i64, num_skills: i64, sp: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillBundleInjected",
        "charid",
        vec![PyRep::Integer(num_skills), PyRep::Integer(sp)],
    );
}

/// `skillMgr2.InjectSkillBundle(bundleID, skillTypeIDs, spAmount)`: injects a validated SDE skill
/// bundle into the caller's trained skills, grants bundle SP, and emits `OnSkillBundleInjected`.
// parity: notify:OnSkillBundleInjected
pub fn handle_inject_skill_bundle(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let bundle_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if bundle_id <= 0 {
        return Err(err(
            "InvalidSkillBundle",
            "InjectSkillBundle requires a positive bundleID",
        ));
    }
    let skill_ids: Vec<TypeId> = match args.get(1) {
        Some(PyRep::List(items) | PyRep::Tuple(items)) => items
            .iter()
            .filter_map(|x| x.as_i64().and_then(|v| u32::try_from(v).ok()))
            .collect(),
        _ => Vec::new(),
    };
    if skill_ids.is_empty() {
        return Err(err(
            "InvalidSkillBundle",
            "InjectSkillBundle requires a non-empty list of SDE skill typeIDs",
        ));
    }
    for &tid in &skill_ids {
        let valid_skill = ctx
            .sde
            .types
            .get(&tid)
            .is_some_and(|t| t.category_id == 16 && t.published);
        if !valid_skill {
            return Err(err(
                "InvalidSkillType",
                format!("Type {tid} is not a published SDE skill"),
            ));
        }
    }
    let sp_grant = args
        .get(2)
        .and_then(|a| a.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(25_000.0);
    let now_ft = ctx.now_blueos_filetime_ticks();

    let (injected_count, new_free_sp, ship_id) = ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            return Err(err(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            ));
        }
        let mut injected = 0_i64;
        let skills_map = snap.skills.entry(char_id).or_default();
        let hist = snap.skill_history.entry(char_id).or_default();
        for &tid in &skill_ids {
            if !skills_map.contains_key(&tid) {
                let rank = ctx
                    .sde
                    .types
                    .get(&tid)
                    .and_then(|t| t.default_attributes.get(&attr_ids::SKILL_TIME_CONSTANT).copied())
                    .unwrap_or(1.0);
                let sp_lvl1 = crate::formulas::sp_for_skill_level(rank, 1);
                skills_map.insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: 1,
                        skill_points: sp_lvl1,
                    },
                );
                hist.push(SkillHistoryEntry {
                    log_date_ft: now_ft,
                    event_type_id: 36,
                    skill_type_id: tid,
                    skill_level: 1,
                    absolute_points: sp_lvl1,
                });
                injected += 1;
            }
        }
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.free_skill_points += sp_grant;
        Ok((injected, ch.free_skill_points, ch.active_ship_id))
    })?;

    if ship_id > 0 {
        ctx.invalidate_ship_fitting(ship_id);
    }
    emit_skill_bundle_injected(ctx, char_id, injected_count, sp_grant.round() as i64);
    emit_server_brain_updated(ctx, char_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFreeSkillPointsChanged",
        "charid",
        vec![PyRep::Real(new_free_sp)],
    );
    Ok(PyRep::keyval([
        ("bundleID", PyRep::Integer(bundle_id)),
        ("injectedSkills", PyRep::Integer(injected_count)),
        ("spGranted", PyRep::Real(sp_grant)),
        ("freeSkillPoints", PyRep::Real(new_free_sp)),
    ]))
}

/// Emits `OnSkillForcedRefresh()` (`0` args, `id_type = "charid"`).
// parity: notify:OnSkillForcedRefresh
pub fn emit_skill_forced_refresh(ctx: &ServerContext, char_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillForcedRefresh",
        "charid",
        vec![],
    );
}

/// `skillMgr2.ForceSkillRefresh(targetCharID=None)`: catches up skill queue, migrates obsolete
/// skills, updates brain state, and emits `OnSkillForcedRefresh()`.
// parity: notify:OnSkillForcedRefresh
pub fn handle_force_skill_refresh(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_id) = ServerContext::require_character(session)?;
    let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_id);
    if target_char_id != caller_id && !session.has_gm_role() {
        return Err(err(
            "CharacterOwnershipMismatch",
            format!("Cannot force skill refresh for character {target_char_id}"),
        ));
    }
    let exists = ctx
        .db
        .with_snapshot(|snap| snap.characters.contains_key(&target_char_id));
    if !exists {
        return Err(err(
            "CharacterNotFound",
            format!("Character {target_char_id} not found"),
        ));
    }
    super::character_skills::catch_up_skill_queue(ctx, target_char_id);
    let (migrated_count, refunded_sp) =
        super::character_skill_ops::migrate_obsolete_skills_for_character(ctx, target_char_id);
    emit_skill_forced_refresh(ctx, target_char_id);
    emit_server_brain_updated(ctx, target_char_id);
    Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(target_char_id)),
        ("migratedSkills", PyRep::Integer(migrated_count as i64)),
        ("refundedSP", PyRep::Real(refunded_sp)),
    ]))
}

/// Emits `OnMultipleCharactersTrainingUpdated()` (`0` args, `id_type = "charid"`) to all characters on `user_id`.
// parity: notify:OnMultipleCharactersTrainingUpdated
pub fn emit_multiple_characters_training_updated(ctx: &ServerContext, user_id: u64) {
    let char_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .map(|c| c.character_id)
            .collect();
        ids.sort_unstable();
        ids
    });
    for cid in char_ids {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnMultipleCharactersTrainingUpdated",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnMilestoneTimerStarted(timeLeft)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnMilestoneTimerStarted
pub fn emit_login_milestone_timer_started(ctx: &ServerContext, char_id: i64, time_left_ft: i64) {
    emit_milestone_timer_started(ctx, char_id, time_left_ft);
}

/// Emits V24-conformant 2-arg `OnLoginMilestoneAchieved(milestoneID, iskAmount)` (`id_type = "charid"`).
// parity: notify:OnLoginMilestoneAchieved
pub fn emit_login_milestone_achieved_v24(
    ctx: &ServerContext,
    char_id: i64,
    milestone_id: i64,
    isk_amount: f64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnLoginMilestoneAchieved",
        "charid",
        vec![PyRep::Integer(milestone_id), PyRep::Real(isk_amount)],
    );
}

/// Evaluates due 120-minute login milestones and expired 24-hour Daily Alpha Injector cooldowns on cluster tick.
// parity: notify:OnLoginMilestoneAchieved, notify:OnAlphaInjectionAvailable
pub fn sweep_due_login_milestones_and_alpha_injectors(ctx: &ServerContext, now_unix: u64) {
    const MILESTONE_DURATION_SECS: u64 = 7_200;
    const MILESTONE_REWARD_ISK: i64 = 500_000;
    let now_ft = ctx.now_blueos_filetime_ticks();

    let (achieved_chars, ready_alpha_chars) = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let mut achieved = Vec::new();
        let mut logon_cids: Vec<i64> = ext.milestone_logons.keys().copied().collect();
        logon_cids.sort_unstable();
        for cid in logon_cids {
            let Some(&logon_unix) = ext.milestone_logons.get(&cid) else {
                continue;
            };
            if now_unix < logon_unix.saturating_add(MILESTONE_DURATION_SECS) {
                continue;
            }
            let already = ext
                .claimed_milestones
                .get(&cid)
                .is_some_and(|v| v.contains(&1));
            if already {
                continue;
            }
            ext.claimed_milestones.entry(cid).or_default().push(1);
            let reward_cents = IskCents::from_isk(MILESTONE_REWARD_ISK);
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                let bal_after = ch.balance;
                snap.append_journal(
                    cid,
                    "LoginMilestoneReward",
                    reward_cents,
                    bal_after,
                    "Daily 120-minute login milestone reward",
                );
                achieved.push((cid, bal_after.to_wire_f64()));
            }
        }
        if !achieved.is_empty() {
            save_cosmetics_ui_state(snap, &ext);
        }

        let cooldowns = super::implant_plugging::load_alpha_injector_cooldowns(snap);
        let mut notified_map: HashMap<i64, bool> = snap
            .service_state_blobs
            .get("alpha_avail_notified")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let mut alpha_ready = Vec::new();
        let mut cd_cids: Vec<i64> = cooldowns.keys().copied().collect();
        cd_cids.sort_unstable();
        for cid in cd_cids {
            let Some(&last_ft) = cooldowns.get(&cid) else {
                continue;
            };
            let is_alpha = snap
                .characters
                .get(&cid)
                .is_some_and(|c| c.clone_grade <= 1);
            if !is_alpha
                || now_ft
                    < last_ft.saturating_add(super::implant_plugging::ALPHA_INJECTOR_COOLDOWN_TICKS)
                || notified_map.get(&cid).copied().unwrap_or(false)
            {
                continue;
            }
            notified_map.insert(cid, true);
            alpha_ready.push(cid);
        }
        if !alpha_ready.is_empty() {
            if let Ok(json) = serde_json::to_string(&notified_map) {
                snap.service_state_blobs
                    .insert("alpha_avail_notified".into(), json);
            }
        }
        (achieved, alpha_ready)
    });

    for (cid, bal_wire) in achieved_chars {
        let _ = ctx.notify_account_change(cid, "cash", cid, bal_wire);
        emit_login_milestone_achieved_v24(ctx, cid, 1, MILESTONE_REWARD_ISK as f64);
    }
    for cid in ready_alpha_chars {
        emit_alpha_injection_available(ctx, cid);
    }
}

/// Emits `OnRedeemingQueueUpdated()` (`0` args, `id_type = "charid"`) to `preferred_char_id` or all characters on `user_id`.
// parity: notify:OnRedeemingQueueUpdated
pub fn emit_redeeming_queue_updated_for_user(
    ctx: &ServerContext,
    user_id: u64,
    preferred_char_id: Option<i64>,
) {
    let char_ids: Vec<i64> = if let Some(cid) = preferred_char_id {
        vec![cid]
    } else {
        ctx.db.with_snapshot(|snap| {
            let mut ids: Vec<i64> = snap
                .characters
                .values()
                .filter(|c| c.user_id == user_id)
                .map(|c| c.character_id)
                .collect();
            ids.sort_unstable();
            ids
        })
    };
    for cid in char_ids {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnRedeemingQueueUpdated",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnPodOrCorvetteAssembly(item_id, type_id)` (`2` args, `id_type = "charid"`).
// parity: notify:OnPodOrCorvetteAssembly
pub fn emit_pod_or_corvette_assembly(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    type_id: TypeId,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPodOrCorvetteAssembly",
        "charid",
        vec![PyRep::Integer(item_id), PyRep::Integer(i64::from(type_id))],
    );
}

/// Emits `OnPasswordChanged(which, containerItem)` (`2` args, `id_type = "charid"`).
// parity: notify:OnPasswordChanged
pub fn emit_password_changed(
    ctx: &ServerContext,
    char_id: i64,
    container_id: i64,
    type_id: TypeId,
) {
    let container_item = PyRep::keyval([
        ("itemID", PyRep::Integer(container_id)),
        ("typeID", PyRep::Integer(i64::from(type_id))),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPasswordChanged",
        "charid",
        vec![PyRep::Integer(1), container_item],
    );
}

/// Emits `OnPlayerStateUpdated(playerState)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnPlayerStateUpdated
pub fn emit_player_state_updated(ctx: &ServerContext, char_id: i64, player_state_rep: PyRep) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlayerStateUpdated",
        "charid",
        vec![player_state_rep],
    );
}

/// Emits `OnServerBrainUpdated((brainVersion, grayMatter))` (`1` arg, `id_type = "charid"`),
/// marshaling `(charEffects, shipEffects, structureEffects)` for V24 `BaseDogmaLocation.ProcessBrainData`.
// parity: notify:OnServerBrainUpdated
pub fn emit_server_brain_updated(ctx: &ServerContext, char_id: i64) {
    let (version, gray_matter) = ctx.db.with_mut_snapshot(|snap| {
        let mut versions: HashMap<i64, i64> = snap
            .service_state_blobs
            .get("char_brain_versions")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let next_ver = versions.get(&char_id).copied().unwrap_or(0).saturating_add(1);
        versions.insert(char_id, next_ver);
        if let Ok(json) = serde_json::to_string(&versions) {
            snap.service_state_blobs
                .insert("char_brain_versions".into(), json);
        }

        let mut skill_ids: Vec<TypeId> = snap
            .skills
            .get(&char_id)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();
        skill_ids.sort_unstable();
        let char_effects: Vec<PyRep> = skill_ids
            .into_iter()
            .filter_map(|tid| {
                let lvl = snap.skills.get(&char_id)?.get(&tid)?.level;
                Some(PyRep::Tuple(vec![
                    PyRep::Integer(i64::from(tid)),
                    PyRep::Integer(i64::from(lvl)),
                ]))
            })
            .collect();

        let ship_effects: Vec<PyRep> = snap
            .active_implants
            .get(&char_id)
            .map(|imps| {
                imps.iter()
                    .map(|&tid| PyRep::Integer(i64::from(tid)))
                    .collect()
            })
            .unwrap_or_default();

        let brain_tuple = PyRep::Tuple(vec![
            PyRep::List(char_effects),
            PyRep::List(ship_effects),
            PyRep::List(Vec::new()),
        ]);
        (next_ver, eve_proto::marshal::encode(&brain_tuple))
    });

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerBrainUpdated",
        "charid",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(version),
            PyRep::Bytes(gray_matter),
        ])],
    );
}

/// Emits `OnSubscriptionChangedServer(newState)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnSubscriptionChangedServer
pub fn emit_subscription_changed_server(ctx: &ServerContext, char_id: i64, new_grade: u32) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSubscriptionChangedServer",
        "charid",
        vec![PyRep::Integer(i64::from(new_grade))],
    );
}

/// Emits `OnSubscriptionTimeUpdateServer(omegaTime)` (`1` arg, `id_type = "charid"`) to all characters on `user_id`.
// parity: notify:OnSubscriptionTimeUpdateServer
pub fn emit_subscription_time_update_server(ctx: &ServerContext, user_id: u64, until_unix: u64) {
    let omega_ft = if until_unix > 0 {
        (until_unix as i64)
            .saturating_add(11_644_473_600)
            .saturating_mul(10_000_000)
    } else {
        0
    };
    let char_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .map(|c| c.character_id)
            .collect();
        ids.sort_unstable();
        ids
    });
    for cid in char_ids {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSubscriptionTimeUpdateServer",
            "charid",
            vec![PyRep::Integer(omega_ft)],
        );
    }
}

/// Handles `corpRegistry.SetAllowWar` / `GetAllowWar`, emitting `OnAllowWarUpdated(corpID, allowWar)`
/// strictly to `NotificationTarget::Corporation(corp_id)` (`id_type = "corpid"`).
// parity: notify:OnAllowWarUpdated
/// Emits `OnAllowWarUpdated(corpID, allowWar)` to all online members of `corp_id`.
// parity: notify:OnAllowWarUpdated
pub fn emit_allow_war_updated(ctx: &ServerContext, corp_id: i64, allow_war: bool) {
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnAllowWarUpdated",
        "corpid",
        vec![PyRep::Integer(corp_id), PyRep::Bool(allow_war)],
    );
}

pub fn handle_corp_allow_war_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let corp_id = session.corporation_id.unwrap_or(0);
    if method == "GetAllowWar" {
        let allow = ctx.db.with_snapshot(|snap| {
            let map: HashMap<i64, bool> = snap
                .service_state_blobs
                .get("corp_allow_war")
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            map.get(&corp_id).copied().unwrap_or(true)
        });
        return Ok(PyRep::Bool(allow));
    }
    let Some(allow_war) = args.first().and_then(|a| a.as_bool()) else {
        return Err(err(
            "InvalidArgument",
            "SetAllowWar requires boolean allowWar argument",
        ));
    };
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(err(
                "CorporationNotFound",
                format!("Corporation {corp_id} not found"),
            ));
        };
        let roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        if corp.ceo_id != char_id
            && !corp_roles::has_role(roles, corp_roles::ROLE_DIRECTOR)
            && !session.has_gm_role()
        {
            return Err(err(
                "CorpRoleDenied",
                "SetAllowWar requires CEO or Director role",
            ));
        }
        let mut map: HashMap<i64, bool> = snap
            .service_state_blobs
            .get("corp_allow_war")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        map.insert(corp_id, allow_war);
        if let Ok(json) = serde_json::to_string(&map) {
            snap.service_state_blobs
                .insert("corp_allow_war".into(), json);
        }
        Ok(())
    })?;
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnAllowWarUpdated",
        "corpid",
        vec![PyRep::Integer(corp_id), PyRep::Bool(allow_war)],
    );
    Ok(PyRep::keyval([
        ("corporationID", PyRep::Integer(corp_id)),
        ("allowWar", PyRep::Bool(allow_war)),
    ]))
}

/// Handles `corpFittingMgr.SaveCommunityFitting` / `DeleteCommunityFitting` (`owner_id == 1`),
/// broadcasting `OnCommunityFittingsUpdated()` (`0` args, `NotificationTarget::All`).
// parity: notify:OnCommunityFittingsUpdated
pub fn handle_community_fitting_admin_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let allowed_roles =
        eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_GML | eve_db::service_roles::ROLE_CONTENT;
    if !session.has_gm_role() && (session.role & allowed_roles) == 0 {
        return Err(err(
            "InsufficientRole",
            format!("corpFittingMgr.{method} requires GM or Content role"),
        ));
    }
    if method == "DeleteCommunityFitting" {
        let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        if fid <= 0 {
            return Err(err("InvalidFittingID", "Valid fittingID required"));
        }
        let deleted = ctx.db.with_mut_snapshot(|snap| {
            if snap.saved_fittings.get(&fid).is_some_and(|f| f.owner_id == 1) {
                snap.saved_fittings.remove(&fid);
                true
            } else {
                false
            }
        });
        if !deleted {
            return Err(err(
                "FittingNotFound",
                format!("Community fitting {fid} not found"),
            ));
        }
        emit_community_fittings_updated(ctx);
        return Ok(PyRep::Bool(true));
    }

    let (name, ship_type_id, description) = match args.first() {
        Some(PyRep::KeyVal(kv)) => (
            kv.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
            kv.get("shipTypeID")
                .and_then(|v| v.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
            kv.get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        _ => (
            args.first().and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
            args.get(1)
                .and_then(|v| v.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
            args.get(2)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
    };
    if name.is_empty() {
        return Err(err(
            "InvalidFittingName",
            "Community fitting name cannot be empty",
        ));
    }
    let valid_ship = ctx
        .sde
        .types
        .get(&ship_type_id)
        .is_some_and(|t| t.category_id == 6);
    if !valid_ship {
        return Err(err(
            "InvalidShipType",
            format!("Type {ship_type_id} is not a valid SDE ship"),
        ));
    }
    let fid = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.saved_fittings.insert(
            id,
            SavedFittingRecord {
                fitting_id: id,
                owner_id: 1,
                name,
                ship_type_id,
                description,
                items: Vec::new(),
            },
        );
        id
    });
    emit_community_fittings_updated(ctx);
    Ok(PyRep::Integer(fid))
}

/// Handles `contextualOfferMgr.DeliverOffer` / `TriggerOffer`, emitting `OnOfferReceived(offerID, offer)`.
// parity: notify:OnOfferReceived
/// Emits `OnOfferReceived(offerID, offerKeyVal)` to `char_id`.
// parity: notify:OnOfferReceived
pub fn emit_offer_received(ctx: &ServerContext, char_id: i64, offer_id: i64) {
    let offer_kv = PyRep::keyval([
        ("offerID", PyRep::Integer(offer_id)),
        ("title", PyRep::String(format!("Offer #{offer_id}"))),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOfferReceived",
        "charid",
        vec![PyRep::Integer(offer_id), offer_kv],
    );
}

pub fn handle_deliver_contextual_offer(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_id) = ServerContext::require_character(session)?;
    let offer_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if offer_id <= 0 {
        return Err(err(
            "InvalidOfferID",
            "DeliverOffer requires a positive offerID",
        ));
    }
    let target_char_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(caller_id);
    if target_char_id != caller_id && !session.has_gm_role() {
        return Err(err(
            "InsufficientRole",
            "Delivering contextual offers to another character requires GM role",
        ));
    }
    let title = args
        .get(2)
        .and_then(|a| a.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Special Pilot Offer")
        .to_string();
    let now_unix = ctx.now_unix();
    let now_ft = ctx.now_blueos_filetime_ticks();

    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&target_char_id) {
            return Err(err(
                "CharacterNotFound",
                format!("Character {target_char_id} not found"),
            ));
        }
        let mut ext = load_cosmetics_ui_state(snap);
        let log = ext
            .contextual_offer_events
            .entry(target_char_id)
            .or_default();
        log.push(("OfferReceived".into(), offer_id, now_unix));
        save_cosmetics_ui_state(snap, &ext);
        Ok(())
    })?;

    let offer_kv = PyRep::keyval([
        ("offerID", PyRep::Integer(offer_id)),
        ("title", PyRep::String(title)),
        ("deliveredAt", PyRep::Integer(now_ft)),
        ("active", PyRep::Bool(true)),
    ]);
    ctx.notify(
        NotificationTarget::Character(target_char_id),
        "OnOfferReceived",
        "charid",
        vec![PyRep::Integer(offer_id), offer_kv.clone()],
    );
    Ok(offer_kv)
}

/// Emits `OnNesIntroStateChanged(character_id, new_state_id)` (`2` args, `id_type = "charid"`).
// parity: notify:OnNesIntroStateChanged
pub fn emit_nes_intro_state_changed(ctx: &ServerContext, char_id: i64, new_state_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNesIntroStateChanged",
        "charid",
        vec![PyRep::Integer(char_id), PyRep::Integer(new_state_id)],
    );
}

/// Handles `nes_intro.set_nes_intro_state(state_id)`, validating `0..=2` and emitting `OnNesIntroStateChanged`.
// parity: notify:OnNesIntroStateChanged
pub fn handle_nes_intro_set_state(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(state_id) = args.first().and_then(|a| a.as_i64()).filter(|&s| (0..=2).contains(&s)) else {
        return Err(err(
            "InvalidArgument",
            "set_nes_intro_state requires integer state_id in 0..=2",
        ));
    };
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        if state_id == 0 {
            ext.nes_intro_skipped.remove(&char_id);
        } else {
            ext.nes_intro_skipped.insert(char_id, now_unix);
        }
        save_cosmetics_ui_state(snap, &ext);
    });
    emit_nes_intro_state_changed(ctx, char_id, state_id);
    Ok(PyRep::Integer(state_id))
}

pub use crate::services::notify_emitters_batch9_crates::handle_crate_service;


/// Extracted R&D agent datacore purchase handler (action `14`), emitting `OnDatacoreBought(charID, agentID, balance)`.
// parity: notify:OnDatacoreBought
pub fn handle_rnd_buy_datacores(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    agent_id: i64,
    sys_id: u32,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let qty_opt = match args.get(2).and_then(|a| a.as_i64()) {
        Some(raw) => i32::try_from(raw).ok().filter(|&q| (1..=100_000).contains(&q)),
        None => Some(1),
    };
    let Some(qty) = qty_opt else {
        return Err(err(
            "InvalidDatacoreQuantity",
            "Datacore quantity must be between 1 and 100,000",
        ));
    };
    if let Some(agent_info) = ctx.sde.agents.get(&agent_id) {
        if agent_info.solar_system_id.is_some_and(|asys| asys != sys_id)
            || (session.station_id.is_some()
                && agent_info.station_id.is_some()
                && agent_info.station_id != session.station_id)
        {
            return Err(err(
                "AgentNotCoLocated",
                format!("Must be co-located with R&D agent {agent_id} to redeem datacores"),
            ));
        }
    }
    let now_unix = ctx.now_unix();
    let station_id = ctx
        .sde
        .agents
        .get(&agent_id)
        .and_then(|a| a.station_id)
        .or(session.station_id)
        .unwrap_or(60_003_760);
    let (item_id, dc_type_id, rem_rp, bal_after_isk) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut entries = super::agents_missions::load_rnd_research_entries(snap, char_id);
        let Some(entry) = entries.iter_mut().find(|e| e.agent_id == agent_id) else {
            return Err(err(
                "ResearchNotActive",
                format!("No active R&D research with agent {agent_id}"),
            ));
        };
        let elapsed_days = (now_unix.saturating_sub(entry.started_unix) as f64) / 86_400.0;
        let total_rp = entry.initial_points + entry.points_per_day * elapsed_days;
        let rp_cost = 50.0 * (qty as f64);
        if total_rp + 1e-6 < rp_cost {
            return Err(err(
                "InsufficientResearchPoints",
                format!("Buying {qty} datacore(s) requires {rp_cost:.1} RP, character has {total_rp:.1} RP"),
            ));
        }
        let isk_cost = IskCents::from_isk(10_000 * i64::from(qty));
        let mut bal_after_opt = None;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            if ch.balance < isk_cost {
                return Err(err(
                    "InsufficientFunds",
                    "Insufficient ISK to redeem R&D datacores",
                ));
            }
            ch.balance = ch.balance.checked_sub(isk_cost).unwrap_or(IskCents(0));
            bal_after_opt = Some(ch.balance);
        }
        let bal_wire = bal_after_opt.map(|b| b.to_wire_f64()).unwrap_or(0.0);
        if let Some(bal_after) = bal_after_opt {
            snap.append_journal(
                char_id,
                "DatacoreFee",
                IskCents(-isk_cost.0),
                bal_after,
                format!("Redeemed {qty} datacore(s) from R&D agent {agent_id}"),
            );
        }
        let rem_rp = (total_rp - rp_cost).max(0.0);
        entry.initial_points = rem_rp;
        entry.started_unix = now_unix;
        let dc_type_id: TypeId = if entry.research_type_id == 11452 {
            20172
        } else {
            20424
        };
        super::agents_missions::save_rnd_research_entries(snap, char_id, &entries);
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: dc_type_id,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        Ok((item_id, dc_type_id, rem_rp, bal_wire))
    })?;
    let _ = ctx.notify_account_change(char_id, "cash", char_id, bal_after_isk);
    emit_datacore_bought(ctx, char_id, agent_id, bal_after_isk);
    Ok(PyRep::keyval([
        ("agentID", PyRep::Integer(agent_id)),
        ("datacoreItemID", PyRep::Integer(item_id)),
        ("datacoreTypeID", PyRep::Integer(dc_type_id as i64)),
        ("quantity", PyRep::Integer(qty as i64)),
        ("remainingResearchPoints", PyRep::Real(rem_rp)),
    ]))
}
