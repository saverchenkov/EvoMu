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

#[derive(Copy, Clone, PartialEq, Eq)]
enum EssQuadrant {
    Nw,
    Ne,
    Se,
    Sw,
}

fn system_ess_quadrant(sde: &eve_sde::SdeRegistry, sys_id: u32) -> EssQuadrant {
    let Some(sys) = sde.solar_systems.get(&sys_id) else {
        return EssQuadrant::Nw;
    };
    match sys.region_id {
        // SCC North-West Encounter Surveillance Quadrant (+ adjacent NPC null)
        10_000_003 | 10_000_010 | 10_000_023 | 10_000_034 | 10_000_035 | 10_000_045
        | 10_000_046 | 10_000_051 | 10_000_055 | 10_000_058 | 10_000_015 | 10_000_041
        | 10_000_057 => EssQuadrant::Nw,
        // SCC North-East Encounter Surveillance Quadrant
        10_000_007 | 10_000_013 | 10_000_018 | 10_000_021 | 10_000_027 | 10_000_040
        | 10_000_053 | 10_000_066 => EssQuadrant::Ne,
        // SCC South-East Encounter Surveillance Quadrant (+ adjacent NPC null)
        10_000_005 | 10_000_006 | 10_000_008 | 10_000_009 | 10_000_025 | 10_000_029
        | 10_000_061 | 10_000_062 | 10_000_011 | 10_000_012 => EssQuadrant::Se,
        // SCC South-West Encounter Surveillance Quadrant (+ adjacent NPC null)
        10_000_014 | 10_000_031 | 10_000_039 | 10_000_047 | 10_000_050 | 10_000_056
        | 10_000_059 | 10_000_060 | 10_000_063 | 10_000_022 => EssQuadrant::Sw,
        _ => match (sys.galactic_pos_ly[0] > 0.0, sys.galactic_pos_ly[2] >= 0.0) {
            (false, true) => EssQuadrant::Nw,
            (true, true) => EssQuadrant::Ne,
            (true, false) => EssQuadrant::Se,
            (false, false) => EssQuadrant::Sw,
        },
    }
}

fn ess_key_quadrant_and_duration(type_id: u32) -> Option<(EssQuadrant, u64)> {
    match type_id {
        57714 => Some((EssQuadrant::Nw, 15)),
        57715 => Some((EssQuadrant::Nw, 45)),
        57716 => Some((EssQuadrant::Ne, 15)),
        57719 => Some((EssQuadrant::Ne, 45)),
        57717 => Some((EssQuadrant::Se, 15)),
        57718 => Some((EssQuadrant::Se, 45)),
        57720 => Some((EssQuadrant::Sw, 15)),
        57721 => Some((EssQuadrant::Sw, 45)),
        _ => None,
    }
}

/// Validates and selects an ESS Reserve Security Key (`57714..=57721`, group `4102`) in `ship_id`
/// cargo matching `sys_id`'s SCC quadrant. Never accepts non-key cargo items even when `target_arg` is provided.
pub(crate) fn select_ess_reserve_key_in_cargo<'a>(
    sde: &eve_sde::SdeRegistry,
    items: impl Iterator<Item = &'a eve_db::InventoryItemRecord>,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    target_arg: Option<i64>,
) -> Result<(i64, u32, u64), BridgeRpcError> {
    let sys_quad = system_ess_quadrant(sde, sys_id);
    let mut saw_wrong_quadrant_key = false;
    for i in items {
        if i.owner_id != char_id
            || i.location_id != ship_id
            || i.flag != InventoryFlag::Cargo
            || i.quantity < 1
        {
            continue;
        }
        if let Some(target_iid) = target_arg {
            if i.item_id != target_iid && i.type_id as i64 != target_iid {
                continue;
            }
        }
        let is_sde_ess_key_group = sde
            .types
            .get(&i.type_id)
            .is_some_and(|t| t.group_id == 4102);
        let Some((key_quad, dur_mins)) = ess_key_quadrant_and_duration(i.type_id) else {
            let _ = is_sde_ess_key_group;
            continue;
        };
        if key_quad != sys_quad {
            saw_wrong_quadrant_key = true;
            continue;
        }
        return Ok((i.item_id, i.type_id, dur_mins));
    }
    if saw_wrong_quadrant_key {
        Err(BridgeRpcError {
            code: "EssReserveKeyWrongQuadrant".into(),
            message: format!(
                "ESS Reserve Security Key quadrant does not match solar system {sys_id}"
            ),
        })
    } else {
        Err(BridgeRpcError {
            code: "MissingEssReserveSecurityKey".into(),
            message: "Unlocking the ESS Reserve Bank requires a matching-quadrant ESS Reserve Security Key (type 57714..57721) in active ship cargo".into(),
        })
    }
}

/// Canonical cache priming structure for system ESS reserve and main banks.
///
/// Returns the expected cache priming structure for system ESS reserve and main banks
/// as consumed by `eve/client/script/ui/shared/dynamicResource/dynamicResourceSvc.pyj`.
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

    let _ = step_ess_main_bank_autopayout(ctx, sys_id, ctx.now_unix());
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
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
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
    let unlocked_until = with_combat_runtime(ctx, |rt| {
        rt.ess_reserve_unlocked_until.get(&sys_id).copied().unwrap_or(0)
    });
    let is_unlocked = unlocked_until > now_unix;

    let (payout, link_started_only, unlocked_until_new, clear_unlock) = ctx.db.try_with_mut_snapshot(|snap| {
        let is_reserve = method == "AttemptLinkToReserveBank";
        let mut unlocked_until_new: Option<u64> = None;
        let mut clear_unlock = false;
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
        if is_reserve && !already_linked {
            if is_unlocked {
                if pulse_st.pulses_total == 0 && pulse_st.last_pulse_sec.is_none() {
                    let mins = ((unlocked_until.saturating_sub(now_unix) + 59) / 60).max(1) as u32;
                    pulse_st.pulses_total = mins;
                    pulse_st.pulses_remaining = mins;
                }
                if pulse_st.pulses_remaining == 0 {
                    return Err(BridgeRpcError {
                        code: "EssReservePulsesExhausted".into(),
                        message: "All ESS Reserve Bank pulses for this key have been exhausted".into(),
                    });
                }
                if pulse_st.last_pulse_sec.is_some_and(|last| now_unix < last.saturating_add(60)) {
                    return Err(BridgeRpcError {
                        code: "EssReservePulseCooldown".into(),
                        message: "ESS Reserve Bank pulse is on 60s cooldown".into(),
                    });
                }
            } else {
                let (key_id, _key_type_id, dur_mins) = select_ess_reserve_key_in_cargo(
                    &ctx.sde,
                    snap.items.values(),
                    char_id,
                    ship_id,
                    sys_id,
                    None,
                )?;
                if let Some(key_item) = snap.items.get_mut(&key_id) {
                    if key_item.quantity > 1 {
                        key_item.quantity -= 1;
                        key_item.version += 1;
                    } else {
                        snap.items.remove(&key_id);
                    }
                }
                unlocked_until_new = Some(now_unix + dur_mins * 60);
                pulse_st.pulses_total = dur_mins as u32;
                pulse_st.pulses_remaining = dur_mins as u32;
                pulse_st.last_pulse_sec = None;
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
        };

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
            return Ok((0.0, true, unlocked_until_new, false));
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
            if pulse_st.pulses_remaining == 0 {
                return Err(BridgeRpcError {
                    code: "EssReservePulsesExhausted".into(),
                    message: "All ESS Reserve Bank pulses for this key have been exhausted".into(),
                });
            }
            if pulse_st.last_pulse_sec.is_some_and(|last| now_unix < last.saturating_add(60)) {
                return Err(BridgeRpcError {
                    code: "EssReservePulseCooldown".into(),
                    message: "ESS Reserve Bank pulse is on 60s cooldown".into(),
                });
            }
            pulse_st.reserve_linked_character_id = None;
            pulse_st.reserve_link_started_tick = None;
            if st.ess_linked_character_id == Some(char_id) {
                st.ess_linked_character_id = None;
                st.ess_link_started_tick = None;
            }
            let rem = pulse_st.pulses_remaining.max(1) as i64;
            let p = (st.ess_reserve_bank.0 / rem).max(1).min(st.ess_reserve_bank.0);
            st.ess_reserve_bank = IskCents(st.ess_reserve_bank.0.saturating_sub(p));
            pulse_st.pulses_remaining = pulse_st.pulses_remaining.saturating_sub(1);
            pulse_st.last_pulse_sec = Some(now_unix);
            if pulse_st.pulses_remaining == 0 {
                clear_unlock = true;
            }
            crate::services::space_destiny::set_ess_reserve_pulses_in_blobs(
                &mut snap.service_state_blobs,
                sys_id,
                &pulse_st,
            );
            IskCents(p)
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
        Ok((bank.to_wire_f64(), false, unlocked_until_new, clear_unlock))
    })?;

    if let Some(until) = unlocked_until_new {
        with_mut_combat_runtime(ctx, |rt| {
            rt.ess_reserve_unlocked_until.insert(sys_id, until);
        });
        if let Some(sde_sys) = ctx.sde.solar_systems.get(&sys_id) {
            let reg_id = sde_sys.region_id as i64;
            super::notify_emitters_batch6::emit_reserve_bank_unlock(ctx, sys_id, reg_id);
        }
    }
    if link_started_only {
        if method == "AttemptLinkToReserveBank" {
            super::notify_emitters_batch6::emit_ess_reserve_bank_link_notification(ctx, sys_id);
        } else {
            super::notify_emitters_batch6::emit_ess_main_bank_link_notification(ctx, sys_id);
        }
    }
    if clear_unlock {
        with_mut_combat_runtime(ctx, |rt| {
            rt.ess_reserve_unlocked_until.remove(&sys_id);
        });
    }

    // Linking to an ESS Main/Reserve Bank activates a 15-minute Suspect Timer (`feat:crimewatch.suspect-timer-looting-ess`)
    let suspect_until = now_unix.saturating_add(900);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(char_id);
        st.suspect_flag_until_unix = suspect_until;
    }
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
        let link_id = ess_ball_id;
        if method != "AttemptLinkToReserveBank" {
            super::notify_emitters_batch6::emit_main_bank_player_hack_success(ctx, char_id, sys_id, link_id);
            super::notify_emitters_batch6::emit_main_bank_player_link_disconnected(ctx, char_id, sys_id, "Completed", link_id);
        } else {
            // V24 `dynamicresources.common.ess.const.UNLINKED_MANUAL = 0` (`unlink_reason_description(0) -> None`, normal completion disconnect)
            super::notify_emitters_batch6::emit_reserve_bank_player_link_disconnected(ctx, char_id, sys_id, 0);
        }
        let (post_main, post_reserve) = ctx.db.with_snapshot(|snap| {
            snap.system_states
                .get(&sys_id)
                .map(|s| (s.ess_main_bank.to_wire_f64(), s.ess_reserve_bank.to_wire_f64()))
                .unwrap_or((0.0, 0.0))
        });
        let ess_data = PyRep::keyval([
            ("solarSystemID", PyRep::Integer(sys_id as i64)),
            ("mainBank", PyRep::Real(post_main)),
            ("reserveBank", PyRep::Real(post_reserve)),
        ]);
        super::notify_emitters_batch6::emit_system_ess_data_changed(ctx, sys_id as i64, ess_data);
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

pub const ESS_INTRUSION_BUBBLE_RADIUS_M: f64 = 75_000.0;
pub const ESS_MAIN_BANK_AUTOPAYOUT_SECS: u64 = 9_900;

/// Returns `true` if `ship_id` in `sys_id` is within the 75 km ESS intrusion bubble
/// (`feat:pve.ess-grid-warp-mwd-restrictions`).
pub async fn is_ship_in_ess_intrusion_bubble(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
) -> bool {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return false;
    };
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let Some(ess) = balls
        .into_iter()
        .find(|b| b.type_id == 32882 || b.type_id == 56002 || b.id == 820_000_000 + sys_id as i64)
    else {
        return false;
    };
    ship_ball.position.distance(ess.position) <= ESS_INTRUSION_BUBBLE_RADIUS_M
}

/// Rejects MWD, MJD, Cloaking Device, and Cynosural Field activation inside the 75 km ESS
/// intrusion bubble (`feat:pve.ess-grid-warp-mwd-restrictions`).
pub async fn check_ess_module_activation_allowed(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    module_type_id: u32,
) -> Result<(), BridgeRpcError> {
    let Some(t) = ctx.sde.types.get(&module_type_id) else {
        return Ok(());
    };
    let is_mwd = t.group_id == 46 && t.name.to_ascii_lowercase().contains("microwarpdrive");
    let is_mjd = t.group_id == 1189;
    let is_cloak = t.group_id == 330;
    let is_cyno = t.group_id == 658;
    if !(is_mwd || is_mjd || is_cloak || is_cyno) {
        return Ok(());
    }
    if is_ship_in_ess_intrusion_bubble(ctx, sys_id, ship_id).await {
        return Err(BridgeRpcError {
            code: "ModuleBlockedInEssBubble".into(),
            message: format!(
                "{} cannot be activated inside the 75 km ESS intrusion bubble",
                t.name
            ),
        });
    }
    Ok(())
}

/// Records a character's ratting bounty contribution to the system's ESS Main Bank cycle
/// (`feat:pve.ess-main-bank-autopayout`).
pub fn record_ess_main_bank_contribution(
    snap: &mut eve_db::WorldStateSnapshot,
    sys_id: u32,
    char_id: i64,
    ess_main_cents: i64,
    now_unix: u64,
) {
    if ess_main_cents <= 0 {
        return;
    }
    snap.sov_runtime
        .ess_main_bank_next_payout_unix
        .entry(sys_id)
        .or_insert_with(|| now_unix.saturating_add(ESS_MAIN_BANK_AUTOPAYOUT_SECS));
    *snap
        .sov_runtime
        .ess_main_bank_contributors
        .entry(sys_id)
        .or_default()
        .entry(char_id)
        .or_insert(0) += ess_main_cents;
}

/// Distributes the ESS Main Bank every 2h45m (`9,900` s) proportionally to ratting contributors
/// when no active Main Bank hack is in progress (`feat:pve.ess-main-bank-autopayout`).
pub fn step_ess_main_bank_autopayout(
    ctx: &ServerContext,
    sys_id: u32,
    now_unix: u64,
) -> Vec<(i64, i64)> {
    ctx.db.with_mut_snapshot(|snap| {
        if snap
            .system_states
            .get(&sys_id)
            .and_then(|s| s.ess_linked_character_id)
            .is_some()
        {
            return Vec::new();
        }
        let next_payout = snap
            .sov_runtime
            .ess_main_bank_next_payout_unix
            .get(&sys_id)
            .copied()
            .unwrap_or(0);
        if next_payout == 0 || now_unix < next_payout {
            return Vec::new();
        }
        let bank_cents = snap
            .system_states
            .get(&sys_id)
            .map(|s| s.ess_main_bank.0)
            .unwrap_or(0);
        snap.sov_runtime
            .ess_main_bank_next_payout_unix
            .insert(sys_id, now_unix.saturating_add(ESS_MAIN_BANK_AUTOPAYOUT_SECS));
        if bank_cents <= 0 {
            return Vec::new();
        }
        let contributors = snap
            .sov_runtime
            .ess_main_bank_contributors
            .remove(&sys_id)
            .unwrap_or_default();
        let total_contrib: i64 = contributors.values().copied().sum();
        if total_contrib <= 0 {
            return Vec::new();
        }
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.ess_main_bank = IskCents(0);
        }
        let mut payouts = Vec::new();
        let mut distributed = 0i64;
        let mut sorted: Vec<(i64, i64)> = contributors.into_iter().collect();
        sorted.sort_by_key(|(cid, _)| *cid);
        let len = sorted.len();
        for (idx, (cid, contrib)) in sorted.into_iter().enumerate() {
            let share = if idx + 1 == len {
                bank_cents.saturating_sub(distributed)
            } else {
                ((bank_cents as i128 * contrib as i128) / total_contrib as i128) as i64
            };
            if share <= 0 {
                continue;
            }
            distributed = distributed.saturating_add(share);
            let entry_id = { snap.next_entity_id += 1; snap.next_entity_id };
            if let Some(ch) = snap.characters.get_mut(&cid) {
                let delta = IskCents(share);
                ch.balance = ch.balance.checked_add(delta).unwrap_or(ch.balance);
                let new_bal = ch.balance;
                snap.wallet_journal.push(eve_db::WalletJournalEntry {
                    entry_id,
                    character_id: cid,
                    entry_type: "BountyPrizes".into(),
                    amount: delta,
                    balance_after: new_bal,
                    reason: format!("ESS Main Bank Auto-Payout (system {sys_id})"),
                });
            }
            payouts.push((cid, share));
        }
        payouts
    })
}
