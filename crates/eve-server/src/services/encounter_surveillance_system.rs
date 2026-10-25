//! `encounter_surveillance_system` — Encounter Surveillance System (`essMgr`) cache priming and handlers.

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use crate::services::space_destiny::get_ess_reserve_pulses;

/// Canonical cache priming structure for system ESS reserve and main banks.
///
/// Returns the expected cache priming structure for system ESS reserve and main banks
/// as consumed by `eve/client/script/ui/shared/dynamicResource/dynamicResourceSvc.pyj`.
///
/// parity: rpc:essMgr.prime_cache_result
pub fn prime_cache_result(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;

    // Accept optional solarSystemID in args[0], defaulting to session.solar_system_id
    let sys_id = args
        .first()
        .and_then(|a| match a {
            PyRep::Integer(v) => u32::try_from(*v).ok(),
            _ => None,
        })
        .or(session.solar_system_id);

    let Some(sys_id) = sys_id else {
        return Ok(PyRep::None);
    };

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    if !(sys_sec <= 0.0 && sys_id < 31_000_000) {
        return Ok(PyRep::None);
    }

    let rep = ctx.db.with_snapshot(|snap| {
        let Some(st) = snap.system_states.get(&sys_id) else {
            return PyRep::None;
        };
        let pulse_st = get_ess_reserve_pulses(snap, sys_id);
        let ess_id = 820_000_000 + sys_id as i64;

        let main_bank_link = if let Some(linked_char_id) = st.ess_linked_character_id {
            let started_tick = st.ess_link_started_tick.unwrap_or(0);
            PyRep::Dict(vec![
                (PyRep::String("characterID".into()), PyRep::Integer(linked_char_id)),
                (PyRep::String("linkID".into()), PyRep::Integer(1)),
                (
                    PyRep::String("startedAt".into()),
                    PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(started_tick)),
                ),
                (
                    PyRep::String("completesAt".into()),
                    PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(started_tick + 390)),
                ),
            ])
        } else {
            PyRep::None
        };

        PyRep::Dict(vec![
            (PyRep::String("solarSystemID".into()), PyRep::Integer(sys_id as i64)),
            (PyRep::String("essID".into()), PyRep::Integer(ess_id)),
            (PyRep::String("beaconID".into()), PyRep::Integer(ess_id)),
            (
                PyRep::String("mainValue".into()),
                PyRep::Real(st.ess_main_bank.to_wire_f64()),
            ),
            (
                PyRep::String("reserveValue".into()),
                PyRep::Real(st.ess_reserve_bank.to_wire_f64()),
            ),
            (PyRep::String("mainBankLink".into()), main_bank_link),
            (
                PyRep::String("reserveBankLastPulseInitiated".into()),
                pulse_st.last_pulse_sec
                    .map(|s| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(s)))
                    .unwrap_or(PyRep::None),
            ),
            (
                PyRep::String("reserveBankPulsesRemaining".into()),
                PyRep::Integer(pulse_st.pulses_remaining as i64),
            ),
            (
                PyRep::String("reserveBankPulsesTotal".into()),
                PyRep::Integer(pulse_st.pulses_total as i64),
            ),
            (
                PyRep::String("reserveBankActiveLinks".into()),
                PyRep::Integer(0),
            ),
        ])
    });

    Ok(rep)
}
