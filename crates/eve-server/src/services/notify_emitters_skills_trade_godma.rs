//! `notify_emitters_skills_trade_godma` — Server-side V24 notification emitters & handlers for
//! skills, injectors, milestones, redeeming, trade priming, market repackage, Godma, and localization.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_character_services::{
    emit_alpha_injection_available,
    emit_datacore_bought, emit_milestone_timer_started,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use crate::services::notify_emitters_space_dungeons::{
    emit_module_activation_godma_notifications, flush_module_profile_and_ewar_end,
};
use std::collections::{BTreeMap, HashMap};

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

pub use crate::services::crate_loot_service::handle_crate_service;


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
    if session.station_id.is_none()
        || agent_info.solar_system_id.is_some_and(|asys| asys != sys_id)
        || agent_info.station_id != session.station_id
    {
        return Err(err(
            "AgentNotCoLocated",
            format!("Must be docked in R&D agent {agent_id}'s station to redeem datacores"),
        ));
    }
    let now_unix = ctx.now_unix();
    let Some(station_id) = session.station_id else {
        return Err(err(
            "AgentNotCoLocated",
            format!("Must be docked in R&D agent {agent_id}'s station to redeem datacores"),
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
        let Some(dc_type_id) = super::pve_open_feats_b36::resolve_datacore_for_research_skill(
            &ctx.sde,
            entry.research_type_id,
        ) else {
            return Err(err(
                "InvalidResearchSkill",
                format!("Research field {} does not produce a redeemable SDE datacore", entry.research_type_id),
            ));
        };
        let elapsed_days = (now_unix.saturating_sub(entry.started_unix) as f64) / 86_400.0;
        let total_rp = entry.initial_points + entry.points_per_day * elapsed_days;
        let max_offered = super::pve_open_feats_b36::max_affordable_datacores(total_rp);
        let rp_cost = 100.0 * (qty as f64);
        if qty > max_offered || total_rp + 1e-6 < rp_cost {
            return Err(err(
                "InsufficientResearchPoints",
                format!("Buying {qty} datacore(s) requires {rp_cost:.1} RP, character has {total_rp:.1} RP (max {max_offered})"),
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

/// Emits `OnPrimingNeededForTradeItems(itemIDsToPrime)` (`1` arg, `id_type = "charid"`) to the trade partner.
// parity: notify:OnPrimingNeededForTradeItems
pub fn emit_priming_needed_for_trade_items(
    ctx: &ServerContext,
    partner_char_id: i64,
    item_ids: &[i64],
) {
    if item_ids.is_empty() || partner_char_id <= 0 {
        return;
    }
    let list = item_ids.iter().copied().map(PyRep::Integer).collect();
    ctx.notify(
        NotificationTarget::Character(partner_char_id),
        "OnPrimingNeededForTradeItems",
        "charid",
        vec![PyRep::List(list)],
    );
}

/// Counts how many items in a multi-sell order are assembled and damaged (hull, armor, or fitted module/heat damage).
pub fn count_damaged_sell_items(
    snap: &eve_db::WorldStateSnapshot,
    damaged_fitted_ships: &std::collections::HashSet<i64>,
    char_id: i64,
    caller_corp_id: i64,
    use_corp: bool,
    parsed_items: &[(i64, u32, i64, eve_db::IskCents, i32, i64)],
) -> i64 {
    let mut damaged_count: i64 = 0;
    for (station_id, type_id, raw_item_id, _, _, _) in parsed_items {
        let candidate_opt = if *raw_item_id > 0 {
            snap.items.get(raw_item_id)
        } else {
            snap.items.values().find(|i| {
                i.type_id == *type_id
                    && i.singleton
                    && (snap.is_item_consumable_by(i, char_id, Some(*station_id), None)
                        || (use_corp
                            && i.owner_id == caller_corp_id
                            && i.location_id == *station_id
                            && i.flag != eve_db::InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)))
            })
        };
        if let Some(item) = candidate_opt {
            if item.singleton {
                let hull_or_armor_dmg = item
                    .dynamic_attributes
                    .get(&eve_sde::attr_ids::DAMAGE)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
                    || item
                        .dynamic_attributes
                        .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                        .copied()
                        .unwrap_or(0.0)
                        > 0.0;
                let module_dmg = damaged_fitted_ships.contains(&item.item_id);
                if hull_or_armor_dmg || module_dmg {
                    damaged_count += 1;
                }
            }
        }
    }
    damaged_count
}

/// Emits `OnSellRepackageErrors(repackageErrorCount)` (`1` arg, `id_type = "charid"`).
// parity: notify:OnSellRepackageErrors
pub fn emit_sell_repackage_errors(
    ctx: &ServerContext,
    char_id: i64,
    error_code: &str,
    count: i64,
) {
    if count <= 0 || error_code != "CantRepackageDamagedItem" {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSellRepackageErrors",
        "charid",
        vec![PyRep::Dict(vec![(
            PyRep::String("CantRepackageDamagedItem".into()),
            PyRep::Integer(count),
        )])],
    );
}

/// Extracted `dogmaIM.SetModuleOnline` handler emitting `OnGodmaPrimeItem` and `OnGodmaEffectInfo`.
// parity: notify:OnGodmaEffectInfo, notify:OnGodmaPrimeItem
pub async fn handle_dogma_set_module_online(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.ship_id)
        .unwrap_or(0);
    let module_id = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .unwrap_or(0);
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
        return crate::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
            ctx, session, ship_id, module_id, true,
        );
    }
    let (owns_ship, mod_type_id) = ctx.db.with_snapshot(|s| {
        let owns = s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id);
        let mtid = s.items.get(&module_id).map(|m| m.type_id).unwrap_or(0);
        (owns, mtid)
    });
    if !owns_ship {
        return Err(err(
            "ShipOwnershipViolation",
            format!("Ship {ship_id} is not owned by character {char_id}"),
        ));
    }

    let is_burned = ctx.db.with_snapshot(|s| {
        let it = s.items.get(&module_id);
        let dmg = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
            .unwrap_or(0.0);
        let hp = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            .or_else(|| {
                it.and_then(|i| ctx.sde.types.get(&i.type_id))
                    .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            })
            .unwrap_or(40.0);
        dmg >= hp - 1e-6
    });
    if is_burned && session.station_id.is_none() && session.structure_id.is_none() {
        return Err(err(
            "ModuleBurnedOut",
            format!("Module {module_id} is burned out and cannot be onlined in space"),
        ));
    }

    if session.station_id.is_none() && session.structure_id.is_none() {
        if let Some(sys_id) = session.solar_system_id {
            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                if cs.max_capacitor > 0.0 && cs.capacitor < 0.75 * cs.max_capacitor {
                    return Err(err(
                        "NotEnoughCapacitorToOnline",
                        format!(
                            "Capacitor level ({:.1} GJ / {:.1} GJ) is below 75% requirement to online modules in space",
                            cs.capacitor, cs.max_capacitor
                        ),
                    ));
                }
            }
        }
    }

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    if let Err(e) = fit.set_module_online(module_id, &ctx.sde) {
        return Err(err("FittingValidationFailed", e));
    }
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(module_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let _ = ctx.notify_module_online_state(char_id, module_id, true);
    if mod_type_id > 0 {
        emit_module_activation_godma_notifications(
            ctx,
            char_id,
            ship_id,
            module_id,
            mod_type_id,
            "online",
            1.0,
            None,
        );
    }

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("online", PyRep::Bool(true)),
    ]))
}

/// Extracted `dogmaIM.TakeModuleOffline` handler emitting `OnGodmaFlushLocationProfile`.
// parity: notify:OnGodmaFlushLocationProfile
pub async fn handle_dogma_take_module_offline(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.ship_id)
        .unwrap_or(0);
    let module_id = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .unwrap_or(0);
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
        return crate::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
            ctx, session, ship_id, module_id, false,
        );
    }
    let owns_ship = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
    if !owns_ship {
        return Err(err(
            "ShipOwnershipViolation",
            format!("Ship {ship_id} is not owned by character {char_id}"),
        ));
    }

    let is_burned = ctx.db.with_snapshot(|s| {
        let it = s.items.get(&module_id);
        let dmg = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
            .unwrap_or(0.0);
        let hp = it
            .and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            .or_else(|| {
                it.and_then(|i| ctx.sde.types.get(&i.type_id))
                    .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
            })
            .unwrap_or(40.0);
        dmg >= hp - 1e-6
    });
    if is_burned && session.station_id.is_none() && session.structure_id.is_none() {
        return Err(err(
            "ModuleBurnedOut",
            format!("Module {module_id} is burned out and cannot be offlined in space"),
        ));
    }

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let was_active_type = fit
        .modules
        .get(&module_id)
        .filter(|m| matches!(m.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded))
        .map(|m| m.type_id);
    if let Err(e) = fit.take_module_offline(module_id, &ctx.sde) {
        return Err(err("FittingValidationFailed", e));
    }
    if let Some(m_tid) = was_active_type {
        let mod_state = fit.build_effective_module_state(module_id, m_tid, false, &ctx.sde);
        let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
        if react_delay_ms > 0.0 {
            ctx.record_module_reactivation_delay(ship_id, module_id, react_delay_ms / 1000.0);
        }
    }
    let offline_mod_ids: Vec<i64> = fit
        .modules
        .values()
        .filter(|m| m.state == eve_dogma::ModuleState::Offline)
        .map(|m| m.item_id)
        .collect();
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(module_id, false);
        snap.module_active_states.remove(&module_id);
        for &mid in &offline_mod_ids {
            snap.module_online_states.insert(mid, false);
            snap.module_active_states.remove(&mid);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut cleanup_mods = vec![module_id];
    for &mid in &offline_mod_ids {
        if mid != module_id {
            cleanup_mods.push(mid);
        }
    }
    let mut affected_ewar = Vec::new();
    for &mid in &cleanup_mods {
        affected_ewar.extend(flush_module_profile_and_ewar_end(
            ctx,
            char_id,
            session.solar_system_id,
            ship_id,
            mid,
        ));
        crate::services::tactical_modules::remove_projected_remote_assistance(ctx, ship_id, mid);
    }
    if let Some(sys_id) = session.solar_system_id {
        let sys = ctx.systems.get_or_spawn(sys_id);
        let is_cycle_gated = ctx
            .db
            .with_snapshot(|s| s.items.get(&module_id).map(|i| i.type_id))
            .and_then(|tid| ctx.sde.types.get(&tid))
            .is_some_and(|t| {
                matches!(
                    t.group_id,
                    40 | 1156 | 41 | 1697 | 67 | 71 | 76 | 72 | 1122 | 650 | 80 | 353
                )
            });
        if let Ok(Some(rem_delay)) = sys.remove_module_cycle(ship_id, module_id).await {
            if is_cycle_gated && rem_delay > 0.0 {
                ctx.record_module_reactivation_delay(ship_id, module_id, rem_delay);
            }
        }
        for &mid in &offline_mod_ids {
            if mid != module_id {
                let _ = sys.remove_module_cycle(ship_id, mid).await;
            }
        }
        for tid in affected_ewar {
            if let Ok(Some(mut t_cs)) = sys.get_combat_state(tid).await {
                let base_sig = t_cs.base_signature_radius.unwrap_or(t_cs.signature_radius);
                t_cs.signature_radius = ctx.effective_target_signature_radius(tid, base_sig);
                let _ = sys.set_combat_state(tid, t_cs).await;
            }
        }
    }

    let _ = ctx.notify_module_online_state(char_id, module_id, false);

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("online", PyRep::Bool(false)),
    ]))
}

/// Emits `OnGodmaEffectInfo` for a module activation event.
// parity: notify:OnGodmaEffectInfo
pub fn emit_godma_effect_info_for_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: TypeId,
    effect_name: &str,
    duration_s: f64,
    target_id_opt: Option<i64>,
) {
    emit_module_activation_godma_notifications(
        ctx,
        char_id,
        ship_id,
        module_id,
        module_type_id,
        effect_name,
        duration_s,
        target_id_opt,
    );
}

/// Emits V24-conformant 6-column `OnCfgDataChanged("evelocations", [locationID, locationName, x, y, z, None])`
/// (`id_type = "solarsystemid"`) matching `config_service.rs` `evelocations` schema.
// parity: notify:OnCfgDataChanged
pub fn emit_structure_cfg_data_changed(
    ctx: &ServerContext,
    sys_id: u32,
    structure_id: i64,
    new_name: &str,
    pos: [f64; 3],
) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnCfgDataChanged",
        "solarsystemid",
        vec![
            PyRep::String("evelocations".into()),
            PyRep::List(vec![
                PyRep::Integer(structure_id),
                PyRep::String(new_name.to_string()),
                PyRep::Real(pos[0]),
                PyRep::Real(pos[1]),
                PyRep::Real(pos[2]),
                PyRep::None,
            ]),
        ],
    );
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FlaggedSiteEntry {
    pub site_url: String,
    pub site_flag: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum StoredFlaggedSite {
    Structured(FlaggedSiteEntry),
    LegacyUrl(String),
}

impl From<StoredFlaggedSite> for FlaggedSiteEntry {
    fn from(v: StoredFlaggedSite) -> Self {
        match v {
            StoredFlaggedSite::Structured(e) => e,
            StoredFlaggedSite::LegacyUrl(site_url) => Self {
                site_url,
                site_flag: 2,
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BrowserLockdownPolicy {
    lockdown: bool,
    default_home_page: String,
    #[serde(default)]
    flagged_sites: Vec<StoredFlaggedSite>,
}

impl Default for BrowserLockdownPolicy {
    fn default() -> Self {
        Self {
            lockdown: false,
            default_home_page: "https://www.eveonline.com/".to_string(),
            flagged_sites: Vec::new(),
        }
    }
}

fn load_browser_lockdown_policy(snap: &eve_db::WorldStateSnapshot) -> (bool, String, Vec<FlaggedSiteEntry>) {
    let raw = snap
        .service_state_blobs
        .get("browser_lockdown_policy")
        .and_then(|s| serde_json::from_str::<BrowserLockdownPolicy>(s).ok())
        .unwrap_or_default();
    let entries = raw.flagged_sites.into_iter().map(FlaggedSiteEntry::from).collect();
    (raw.lockdown, raw.default_home_page, entries)
}

fn compute_flagged_sites_hash(sites: &[FlaggedSiteEntry]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for site in sites {
        for b in site.site_url.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for b in site.site_flag.to_le_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn set_browser_lockdown_policy(
    snap: &mut eve_db::WorldStateSnapshot,
    lockdown: bool,
    default_home_page: &str,
    flagged_sites: Vec<(String, i64)>,
) {
    let policy = BrowserLockdownPolicy {
        lockdown,
        default_home_page: default_home_page.to_string(),
        flagged_sites: flagged_sites
            .into_iter()
            .map(|(site_url, site_flag)| {
                StoredFlaggedSite::Structured(FlaggedSiteEntry { site_url, site_flag })
            })
            .collect(),
    };
    if let Ok(json) = serde_json::to_string(&policy) {
        snap.service_state_blobs
            .insert("browser_lockdown_policy".to_string(), json);
    }
}

/// Dispatches `browserLockdownSvc` queries backed by `service_state_blobs["browser_lockdown_policy"]`.
pub fn handle_misc_cluster_localization(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("browserLockdownSvc", "IsBrowserInLockdown") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let lockdown = ctx
                .db
                .with_snapshot(|snap| load_browser_lockdown_policy(snap).0);
            Some(Ok(PyRep::Bool(lockdown)))
        }
        ("browserLockdownSvc", "GetDefaultHomePage") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let home = ctx
                .db
                .with_snapshot(|snap| load_browser_lockdown_policy(snap).1);
            Some(Ok(PyRep::String(home)))
        }
        ("browserLockdownSvc", "GetFlaggedSitesList") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let sites = ctx
                .db
                .with_snapshot(|snap| load_browser_lockdown_policy(snap).2);
            Some(Ok(PyRep::List(
                sites
                    .into_iter()
                    .map(|entry| {
                        PyRep::Dict(vec![
                            (PyRep::String("siteUrl".into()), PyRep::String(entry.site_url)),
                            (PyRep::String("siteFlag".into()), PyRep::Integer(entry.site_flag)),
                        ])
                    })
                    .collect(),
            )))
        }
        ("browserLockdownSvc", "GetFlaggedSitesHash") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let hash = ctx.db.with_snapshot(|snap| {
                let (_, _, sites) = load_browser_lockdown_policy(snap);
                compute_flagged_sites_hash(&sites)
            });
            Some(Ok(PyRep::String(hash)))
        }
        _ => None,
    }
}

#[doc(hidden)]
pub use handle_misc_cluster_localization as handle_misc_batch9;

/// Broadcasts `OnServerMessage(message)` to all connected clients.
// parity: notify:OnServerMessage
pub fn emit_server_message_broadcast(ctx: &ServerContext, message: &str) {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return;
    }
    ctx.notify(
        NotificationTarget::All,
        "OnServerMessage",
        "clientID",
        vec![PyRep::String(trimmed.to_string())],
    );
}

const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

/// Extracted `localizationServer.GetAllTextChanges` handler.
pub fn handle_get_all_text_changes(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    let key_id = user_id as i64;
    ctx.db.with_snapshot(|snap| {
        let qa_wrap = snap
            .service_state_blobs
            .get("localization:qa_wrap_users")
            .and_then(|s| serde_json::from_str::<BTreeMap<i64, bool>>(s).ok())
            .and_then(|m| m.get(&key_id).copied())
            .unwrap_or(false);
        let overrides: BTreeMap<i64, String> = snap
            .owner_notes
            .get(&LOCALIZATION_NOTE_ID)
            .and_then(|n| serde_json::from_str(&n.note).ok())
            .unwrap_or_default();
        if overrides.is_empty() {
            Ok(PyRep::None)
        } else {
            Ok(PyRep::Dict(
                overrides
                    .into_iter()
                    .map(|(id, txt)| {
                        let rendered = if qa_wrap {
                            // DERIVED: [QA:{id}] {txt} wrap format is derived (client qaSettings.pyj source not in repo).
                            format!("[QA:{id}] {txt}")
                        } else {
                            txt
                        };
                        (PyRep::Integer(id), PyRep::String(rendered))
                    })
                    .collect(),
            ))
        }
    })
}

/// Extracted `localizationServer.UpdateLocalizationQAWrap` handler.
pub fn handle_update_localization_qa_wrap(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    let qa_roles = eve_machonet::service_roles::ROLE_QA
        | eve_db::service_roles::ROLE_GMH
        | eve_db::service_roles::ROLE_GML
        | eve_machonet::service_roles::ROLE_PROGRAMMER
        | eve_db::service_roles::ROLE_CONTENT;
    if !session.has_gm_role() && (session.role & qa_roles) == 0 {
        return Err(err(
            "RoleRequired",
            "localizationServer.UpdateLocalizationQAWrap requires QA, GM, or Content/Programmer role",
        ));
    }
    let Some(enabled) = args.first().and_then(|a| {
        a.as_bool().or_else(|| match a {
            PyRep::Integer(0) => Some(false),
            PyRep::Integer(1) => Some(true),
            _ => None,
        })
    }) else {
        return Err(err(
            "UnimplementedRPC",
            "UpdateLocalizationQAWrap requires a boolean enableQaWrap flag",
        ));
    };
    let key_id = user_id as i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut map: BTreeMap<i64, bool> = snap
            .service_state_blobs
            .get("localization:qa_wrap_users")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if enabled {
            if map.len() >= 256 && !map.contains_key(&key_id) {
                if let Some(first_k) = map.keys().next().copied() {
                    map.remove(&first_k);
                }
            }
            map.insert(key_id, true);
        } else {
            map.remove(&key_id);
        }
        if let Ok(json) = serde_json::to_string(&map) {
            snap.service_state_blobs
                .insert("localization:qa_wrap_users".into(), json);
        }
    });
    Ok(PyRep::Bool(enabled))
}

/// Emits `OnEffectMessage(msgkey, args)` (`2` args, `id_type = "charid"`).
/// Matches `eve/client/script/environment/godma.pyj:Godma.OnEffectMessage`.
pub fn emit_effect_message(
    ctx: &ServerContext,
    char_id: i64,
    msg_key: &str,
    msg_args: &[(&str, PyRep)],
) {
    let args_dict = msg_args
        .iter()
        .map(|(k, v)| (PyRep::String((*k).to_string()), v.clone()))
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEffectMessage",
        "charid",
        vec![PyRep::String(msg_key.to_string()), PyRep::Dict(args_dict)],
    );
}

/// Emits `OnPlexDepositFailed(error_key, reference)` (`2` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:PlexDepositListener.OnPlexDepositFailed`.
pub fn emit_plex_deposit_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexDepositFailed",
        "charid",
        vec![PyRep::String(error_key.to_string()), reference],
    );
}

/// Emits `OnPlexWithdrawFailed(error_key, reference)` (`2` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:PlexWithdrawalListener.OnPlexWithdrawFailed`.
pub fn emit_plex_withdraw_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexWithdrawFailed",
        "charid",
        vec![PyRep::String(error_key.to_string()), reference],
    );
}
