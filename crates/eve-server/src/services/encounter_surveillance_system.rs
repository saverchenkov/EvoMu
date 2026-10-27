//! `encounter_surveillance_system` — Encounter Surveillance System (`essMgr`) cache priming and handlers.

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, SafetyLevel,
};
use crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications;
use crate::services::space_destiny::get_ess_reserve_pulses;
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

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

/// Handles `essMgr.AttemptLinkToMainBank` and `essMgr.AttemptLinkToReserveBank`.
// parity: feat:crimewatch.suspect-timer-looting-ess, feat:crimewatch.safety-setting-action-block
pub async fn handle_ess_link(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot link to ESS while docked".into(),
        });
    }
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        });
    };
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    if !(sys_sec <= 0.0 && sys_id < 31_000_000) {
        return Err(BridgeRpcError {
            code: "EssOnlyInNullSec".into(),
            message: format!(
                "ESS banks only exist in K-space null-sec systems (system {sys_id} has security {sys_sec:.2})"
            ),
        });
    }

    let safety = with_combat_runtime(ctx, |rt| {
        rt.safety_levels
            .get(&char_id)
            .copied()
            .unwrap_or(SafetyLevel::Red)
    });
    if safety == SafetyLevel::Green {
        return Err(BridgeRpcError {
            code: "SafetyLevelRestricted".into(),
            message: "UserError: CantDoInCurrentSafetyLevel - Safety Green blocks linking to an ESS bank because it triggers a Suspect Timer".into(),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((current_tick, ship_ball)) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
    else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not in space".into(),
        });
    };

    let Some(ess_ball_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "EssBeaconNotPresent".into(),
            message: "ESS link requires an explicit integer essBallID".into(),
        });
    };
    let Some((_, ess_ball)) = sys_handle
        .get_ball_snapshot(ess_ball_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
    else {
        return Err(BridgeRpcError {
            code: "EssBeaconNotPresent".into(),
            message: format!("No ESS beacon present in solar system {sys_id}"),
        });
    };
    if ess_ball.type_id != 32882 {
        return Err(BridgeRpcError {
            code: "EssBeaconNotPresent".into(),
            message: format!("Ball {ess_ball_id} is not an Encounter Surveillance System (32882)"),
        });
    }

    let dist = ship_ball.position.distance(ess_ball.position);
    if dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutsideEssLinkRange".into(),
            message: format!("Ship is {:.1} m from ESS beacon (must be <= 2500 m)", dist),
        });
    }

    let now_unix = ctx.now_unix();
    let is_unlocked = with_combat_runtime(ctx, |rt| {
        rt.ess_reserve_unlocked_until.get(&sys_id).copied().unwrap_or(0) > now_unix
    });

    let (payout, link_started_only, unlocked_until_new) = ctx.db.try_with_mut_snapshot(|snap| {
        let is_reserve = method == "AttemptLinkToReserveBank";
        let mut unlocked_until_new: Option<u64> = None;
        let mut pulse_st = crate::services::space_destiny::get_ess_reserve_pulses_from_blobs(
            &snap.service_state_blobs,
            sys_id,
        );
        if is_reserve {
            if let Some(other) = pulse_st.reserve_linked_character_id {
                if other != char_id {
                    return Err(BridgeRpcError {
                        code: "EssReserveAlreadyLinked".into(),
                        message: format!("ESS Reserve Bank in system {sys_id} is already linked by character {other}"),
                    });
                }
            }
        } else if let Some(st) = snap.system_states.get(&sys_id) {
            if let Some(other) = st.ess_linked_character_id {
                if other != char_id && pulse_st.reserve_linked_character_id != Some(other) {
                    return Err(BridgeRpcError {
                        code: "EssAlreadyLinked".into(),
                        message: format!("ESS Main Bank in system {sys_id} is already linked by character {other}"),
                    });
                }
            }
        }
        let already_linked = if is_reserve {
            pulse_st.reserve_linked_character_id == Some(char_id)
        } else {
            snap.system_states
                .get(&sys_id)
                .map(|st| {
                    st.ess_linked_character_id == Some(char_id)
                        && pulse_st.reserve_linked_character_id != Some(char_id)
                })
                .unwrap_or(false)
        };
        if is_reserve && !already_linked && !is_unlocked {
            let key_info = snap
                .items
                .values()
                .find(|i| {
                    i.owner_id == char_id
                        && i.location_id == ship_id
                        && i.flag == InventoryFlag::Cargo
                        && i.quantity >= 1
                        && ((57714..=57721).contains(&i.type_id)
                            || ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                .map(|t| t.group_id == 4102)
                                .unwrap_or(false))
                })
                .map(|i| (i.item_id, i.type_id));
            let Some((key_id, key_type_id)) = key_info else {
                return Err(BridgeRpcError {
                    code: "MissingEssReserveSecurityKey".into(),
                    message: "Linking to the ESS Reserve Bank requires an ESS Reserve Security Key (type 57714..57721) in active ship cargo".into(),
                });
            };
            if let Some(key_item) = snap.items.get_mut(&key_id) {
                if key_item.quantity > 1 {
                    key_item.quantity -= 1;
                    key_item.version += 1;
                } else {
                    snap.items.remove(&key_id);
                }
            }
            let dur_mins: u64 = if matches!(key_type_id, 57715 | 57717 | 57719 | 57721) {
                45
            } else {
                15
            };
            unlocked_until_new = Some(now_unix + dur_mins * 60);
            if pulse_st.pulses_remaining == 0 {
                pulse_st.pulses_remaining = dur_mins as u32;
            }
        }
        let Some(st) = snap.system_states.get_mut(&sys_id) else {
            return Err(BridgeRpcError {
                code: "SystemStateNotFound".into(),
                message: format!("No ESS state for solar system {sys_id}"),
            });
        };
        let bank_available = if is_reserve {
            st.ess_reserve_bank.0
        } else {
            st.ess_main_bank.0
        };
        if bank_available <= 0 {
            return Err(BridgeRpcError {
                code: "EssBankEmpty".into(),
                message: format!("ESS Bank in system {sys_id} is empty"),
            });
        }

        if !already_linked {
            if is_reserve {
                pulse_st.reserve_linked_character_id = Some(char_id);
                pulse_st.reserve_link_started_tick = Some(current_tick);
                crate::services::space_destiny::set_ess_reserve_pulses_in_blobs(
                    &mut snap.service_state_blobs,
                    sys_id,
                    &pulse_st,
                );
                if st.ess_linked_character_id.is_none() {
                    st.ess_linked_character_id = Some(char_id);
                    st.ess_link_started_tick = Some(current_tick);
                }
            } else {
                if pulse_st.reserve_linked_character_id == Some(char_id) {
                    pulse_st.reserve_linked_character_id = None;
                    pulse_st.reserve_link_started_tick = None;
                    crate::services::space_destiny::set_ess_reserve_pulses_in_blobs(
                        &mut snap.service_state_blobs,
                        sys_id,
                        &pulse_st,
                    );
                }
                st.ess_linked_character_id = Some(char_id);
                st.ess_link_started_tick = Some(current_tick);
            }
            return Ok((0.0, true, unlocked_until_new));
        }
        let start_tick = if is_reserve {
            pulse_st.reserve_link_started_tick.unwrap_or(current_tick)
        } else {
            st.ess_link_started_tick.unwrap_or(current_tick)
        };
        if current_tick < start_tick.saturating_add(5) {
            return Err(BridgeRpcError {
                code: "EssLinkTimerNotElapsed".into(),
                message: format!(
                    "ESS link requires 5 ticks to complete (started at {}, current {})",
                    start_tick, current_tick
                ),
            });
        }
        let bank = if is_reserve {
            pulse_st.reserve_linked_character_id = None;
            pulse_st.reserve_link_started_tick = None;
            if st.ess_linked_character_id == Some(char_id) {
                st.ess_linked_character_id = None;
                st.ess_link_started_tick = None;
            }
            if pulse_st.pulses_remaining > 0 {
                let rem = pulse_st.pulses_remaining.max(1) as i64;
                let p = (st.ess_reserve_bank.0 / rem).max(1).min(st.ess_reserve_bank.0);
                st.ess_reserve_bank = IskCents(st.ess_reserve_bank.0.saturating_sub(p));
                pulse_st.pulses_remaining = pulse_st.pulses_remaining.saturating_sub(1);
                pulse_st.last_pulse_sec = Some(ctx.now_unix());
                crate::services::space_destiny::set_ess_reserve_pulses_in_blobs(
                    &mut snap.service_state_blobs,
                    sys_id,
                    &pulse_st,
                );
                IskCents(p)
            } else {
                crate::services::space_destiny::set_ess_reserve_pulses_in_blobs(
                    &mut snap.service_state_blobs,
                    sys_id,
                    &pulse_st,
                );
                let b = st.ess_reserve_bank;
                st.ess_reserve_bank = IskCents(0);
                b
            }
        } else {
            st.ess_linked_character_id = None;
            st.ess_link_started_tick = None;
            let b = st.ess_main_bank;
            st.ess_main_bank = IskCents(0);
            b
        };

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = ch.balance.checked_add(bank).unwrap_or(ch.balance);
        }
        Ok((bank.to_wire_f64(), false, unlocked_until_new))
    })?;

    if let Some(until) = unlocked_until_new {
        with_mut_combat_runtime(ctx, |rt| {
            rt.ess_reserve_unlocked_until.insert(sys_id, until);
        });
    }

    // Linking to an ESS Main/Reserve Bank activates a 15-minute Suspect Timer (`feat:crimewatch.suspect-timer-looting-ess`)
    let suspect_until = now_unix.saturating_add(900);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(char_id);
        st.suspect_flag_until_unix = suspect_until;
    }
    let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[char_id], &[]);
    emit_crimewatch_timer_notifications(ctx, char_id, now_unix);

    if payout > 0.0 {
        let now_u = ctx.now_unix();
        with_mut_combat_runtime(ctx, |rt| {
            let list = if method == "AttemptLinkToReserveBank" {
                rt.ess_reserve_bank_thefts.entry(sys_id).or_default()
            } else {
                rt.ess_main_bank_thefts.entry(sys_id).or_default()
            };
            list.push((char_id, payout, now_u));
        });
    }
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("linked", PyRep::Bool(true)),
        ("linkStartedOnly", PyRep::Bool(link_started_only)),
        ("stolenISK", PyRep::Real(payout)),
        ("suspectFlagged", PyRep::Bool(true)),
        ("suspectTimerUntilUnix", PyRep::Integer(suspect_until as i64)),
    ]))
}
