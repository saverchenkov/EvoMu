//! Character Energy Manager Service (`characterEnergyMgr`)
//!
//! Provides character Complex Encryption Qubit (`CEQ`) energy tracking and regeneration
//! used by V24 live-server features including CRAB beacons and Mobile Phase Anchors.
//!
//! # Parity & Client Protocol
//! - `characterEnergyMgr.GetMyEnergyState` (`rpc:characterEnergyMgr.GetMyEnergyState`):
//!   returns `PyRep::KeyVal` containing `currentEnergy`, `maxEnergy`, `lastUpdateTime`, `rechargeRate`.
//! - `OnCharacterEnergyChanged` (`notify:OnCharacterEnergyChanged`):
//!   emits `(characterID, newEnergyState)` to the character on energy state transitions.

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};

/// Maximum Complex Encryption Qubit (CEQ) pool cap.
pub const DEFAULT_MAX_ENERGY: i64 = 5_000;

/// Default CEQ regeneration rate: 3.0 CEQ per minute (180 CEQ / hour).
pub const DEFAULT_RECHARGE_RATE: f64 = 3.0;

/// Canonical character energy state representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterEnergyState {
    pub current_energy: i64,
    pub max_energy: i64,
    pub last_update_time: i64, // Windows FILETIME ticks (100ns intervals since 1601)
    pub recharge_rate: f64,    // CEQ regenerated per minute
}

impl Default for CharacterEnergyState {
    fn default() -> Self {
        Self {
            current_energy: DEFAULT_MAX_ENERGY,
            max_energy: DEFAULT_MAX_ENERGY,
            last_update_time: 0,
            recharge_rate: DEFAULT_RECHARGE_RATE,
        }
    }
}

/// Converts a Unix timestamp in seconds to Windows FILETIME ticks.
fn unix_to_filetime(u: u64) -> i64 {
    (u as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

/// Converts a `CharacterEnergyState` into a client-compatible `PyRep::KeyVal`.
pub fn energy_to_keyval(state: &CharacterEnergyState) -> PyRep {
    PyRep::keyval([
        ("currentEnergy", PyRep::Integer(state.current_energy)),
        ("maxEnergy", PyRep::Integer(state.max_energy)),
        ("lastUpdateTime", PyRep::Integer(state.last_update_time)),
        ("rechargeRate", PyRep::Real(state.recharge_rate)),
    ])
}

/// Retrieves or initializes the character's energy state, applying real-time regeneration.
pub fn get_character_energy(ctx: &ServerContext, char_id: i64) -> CharacterEnergyState {
    let now_ft = unix_to_filetime(ctx.now_unix());
    let (entry_clone, dirty, regenerated) = {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let had_entry = g.char_energy.contains_key(&char_id);
        let entry = g.char_energy.entry(char_id).or_insert_with(|| CharacterEnergyState {
            current_energy: DEFAULT_MAX_ENERGY,
            max_energy: DEFAULT_MAX_ENERGY,
            last_update_time: now_ft,
            recharge_rate: DEFAULT_RECHARGE_RATE,
        });

        let mut changed = !had_entry;
        let mut regenerated = false;
        if entry.last_update_time > 0 && entry.recharge_rate > 0.0 {
            let elapsed_minutes = ((now_ft - entry.last_update_time).max(0) as f64) / 600_000_000.0;
            let gained = (elapsed_minutes * entry.recharge_rate).floor() as i64;
            if gained > 0 {
                let prev = entry.current_energy;
                entry.current_energy = (entry.current_energy + gained).min(entry.max_energy);
                entry.last_update_time = now_ft;
                changed = true;
                regenerated = entry.current_energy != prev;
            }
        } else {
            entry.last_update_time = now_ft;
        }

        (entry.clone(), changed, regenerated)
    };

    if dirty {
        crate::services::cluster_macho_misc_services::persist_misc_state(ctx);
    }
    if regenerated {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnCharacterEnergyChanged",
            "charid",
            vec![PyRep::Integer(char_id), energy_to_keyval(&entry_clone)],
        );
    }
    entry_clone
}

/// Sets or mutates the character's energy pool, emitting `OnCharacterEnergyChanged`.
pub fn set_character_energy(
    ctx: &ServerContext,
    char_id: i64,
    new_energy: i64,
) -> CharacterEnergyState {
    let now_ft = unix_to_filetime(ctx.now_unix());
    let updated_state = {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.entry(char_id).or_insert_with(|| CharacterEnergyState {
            current_energy: DEFAULT_MAX_ENERGY,
            max_energy: DEFAULT_MAX_ENERGY,
            last_update_time: now_ft,
            recharge_rate: DEFAULT_RECHARGE_RATE,
        });
        entry.current_energy = new_energy.clamp(0, entry.max_energy);
        entry.last_update_time = now_ft;
        entry.clone()
    };

    crate::services::cluster_macho_misc_services::persist_misc_state(ctx);

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCharacterEnergyChanged",
        "charid",
        vec![
            PyRep::Integer(char_id),
            energy_to_keyval(&updated_state),
        ],
    );

    updated_state
}

/// Dispatches RPC calls for `characterEnergyMgr`.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "characterEnergyMgr" {
        return None;
    }

    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    match method {
        // parity: rpc:characterEnergyMgr.GetMyEnergyState
        "GetMyEnergyState" => {
            let state = get_character_energy(ctx, char_id);
            Some(Ok(energy_to_keyval(&state)))
        }
        _ => None,
    }
}
