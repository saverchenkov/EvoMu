//! `notify_emitters_batch6` — Server-side V24 notification emitters for Batch 6
//! (ESS banks, dynamic bounty, death/pod pipelines, and crimewatch system broadcasts).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

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
