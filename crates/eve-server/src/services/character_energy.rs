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

/// Maximum Complex Encryption Qubit (CEQ) pool cap (`100` CEQ, enough for 3 CRAB activations at 30 CEQ each).
pub const DEFAULT_MAX_ENERGY: i64 = 100;

/// Default CEQ regeneration rate: `100 / 1320` CEQ per minute (`~4.545` CEQ / hour, regenerating `0 -> 100` in 22 hours).
pub const DEFAULT_RECHARGE_RATE: f64 = 100.0 / 1_320.0;

/// Canonical character energy state representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterEnergyState {
    pub current_energy: i64,
    pub max_energy: i64,
    pub last_update_time: i64, // Windows FILETIME ticks (100ns intervals since 1601)
    pub recharge_rate: f64,    // CEQ regenerated per minute
    #[serde(default)]
    pub energy_rem: f64,
}

impl Default for CharacterEnergyState {
    fn default() -> Self {
        Self {
            current_energy: DEFAULT_MAX_ENERGY,
            max_energy: DEFAULT_MAX_ENERGY,
            last_update_time: 0,
            recharge_rate: DEFAULT_RECHARGE_RATE,
            energy_rem: 0.0,
        }
    }
}

/// Converts a Unix timestamp in seconds to Windows FILETIME ticks.
fn unix_to_filetime(u: u64) -> i64 {
    (u as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

/// Converts a `CharacterEnergyState` into a client-compatible `PyRep::KeyVal`.
pub fn energy_to_keyval(state: &CharacterEnergyState) -> PyRep {
    let hourly_gain = state.recharge_rate * 60.0;
    PyRep::keyval([
        ("currentEnergy", PyRep::Integer(state.current_energy)),
        ("maxEnergy", PyRep::Integer(state.max_energy)),
        ("lastUpdateTime", PyRep::Integer(state.last_update_time)),
        ("rechargeRate", PyRep::Real(state.recharge_rate)),
        ("energyLevel", PyRep::Integer(state.current_energy)),
        ("lastRechargeTickTimestamp", PyRep::Integer(state.last_update_time)),
        ("energyIncreasePerRechargeTick", PyRep::Real(hourly_gain)),
        ("rechargeTickPeriod", PyRep::Integer(3_600)),
        ("minEnergyLevel", PyRep::Integer(0)),
        ("quiescentEnergyLevel", PyRep::Integer(state.max_energy)),
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
            energy_rem: 0.0,
        });

        let mut changed = !had_entry;
        let mut regenerated = false;
        if entry.max_energy == 5_000 && entry.current_energy == 5_000 {
            entry.max_energy = DEFAULT_MAX_ENERGY;
            entry.current_energy = DEFAULT_MAX_ENERGY;
            entry.recharge_rate = DEFAULT_RECHARGE_RATE;
            entry.energy_rem = 0.0;
            changed = true;
        }
        if entry.current_energy >= entry.max_energy {
            entry.last_update_time = now_ft;
            entry.energy_rem = 0.0;
        } else if entry.last_update_time > 0 && now_ft > entry.last_update_time && entry.recharge_rate > 0.0 {
            let elapsed_minutes = ((now_ft - entry.last_update_time) as f64) / 600_000_000.0;
            let total_gain = entry.energy_rem + elapsed_minutes * entry.recharge_rate;
            let gained = total_gain.floor() as i64;
            if gained > 0 || elapsed_minutes >= 1.0 {
                let prev = entry.current_energy;
                entry.current_energy = (entry.current_energy + gained).min(entry.max_energy);
                entry.energy_rem = if entry.current_energy >= entry.max_energy {
                    0.0
                } else {
                    (total_gain - (gained as f64)).max(0.0)
                };
                entry.last_update_time = now_ft;
                changed = true;
                regenerated = entry.current_energy != prev;
            }
        } else if entry.last_update_time <= 0 {
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
            energy_rem: 0.0,
        });
        if entry.max_energy == 5_000 {
            entry.max_energy = DEFAULT_MAX_ENERGY;
            entry.recharge_rate = DEFAULT_RECHARGE_RATE;
        }
        entry.current_energy = new_energy.clamp(0, entry.max_energy);
        entry.energy_rem = 0.0;
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

/// Regenerates CEQ up to `now` and deducts `amount` CEQ, returning `InsufficientEnergy` if insufficient.
pub fn consume_character_energy(
    ctx: &ServerContext,
    char_id: i64,
    amount: i64,
) -> Result<CharacterEnergyState, BridgeRpcError> {
    let cur = get_character_energy(ctx, char_id);
    if amount <= 0 {
        return Ok(cur);
    }
    if cur.current_energy < amount {
        return Err(BridgeRpcError::user_error("InsufficientEnergy"));
    }
    Ok(set_character_energy(ctx, char_id, cur.current_energy - amount))
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
