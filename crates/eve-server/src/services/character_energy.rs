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
    let energy_range = state.max_energy.max(0);
    let normalised = if energy_range > 0 {
        ((state.current_energy as f64) / (energy_range as f64)).clamp(0.0, 1.0)
    } else {
        0.0
    };
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
        ("isAtQuiescentLevel", PyRep::Bool(state.current_energy >= state.max_energy)),
        ("energyRange", PyRep::Integer(energy_range)),
        ("normalisedEnergyLevel", PyRep::Real(normalised)),
    ])
}

fn default_energy_state(now_ft: i64) -> CharacterEnergyState {
    CharacterEnergyState {
        current_energy: DEFAULT_MAX_ENERGY,
        max_energy: DEFAULT_MAX_ENERGY,
        last_update_time: now_ft,
        recharge_rate: DEFAULT_RECHARGE_RATE,
        energy_rem: 0.0,
    }
}

/// Applies legacy max-energy migration and elapsed regeneration onto `entry` in place,
/// returning `(changed, regenerated)`.
fn compute_regenerated_energy(entry: &mut CharacterEnergyState, now_ft: i64) -> (bool, bool) {
    let mut changed = false;
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
    } else if entry.last_update_time > 0
        && now_ft > entry.last_update_time
        && entry.recharge_rate > 0.0
    {
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
    (changed, regenerated)
}

/// Reads and computes the character's current energy state without mutating state,
/// persisting to DB, or emitting notifications.
pub fn read_character_energy(ctx: &ServerContext, char_id: i64) -> CharacterEnergyState {
    let now_ft = unix_to_filetime(ctx.now_unix());
    let mut entry = {
        let g = ctx.cluster_misc_state.lock().unwrap();
        g.char_energy
            .get(&char_id)
            .cloned()
            .unwrap_or_else(|| default_energy_state(now_ft))
    };
    let _ = compute_regenerated_energy(&mut entry, now_ft);
    entry
}

/// Retrieves or initializes the character's energy state, applying real-time regeneration.
pub fn get_character_energy(ctx: &ServerContext, char_id: i64) -> CharacterEnergyState {
    let now_ft = unix_to_filetime(ctx.now_unix());
    let (entry_clone, dirty, regenerated) = {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let had_entry = g.char_energy.contains_key(&char_id);
        let entry = g
            .char_energy
            .entry(char_id)
            .or_insert_with(|| default_energy_state(now_ft));
        let (comp_changed, regenerated) = compute_regenerated_energy(entry, now_ft);
        (entry.clone(), !had_entry || comp_changed, regenerated)
    };

    if dirty {
        crate::services::cluster_macho_misc_services::persist_misc_state(ctx);
    }
    if regenerated {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnCharacterEnergyChanged {
                character_id: char_id,
                new_energy_state: energy_to_keyval(&entry_clone),
            },
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

    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnCharacterEnergyChanged {
            character_id: char_id,
            new_energy_state: energy_to_keyval(&updated_state),
        },
    );

    updated_state
}

/// Regenerates CEQ up to `now` and atomically deducts `amount` CEQ, returning
/// `InsufficientEnergy` without mutating state or emitting notifications when insufficient.
pub fn consume_character_energy(
    ctx: &ServerContext,
    char_id: i64,
    amount: i64,
) -> Result<CharacterEnergyState, BridgeRpcError> {
    if amount <= 0 {
        return Ok(read_character_energy(ctx, char_id));
    }
    let now_ft = unix_to_filetime(ctx.now_unix());
    let updated_state = {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let mut candidate = g
            .char_energy
            .get(&char_id)
            .cloned()
            .unwrap_or_else(|| default_energy_state(now_ft));
        let _ = compute_regenerated_energy(&mut candidate, now_ft);
        if candidate.current_energy < amount {
            return Err(BridgeRpcError::user_error("InsufficientEnergy"));
        }
        candidate.current_energy = (candidate.current_energy - amount).clamp(0, candidate.max_energy);
        candidate.energy_rem = 0.0;
        candidate.last_update_time = now_ft;
        g.char_energy.insert(char_id, candidate.clone());
        candidate
    };

    crate::services::cluster_macho_misc_services::persist_misc_state(ctx);

    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnCharacterEnergyChanged {
            character_id: char_id,
            new_energy_state: energy_to_keyval(&updated_state),
        },
    );

    Ok(updated_state)
}

/// Dispatches RPC calls for `characterEnergyMgr`.
use crate::router::rpc_registry::{rpc_entry, RpcDispatchHandler, RpcEntry};

pub const RPCS: &[RpcEntry] = &[rpc_entry(
    "characterEnergyMgr",
    "GetMyEnergyState",
    RpcDispatchHandler::SyncNoKwargs(handle),
)];

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
            let state = read_character_energy(ctx, char_id);
            Some(Ok(energy_to_keyval(&state)))
        }
        _ => None,
    }
}

#[cfg(test)] #[path = "character_energy_tests.rs"] mod character_energy_tests;

#[cfg(test)]
#[path = "character_energy_edges_tests.rs"]
mod character_energy_edges_tests;

