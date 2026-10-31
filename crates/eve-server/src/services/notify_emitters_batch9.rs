//! `notify_emitters_batch9` — Server-side V24 notification emitters & handlers for Batch 9 (Part 1:
//! Skills/Injectors, Login/Milestones/Redeeming/Crates/Character, Diplomacy/Fittings/Agents/Offers/NES).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_batch3::{
    emit_alpha_injection_available,
    emit_datacore_bought, emit_milestone_timer_started,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

const BLOB_ALPHA_AVAIL_NOTIFIED: &str = "alpha_avail_notified";

fn load_alpha_avail_notified(snap: &WorldStateSnapshot) -> HashMap<i64, bool> {
    snap.service_state_blobs
        .get(BLOB_ALPHA_AVAIL_NOTIFIED)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_alpha_avail_notified(snap: &mut WorldStateSnapshot, map: &HashMap<i64, bool>) {
    if let Ok(s) = serde_json::to_string(map) {
        snap.service_state_blobs
            .insert(BLOB_ALPHA_AVAIL_NOTIFIED.to_string(), s);
    }
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Clears the Alpha injector ready notification latch when an Alpha injector is consumed.
pub fn clear_alpha_avail_notified(snap: &mut WorldStateSnapshot, char_id: i64) {
    let mut map = load_alpha_avail_notified(snap);
    if map.remove(&char_id).is_some() {
        save_alpha_avail_notified(snap, &map);
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
    let already_notified = ctx.db.with_snapshot(|snap| {
        load_alpha_avail_notified(snap)
            .get(&char_id)
            .copied()
            .unwrap_or(false)
    });
    if already_notified {
        return;
    }
    let should_emit = ctx.db.with_mut_snapshot(|snap| {
        let mut map = load_alpha_avail_notified(snap);
        if map.get(&char_id).copied().unwrap_or(false) {
            false
        } else {
            map.insert(char_id, true);
            save_alpha_avail_notified(snap, &map);
            true
        }
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

/// Emits `OnSkillBundleInjected(numberOfSkills, sp)` (`2` args, `id_type = "charid"`).
pub fn emit_skill_bundle_injected(ctx: &ServerContext, char_id: i64, num_skills: i64, sp: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillBundleInjected",
        "charid",
        vec![PyRep::Integer(num_skills), PyRep::Integer(sp)],
    );
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

/// Evaluates expired 24-hour Daily Alpha Injector cooldowns on cluster tick.
// parity: notify:OnAlphaInjectionAvailable
pub fn sweep_due_login_milestones_and_alpha_injectors(ctx: &ServerContext, _now_unix: u64) {
    let now_ft = ctx.now_blueos_filetime_ticks();

    let candidates = ctx.db.with_snapshot(|snap| {
        let cooldowns = super::implant_plugging::load_alpha_injector_cooldowns(snap);
        if cooldowns.is_empty() {
            return Vec::new();
        }
        let notified_map = load_alpha_avail_notified(snap);
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
            alpha_ready.push(cid);
        }
        alpha_ready
    });

    if candidates.is_empty() {
        return;
    }

    let ready_alpha_chars = ctx.db.with_mut_snapshot(|snap| {
        let mut notified_map = load_alpha_avail_notified(snap);
        let mut to_emit = Vec::new();
        for cid in candidates {
            if !notified_map.get(&cid).copied().unwrap_or(false) {
                notified_map.insert(cid, true);
                to_emit.push(cid);
            }
        }
        if !to_emit.is_empty() {
            save_alpha_avail_notified(snap, &notified_map);
        }
        to_emit
    });

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
    let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
        return Err(err(
            "UnknownAgent",
            format!("R&D agent {agent_id} does not exist in SDE"),
        ));
    };
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
    let now_unix = ctx.now_unix();
    let Some(station_id) = agent_info.station_id.or(session.station_id) else {
        return Err(err(
            "AgentLocationUnknown",
            format!("Cannot resolve station location for R&D agent {agent_id}"),
        ));
    };
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
        // DERIVED: 50 RP per datacore (V24 R&D datacore redemption cost)
        let rp_cost = 50.0 * (qty as f64);
        if total_rp + 1e-6 < rp_cost {
            return Err(err(
                "InsufficientResearchPoints",
                format!("Buying {qty} datacore(s) requires {rp_cost:.1} RP, character has {total_rp:.1} RP"),
            ));
        }
        // DERIVED: 10,000 ISK per datacore redemption fee
        let isk_cost = IskCents::from_isk(10_000 * i64::from(qty));
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        };
        if ch.balance < isk_cost {
            return Err(err(
                "InsufficientFunds",
                "Insufficient ISK to redeem R&D datacores",
            ));
        }
        ch.balance = ch.balance.checked_sub(isk_cost).unwrap_or(IskCents(0));
        let bal_after = ch.balance;
        let bal_wire = bal_after.to_wire_f64();
        snap.append_journal(
            char_id,
            "DatacoreFee",
            IskCents(-isk_cost.0),
            bal_after,
            format!("Redeemed {qty} datacore(s) from R&D agent {agent_id}"),
        );
        let rem_rp = (total_rp - rp_cost).max(0.0);
        entry.initial_points = rem_rp;
        entry.started_unix = now_unix;
        let skill_name = ctx
            .sde
            .types
            .get(&entry.research_type_id)
            .map(|t| t.name.as_str())
            .ok_or_else(|| {
                err(
                    "UnknownResearchSkill",
                    format!("Research skill {} not found in SDE", entry.research_type_id),
                )
            })?;
        let expected_dc_name = format!("Datacore - {skill_name}");
        let dc_type_id: TypeId = ctx
            .sde
            .types
            .values()
            .find(|t| t.name == expected_dc_name)
            .or_else(|| {
                ctx.sde.types.values().find(|t| {
                    t.group_id == 333
                        && t.default_attributes
                            .get(&182)
                            .is_some_and(|&req_sk| (req_sk.round() as u32) == entry.research_type_id)
                })
            })
            .map(|t| t.type_id)
            .ok_or_else(|| {
                err(
                    "UnknownDatacoreType",
                    format!("Datacore type '{expected_dc_name}' not found in SDE"),
                )
            })?;
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
