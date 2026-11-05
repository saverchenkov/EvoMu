//! `notify_emitters_combat_ess_progression` — Server-side V24 notification emitters for
//! progression, combat messages, station guest lists, scanner results, ESS banks, and death/pod pipelines.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::broadcast;

pub fn push_char_notification_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&RwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    event: &str,
    args: Vec<PyRep>,
) {
    if char_id <= 0 {
        return;
    }
    let Some(pend) = pending_notifications else {
        return;
    };
    let uid_opt = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
    let (packet, marshaled_packet) =
        ServerContext::build_macho_notification_packet(char_id, uid_opt, event, "charid", args.clone());
    let notif = PendingMachoNotification {
        recipient_char_id: char_id,
        event: event.to_string(),
        id_type: "charid".to_string(),
        args,
        packet,
        marshaled_packet,
        created_filetime: db.clock().now_blueos_filetime_ticks(),
    };
    let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
    let q = queues.entry(char_id).or_default();
    if q.len() >= 256 {
        q.remove(0);
    }
    q.push(notif.clone());
    if let Some(tx) = notify_tx {
        let _ = tx.send((char_id, notif));
    }
}

/// Emits `OnProgressionAdvanced(objective_id, last_completed_task_id)` (`id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionAdvanced`.
pub fn emit_progression_advanced(
    ctx: &ServerContext,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionAdvanced",
        "charid",
        vec![
            PyRep::Integer(objective_id),
            PyRep::Integer(last_completed_task_id),
        ],
    );
}

/// DB-backed variant of [`emit_progression_advanced`] for `SystemActor` / dungeon death hooks.
pub fn emit_progression_advanced_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&RwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    objective_id: i64,
    last_completed_task_id: i64,
) {
    push_char_notification_with_db(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnProgressionAdvanced",
        vec![
            PyRep::Integer(objective_id),
            PyRep::Integer(last_completed_task_id),
        ],
    );
}

/// Emits `OnProgressionTaskCompleted(task_id)` (`id_type = "charid"`).
/// Matches `progression/client/progressionSvc.pyj:ProgressionService.OnProgressionTaskCompleted`.
#[allow(dead_code)]
pub fn emit_progression_task_completed(ctx: &ServerContext, char_id: i64, task_id: i64) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnProgressionTaskCompleted",
        "charid",
        vec![PyRep::Integer(task_id)],
    );
}

/// DB-backed variant of [`emit_progression_task_completed`] for `SystemActor` / dungeon death hooks.
pub fn emit_progression_task_completed_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&RwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    task_id: i64,
) {
    push_char_notification_with_db(
        db,
        pending_notifications,
        notify_tx,
        char_id,
        "OnProgressionTaskCompleted",
        vec![PyRep::Integer(task_id)],
    );
}

/// Emits `OnCombatMessage(messageName, messageArgs)` (`id_type = "charid"`).
/// Matches `eve/client/script/parklife/tactical.pyj:TacticalSvc.OnCombatMessage(self, messageName, messageArgs)`
/// where `messageArgs` contains `"specialObject"`, `"damage"`, `"type"`, `"weapon"`, `"hitQualityText"`, and `"isWrecking"`.
pub fn emit_combat_message(
    ctx: &ServerContext,
    char_id: i64,
    message_name: &str,
    special_object_id: i64,
    damage: f64,
    weapon_type_id: u32,
    is_wrecking: bool,
) {
    let hit_quality_text = if damage <= 0.0 {
        "misses"
    } else if is_wrecking {
        "wrecks"
    } else {
        "hits"
    };
    let message_args = PyRep::Dict(vec![
        (
            PyRep::String("specialObject".into()),
            PyRep::Integer(special_object_id),
        ),
        (PyRep::String("damage".into()), PyRep::Real(damage)),
        (
            PyRep::String("type".into()),
            PyRep::Integer(i64::from(weapon_type_id)),
        ),
        (
            PyRep::String("weapon".into()),
            PyRep::Integer(i64::from(weapon_type_id)),
        ),
        (
            PyRep::String("hitQualityText".into()),
            PyRep::String(hit_quality_text.into()),
        ),
        (PyRep::String("isWrecking".into()), PyRep::Bool(is_wrecking)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCombatMessage",
        "charid",
        vec![PyRep::String(message_name.into()), message_args],
    );
}

/// Emits `OnCharNowInStation` or `OnCharNoLongerInStation` (`id_type = "charid"`)
/// to all online pilots currently docked in `station_id`.
pub fn emit_char_station_notification(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    is_docked: bool,
) {
    let (corp_id, alliance_id, war_faction_id) = ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&char_id);
        (
            c.map(|c| c.corporation_id).unwrap_or(0),
            c.and_then(|c| c.alliance_id),
            c.and_then(|c| c.war_faction_id),
        )
    });
    let guest_row = PyRep::List(vec![
        PyRep::Integer(char_id),
        PyRep::Integer(corp_id),
        alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        war_faction_id.map(PyRep::Integer).unwrap_or(PyRep::None),
    ]);

    let event = if is_docked {
        "OnCharNowInStation"
    } else {
        "OnCharNoLongerInStation"
    };

    ctx.notify(
        NotificationTarget::Station(station_id),
        event,
        "charid",
        vec![guest_row],
    );
}

/// Emits `OnCargoScanComplete(targetID, cargoList)` (`id_type = "charid"`).
/// `cargoList` consists of tuples `(typeID, quantity)`.
pub fn emit_cargo_scan_complete(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    items: Vec<(i32, i64)>,
) {
    let cargo_list = items
        .into_iter()
        .map(|(tid, qty)| {
            PyRep::Tuple(vec![
                PyRep::Integer(i64::from(tid)),
                PyRep::Integer(qty),
            ])
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCargoScanComplete",
        "charid",
        vec![PyRep::Integer(target_id), PyRep::List(cargo_list)],
    );
}

/// Emits `OnShipScanCompleted(targetID, capCharge, capCapacity, hardwareList)` (`id_type = "charid"`).
/// `hardwareList` consists of tuples `(typeID, quantity)`.
pub fn emit_ship_scan_completed(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    cap_charge: f64,
    max_cap: f64,
    hardware: Vec<(i32, i64)>,
) {
    let hw_list = hardware
        .into_iter()
        .map(|(tid, qty)| {
            PyRep::Tuple(vec![
                PyRep::Integer(i64::from(tid)),
                PyRep::Integer(qty),
            ])
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnShipScanCompleted",
        "charid",
        vec![
            PyRep::Integer(target_id),
            PyRep::Real(cap_charge),
            PyRep::Real(max_cap),
            PyRep::List(hw_list),
        ],
    );
}

/// Emits `OnOreMined(dataDict)` matching `OreMinedNotificationAdapter.OnOreMined(dataDict)`.
#[allow(clippy::too_many_arguments)]
pub fn emit_ore_mined(
    ctx: &ServerContext,
    char_id: i64,
    ore_type_id: i32,
    delivered: i64,
    mined: i64,
    wasted: i64,
    crit_bonus: i64,
    target_id: i64,
    module_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOreMined",
        "charid",
        vec![PyRep::Dict(vec![
            (PyRep::String("oreType".into()), PyRep::Integer(i64::from(ore_type_id))),
            (PyRep::String("quantity_added".into()), PyRep::Integer(delivered)),
            (PyRep::String("quantity_removed".into()), PyRep::Integer(mined)),
            (PyRep::String("hasRewards".into()), PyRep::Bool(false)),
            (PyRep::String("amountWasted".into()), PyRep::Integer(wasted)),
            (PyRep::String("amountCritBonus".into()), PyRep::Integer(crit_bonus)),
            (PyRep::String("itemID".into()), PyRep::Integer(target_id)),
            (PyRep::String("moduleItemID".into()), PyRep::Integer(module_id)),
        ])],
    );
}

/// Emits `OnDamageStateChange(itemID, ((shield, tau, ft), armor, struct))` (`id_type = "charid"`).
pub fn emit_damage_state_change(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    shield: f64,
    armor: f64,
    structure: f64,
) {
    let damage_tuple = super::notify_emitters_space_dungeons::build_damage_state_tuple(
        ctx, ship_id, shield, armor, structure,
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDamageStateChange",
        "charid",
        vec![PyRep::Integer(ship_id), damage_tuple],
    );
}

/// Emits `OnSystemESSDataChanged(systemID, data)` (`id_type = "solarsystemid"`).
pub fn emit_system_ess_data_changed(ctx: &ServerContext, system_id: i64, data: PyRep) {
    ctx.notify(
        NotificationTarget::SolarSystem(system_id as u32),
        "OnSystemESSDataChanged",
        "solarsystemid",
        vec![PyRep::Integer(system_id), data],
    );
}

/// Emits `OnSystemDynamicBountyStateChanged(systemID, rate)` (`id_type = "solarsystemid"`).
pub fn emit_system_dynamic_bounty_state_changed(
    ctx: &ServerContext,
    system_id: i64,
    rate: f64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(system_id as u32),
        "OnSystemDynamicBountyStateChanged",
        "solarsystemid",
        vec![PyRep::Integer(system_id), PyRep::Real(rate)],
    );
}

/// Emits `OnESSMainBankLinkNotification(solarSystemID)` (`id_type = "solarsystemid"`).
pub fn emit_ess_main_bank_link_notification(ctx: &ServerContext, system_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(system_id),
        "OnESSMainBankLinkNotification",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(system_id))],
    );
}

/// Emits `OnESSReserveBankLinkNotification(solar_system_id)` (`id_type = "solarsystemid"`).
pub fn emit_ess_reserve_bank_link_notification(ctx: &ServerContext, system_id: u32) {
    ctx.notify(
        NotificationTarget::SolarSystem(system_id),
        "OnESSReserveBankLinkNotification",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(system_id))],
    );
}

/// Emits `OnMainBankPlayerLinkDisconnected(data)` (`id_type = "charid"`).
pub fn emit_main_bank_player_link_disconnected(
    ctx: &ServerContext,
    char_id: i64,
    system_id: u32,
    reason: &str,
    link_id: i64,
) {
    let payload = PyRep::keyval([
        ("characterID", PyRep::Integer(char_id)),
        ("solarSystemID", PyRep::Integer(i64::from(system_id))),
        ("reason", PyRep::String(reason.to_string())),
        ("linkID", PyRep::Integer(link_id)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMainBankPlayerLinkDisconnected",
        "charid",
        vec![payload],
    );
}

/// Emits `OnReserveBankPlayerLinkDisconnected(data)` (`id_type = "charid"`).
pub fn emit_reserve_bank_player_link_disconnected(
    ctx: &ServerContext,
    char_id: i64,
    system_id: u32,
    reason: i64,
) {
    let payload = PyRep::keyval([
        ("characterID", PyRep::Integer(char_id)),
        ("solarSystemID", PyRep::Integer(i64::from(system_id))),
        ("reason", PyRep::Integer(reason)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnReserveBankPlayerLinkDisconnected",
        "charid",
        vec![payload],
    );
}

/// Emits `OnMainBankPlayerHackSuccess(data)` (`id_type = "charid"`).
pub fn emit_main_bank_player_hack_success(
    ctx: &ServerContext,
    char_id: i64,
    system_id: u32,
    link_id: i64,
) {
    let payload = PyRep::keyval([
        ("characterID", PyRep::Integer(char_id)),
        ("solarSystemID", PyRep::Integer(i64::from(system_id))),
        ("linkID", PyRep::Integer(link_id)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMainBankPlayerHackSuccess",
        "charid",
        vec![payload],
    );
}

/// Emits `OnReserveBankUnlock(data)` (`id_type = "solarsystemid"`).
pub fn emit_reserve_bank_unlock(ctx: &ServerContext, sys_id: u32, region_id: i64) {
    let payload = PyRep::keyval([("regionID", PyRep::Integer(region_id))]);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnReserveBankUnlock",
        "solarsystemid",
        vec![payload],
    );
}

/// Emits `OnKillNotification()` (`id_type = "charid"`).
pub fn emit_kill_notification(ctx: &ServerContext, char_id: i64) {
    if char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnKillNotification",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnShipDeath()` (`id_type = "charid"`).
pub fn emit_ship_death(ctx: &ServerContext, char_id: i64) {
    if char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnShipDeath",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnPodDeath()` (`id_type = "charid"`).
pub fn emit_pod_death(ctx: &ServerContext, char_id: i64) {
    if char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnPodDeath",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnPodKilled(destroyedPodID)` (`id_type = "charid"`).
pub fn emit_pod_killed(ctx: &ServerContext, char_id: i64, destroyed_pod_id: i64) {
    if char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnPodKilled",
            "charid",
            vec![PyRep::Integer(destroyed_pod_id)],
        );
    }
}

/// Emits `OnPlayerPodDeath()` (`id_type = "charid"`).
pub fn emit_player_pod_death(ctx: &ServerContext, char_id: i64) {
    if char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnPlayerPodDeath",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnPlayerPodRespawn()` (`id_type = "charid"`).
pub fn emit_player_pod_respawn(ctx: &ServerContext, char_id: i64) {
    if char_id > 0 {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnPlayerPodRespawn",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnSystemCriminalFlagUpdates(newIdles, newSuspects, newCriminals)` (`id_type = "solarsystemid"`).
pub fn emit_system_criminal_flag_updates(
    ctx: &ServerContext,
    sys_id: u32,
    new_idles: &[i64],
    new_suspects: &[i64],
    new_criminals: &[i64],
) {
    let ids = |v: &[i64]| PyRep::List(v.iter().map(|c| PyRep::Integer(*c)).collect());
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSystemCriminalFlagUpdates",
        "solarsystemid",
        vec![ids(new_idles), ids(new_suspects), ids(new_criminals)],
    );
}

/// Emits `OnSystemDisapprovalFlagUpdates(newIdles, newNaughyBoysAndGirls)` (`id_type = "solarsystemid"`).
pub fn emit_system_disapproval_flag_updates(
    ctx: &ServerContext,
    sys_id: u32,
    new_idles: &[i64],
    new_naughty: &[i64],
) {
    let ids = |v: &[i64]| PyRep::List(v.iter().map(|c| PyRep::Integer(*c)).collect());
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSystemDisapprovalFlagUpdates",
        "solarsystemid",
        vec![ids(new_idles), ids(new_naughty)],
    );
}

/// Emits `OnSecurityModified(solarSystemID, modifierAmount, newSecurity)` (`id_type = "solarsystemid"`).
pub fn emit_security_modified(
    ctx: &ServerContext,
    sys_id: u32,
    modifier_amount: f64,
    new_security: f64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSecurityModified",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Real(modifier_amount),
            PyRep::Real(new_security),
        ],
    );
}

#[cfg(test)]
#[path = "notify_emitters_combat_ess_progression_tests.rs"]
mod notify_emitters_combat_ess_progression_tests;

