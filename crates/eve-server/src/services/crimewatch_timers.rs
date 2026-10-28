//! `crimewatch_timers` — CrimeWatch 2.0 Timers, High-Sec/Low-Sec Restrictions, Duels, Safety, Logi, Faction Police, Kill Rights, Security Tags & Sentry Guns (`C-8`).

#![forbid(unsafe_code)]

use super::*;

/// Evaluates Faction Police security status threshold for a given high-security system (`feat:crimewatch.faction-police-thresholds`).
pub fn faction_police_sec_threshold(system_security: f64) -> Option<f64> {
    let rounded = (system_security * 10.0).round() / 10.0;
    if rounded >= 1.0 {
        Some(-2.0)
    } else if rounded >= 0.9 {
        Some(-2.5)
    } else if rounded >= 0.8 {
        Some(-3.0)
    } else if rounded >= 0.7 {
        Some(-3.5)
    } else if rounded >= 0.6 {
        Some(-4.0)
    } else if rounded >= 0.5 {
        Some(-4.5)
    } else {
        None
    }
}

/// Evaluates whether Faction Police engage a pilot in `solar_system_id` (`feat:crimewatch.faction-police`, `feat:crimewatch.faction-police-thresholds`).
/// Below threshold in high-sec: cloak is disabled, docking/tethering in a ship is refused, docking in a capsule (`670`) is allowed, and capsules are not attacked.
pub async fn evaluate_faction_police_response(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id: i64,
    ship_id: i64,
) -> (bool, bool, bool) {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let Some(threshold) = faction_police_sec_threshold(sys_sec) else {
        return (false, true, false);
    };
    let (char_sec, is_capsule) = ctx.db.with_snapshot(|snap| {
        let sec = snap
            .characters
            .get(&char_id)
            .map(|c| c.security_status)
            .unwrap_or(0.0);
        let is_pod = snap
            .items
            .get(&ship_id)
            .map(|i| i.type_id == 670)
            .unwrap_or(false);
        (sec, is_pod)
    });
    if char_sec <= threshold {
        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = sys_handle.break_gate_cloak(ship_id).await;
        if is_capsule {
            // Capsules are not attacked and are allowed to dock
            (false, true, true)
        } else {
            // Ships are engaged by Faction Police and refused docking/tethering
            (true, false, true)
        }
    } else {
        (false, true, false)
    }
}

/// Evaluates third-party remote assistance (remote shield/armor/hull/cap) legality (`feat:crimewatch.logi-timers`, `feat:crimewatch.third-party-assistance-legality`, `feat:crimewatch.outlaw-status`).
pub fn evaluate_remote_assistance_legality(
    ctx: &ServerContext,
    helper_char_id: i64,
    recipient_char_id: i64,
    solar_system_id: u32,
    now_unix: u64,
) -> (&'static str, bool, bool) {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let recipient_sec = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters
                .get(&recipient_char_id)
                .map(|c| c.security_status)
                .unwrap_or(0.0)
        });
    let is_outlaw = recipient_sec <= -4.95;

    let rec_in_le = with_combat_runtime(ctx, |rt| {
        rt.is_in_any_limited_engagement(recipient_char_id, now_unix)
    });
    let rec_in_war = ctx.db.with_snapshot(|snap| {
        let Some(rec) = snap.characters.get(&recipient_char_id) else {
            return false;
        };
        let rec_ent = rec.alliance_id.unwrap_or(rec.corporation_id);
        snap.wars.values().any(|w| {
            w.active && (w.aggressor_corp_id == rec_ent || w.defender_corp_id == rec_ent)
        })
    });
    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
    let rec_state = cw.get_or_create(recipient_char_id).clone();
    let rec_is_criminal = rec_state.is_criminal(now_unix);
    let rec_is_suspect = rec_state.is_suspect(now_unix);

    let helper_state = cw.get_or_create(helper_char_id);
    let res = if rec_is_criminal {
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        helper_state.criminal_flag_until_unix = now_unix.saturating_add(900);
        ("Criminal", true, false)
    } else if rec_in_le || rec_in_war {
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        if sys_sec >= 0.45 {
            helper_state.criminal_flag_until_unix = now_unix.saturating_add(900);
            ("Criminal", true, false)
        } else if sys_sec > 0.0 {
            helper_state.suspect_flag_until_unix = now_unix.saturating_add(900);
            ("Suspect", false, true)
        } else {
            ("Legal", false, false)
        }
    } else if rec_is_suspect || (is_outlaw && sys_sec >= 0.45) {
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        helper_state.suspect_flag_until_unix = now_unix.saturating_add(900);
        ("Suspect", false, true)
    } else {
        ("Legal", false, false)
    };
    drop(cw);
    if res.0 != "Legal" {
        emit_crimewatch_timer_notifications(ctx, helper_char_id, now_unix);
    }
    res
}

/// Pushes `OnWeaponsTimerUpdate`, `OnPvpTimerUpdate`, `OnNpcTimerUpdate`, and `OnCriminalTimerUpdate`
/// (`(state, expiryTime)` with Windows FILETIME expiry) to `char_id` on timer state changes (`feat:crimewatch.crimewatch-states`).
pub fn emit_crimewatch_timer_notifications(ctx: &ServerContext, char_id: i64, now_unix: u64) {
    let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
    let st = ctx.crimewatch.read().ok().and_then(|cw| cw.states.get(&char_id).cloned()).unwrap_or_default();
    let npc_exp = with_combat_runtime(ctx, |rt| rt.npc_timers.get(&char_id).copied().filter(|&exp| now_unix < exp));
    let (weap_code, weap_val, weap_sig) = if st.is_weapon_timer_active(now_unix) {
        (101i64, PyRep::Integer(unix_to_ft(st.weapon_timer_until_unix)), st.weapon_timer_until_unix as i64)
    } else { (100i64, PyRep::None, 100i64) };
    let (pvp_code, pvp_val, pvp_sig) = if st.is_pvp_active(now_unix) {
        (201i64, PyRep::Integer(unix_to_ft(st.pvp_timer_until_unix)), st.pvp_timer_until_unix as i64)
    } else { (200i64, PyRep::None, 200i64) };
    let (npc_code, npc_val, npc_sig) = if let Some(exp) = npc_exp {
        (401i64, PyRep::Integer(unix_to_ft(exp)), exp as i64)
    } else { (400i64, PyRep::None, 400i64) };
    let (crim_code, crim_val, crim_sig) = if st.is_criminal(now_unix) {
        (301i64, PyRep::Integer(unix_to_ft(st.criminal_flag_until_unix)), st.criminal_flag_until_unix as i64)
    } else if st.is_suspect(now_unix) {
        (302i64, PyRep::Integer(unix_to_ft(st.suspect_flag_until_unix)), -(st.suspect_flag_until_unix as i64))
    } else { (300i64, PyRep::None, 300i64) };

    let is_all_idle = weap_code == 100 && pvp_code == 200 && npc_code == 400 && crim_code == 300;
    let mut updates: Vec<(&str, Vec<PyRep>)> = Vec::new();
    with_mut_combat_runtime(ctx, |rt| {
        let prev_opt = rt.crimewatch_timer_states.get(&char_id).copied();
        if prev_opt.is_none() && is_all_idle { return; }
        let prev = prev_opt.unwrap_or((0, 0, 0, 0));
        if prev.0 != weap_sig { updates.push(("OnWeaponsTimerUpdate", vec![PyRep::Integer(weap_code), weap_val])); }
        if prev.1 != pvp_sig { updates.push(("OnPvpTimerUpdate", vec![PyRep::Integer(pvp_code), pvp_val])); }
        if prev.2 != npc_sig { updates.push(("OnNpcTimerUpdate", vec![PyRep::Integer(npc_code), npc_val])); }
        if prev.3 != crim_sig { updates.push(("OnCriminalTimerUpdate", vec![PyRep::Integer(crim_code), crim_val])); }
        if is_all_idle {
            rt.crimewatch_timer_states.remove(&char_id);
        } else {
            rt.crimewatch_timer_states.insert(char_id, (weap_sig, pvp_sig, npc_sig, crim_sig));
        }
    });
    for (ev, a) in updates {
        ctx.notify(NotificationTarget::Character(char_id), ev, "charid", a);
    }
}

/// Evaluates whether an active Weapons Timer (60s), PvP Timer (15m), or High-Sec Criminal Timer blocks
/// jumping, docking, tethering, refitting, ejecting, storing, warping, or filament activation
/// (`feat:crimewatch.weapon-timer-scope`, `feat:crimewatch.weapon-timer-tether-filament-block`,
/// `feat:crimewatch.highsec-criminal-restrictions`, `feat:crimewatch.pvp-logoff-timer-renewal`,
/// `feat:navigation.wormhole-aggression-exemption`).
pub fn check_crimewatch_action_restriction(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    solar_system_id: u32,
    action: &str,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let is_capsule = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id == 670).unwrap_or(false));
    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
    let st = cw.get_or_create(char_id);
    let has_pending_concord = st.pending_concord_arrival.is_some();
    let has_weapon_timer = st.is_weapon_timer_active(now_unix);
    let has_pvp_timer = st.is_pvp_active(now_unix);
    let has_criminal_timer = st.is_criminal(now_unix);

    if matches!(action, "wormhole_jump" | "wh_jump")
        && !is_capsule
        && (has_pending_concord || (has_criminal_timer && sys_sec >= 0.45))
    {
        return Err(BridgeRpcError {
            code: "CantEnterWormholeGlobalCriminalFlag".into(),
            message: "Mysterious technology prevents a criminal with a pending CONCORD response from entering this wormhole".into(),
        });
    }

    if has_criminal_timer && sys_sec >= 0.45 {
        if action == "warp" && !is_capsule {
            return Err(BridgeRpcError {
                code: "CantWarpAwayGlobalCriminalFlag".into(),
                message: "Ships with an active criminal timer in high-security space cannot warp".into(),
            });
        }
        if matches!(
            action,
            "eject"
                | "stargate_jump"
                | "dock"
                | "store_ship"
                | "switch_ship"
                | "wormhole_jump"
                | "wh_jump"
                | "jump_drive"
                | "jump_bridge"
                | "jump_portal"
        ) && !is_capsule
        {
            const ERR_EJECT_CRIM: &str = "CannotEjectInvolvedInCrime";
            let mut code = match action {
                "stargate_jump" => "CantJumpGlobalCriminalFlag",
                "dock" => "CantDockGlobalCriminalFlag",
                "wormhole_jump" | "wh_jump" => "CantEnterWormholeGlobalCriminalFlag",
                "jump_drive" | "jump_bridge" | "jump_portal" => "CantBeaconJumpGlobalCriminalFlag",
                "store_ship" | "switch_ship" => "CannotBoardShipInHighSecAsCriminal",
                _ => "CantJumpGlobalCriminalFlag",
            };
            if action == "eject" {
                code = ERR_EJECT_CRIM;
            }
            return Err(BridgeRpcError {
                code: code.into(),
                message: format!(
                    "Action '{action}' is blocked by an active high-security criminal timer / CONCORD response"
                ),
            });
        }
    }

    if has_weapon_timer {
        // Cosmic wormhole jumping IS allowed under a weapons/PvP timer (unless criminal in high-sec above)
        if matches!(action, "wormhole_jump" | "wh_jump") {
            return Ok(());
        }
        if matches!(
            action,
            "stargate_jump"
                | "jump_drive"
                | "jump_bridge"
                | "jump_portal"
                | "dock"
                | "acceleration_gate"
                | "eject"
                | "store_ship"
                | "refit"
                | "refit_in_space"
                | "switch_ship"
                | "tether"
                | "filament"
                | "activate_filament"
        ) {
            const ERR_EJECT_WPN: &str = "CannotEjectActiveWeaponsTimer";
            let mut code = match action {
                "dock" => "WeaponsTimerActive",
                "stargate_jump" | "jump_drive" | "jump_bridge" | "jump_portal" | "acceleration_gate" => "CannotJumpWithWeaponsTimer",
                "store_ship" | "switch_ship" => "CannotStoreShipActiveWeaponsTimer",
                "refit" | "refit_in_space" => "CannotRefitInSpaceWithActiveWeaponsTimer",
                _ => "CannotJumpWithWeaponsTimer",
            };
            if action == "eject" {
                code = ERR_EJECT_WPN;
            }
            return Err(BridgeRpcError::user_error(code));
        }
    }

    if has_pvp_timer && matches!(action, "activate_filament" | "filament") {
        return Err(BridgeRpcError {
            code: "PvpTimerBlocksFilament".into(),
            message: "Cannot activate a filament while a 15-minute PvP Logoff Timer is active".into(),
        });
    }

    if has_pvp_timer && action == "safe_logoff" {
        return Err(BridgeRpcError {
            code: "PvpLogoffRestricted".into(),
            message: "Safe logoff is prohibited while a 15-minute PvP Logoff Timer is active".into(),
        });
    }

    Ok(())
}

/// Triggers or steps gate/station Sentry Gun grid aggression in high-sec/low-sec (`feat:npc.sentry-gun-grid-aggression`).
/// Sentries have 500 km optimal range, are invulnerable and untargetable, engage offenders while on grid,
/// drop aggression when the offender warps off-grid (> 500 km), and re-trigger on a new sec-status-loss action.
pub async fn step_sentry_gun_grid_aggression(
    ctx: &ServerContext,
    solar_system_id: u32,
    offender_char_id: i64,
    offender_ship_id: i64,
    sentry_origin: DVec3,
    new_sec_loss_act: bool,
) -> (bool, f64) {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    // In Insurgency Corruption Stage 3+, low-sec sentry guns are disabled
    let corruption_stage = ctx.db.with_snapshot(|snap| {
        snap.system_states
            .get(&solar_system_id)
            .map(|st| st.insurgency_corruption_stage)
            .unwrap_or(0)
    });
    if sys_sec <= 0.0 || (sys_sec < 0.45 && corruption_stage >= 3) {
        return (false, 0.0);
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let Some((_, ship_ball)) = sys_handle
        .get_ball_snapshot(offender_ship_id)
        .await
        .ok()
        .flatten()
    else {
        return (false, 0.0);
    };

    let dist = ship_ball.position.distance(sentry_origin);
    let on_grid = dist <= 500_000.0; // 500 km sentry optimal range

    let mut engaged = with_combat_runtime(ctx, |rt| {
        rt.sentry_gun_aggression
            .get(&(solar_system_id, offender_ship_id))
            .map(|s| s.active_on_grid)
            .unwrap_or(false)
    });

    if new_sec_loss_act && on_grid {
        engaged = true;
    } else if !on_grid {
        engaged = false;
    }

    let mut applied = 0.0;
    if engaged && on_grid {
        let sentry_dmg = DamageVector::new(88.0, 88.0, 88.0, 88.0);
        if let Ok(Some((dmg, _, _))) = sys_handle.apply_damage(offender_ship_id, sentry_dmg).await {
            applied = dmg;
        }
    }

    with_mut_combat_runtime(ctx, |rt| {
        let entry = rt
            .sentry_gun_aggression
            .entry((solar_system_id, offender_ship_id))
            .or_insert(SentryAggressionState {
                solar_system_id,
                offender_char_id,
                offender_ship_id,
                sentry_origin,
                active_on_grid: engaged,
                total_sentry_damage_dealt: 0.0,
            });
        entry.active_on_grid = engaged;
        entry.sentry_origin = sentry_origin;
        entry.total_sentry_damage_dealt += applied;
    });

    (engaged, applied)
}

fn check_kill_right_restriction(
    snap: &eve_db::WorldStateSnapshot,
    kr_id: i64,
    caller_id: i64,
) -> Result<Option<i64>, BridgeRpcError> {
    let restr_opt = snap
        .combat_crimewatch_state
        .kill_right_restricted_to
        .get(&kr_id)
        .copied()
        .filter(|&r| r > 0);
    if let Some(restr) = restr_opt {
        let caller_corp = snap
            .characters
            .get(&caller_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let caller_all = snap
            .corporations
            .get(&caller_corp)
            .and_then(|co| co.alliance_id)
            .unwrap_or(0);
        if caller_id != restr && caller_corp != restr && (caller_all == 0 || caller_all != restr) {
            return Err(BridgeRpcError {
                code: "KillRightRestricted".into(),
                message: format!("Kill right {kr_id} is restricted to entity {restr}"),
            });
        }
    }
    Ok(restr_opt)
}

pub async fn handle_crimewatch(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("killRightMgr", "ActivateKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_docked = session.station_id.is_some() || session.structure_id.is_some();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get(&kr_id).cloned() else {
                    return Err(BridgeRpcError { code: "KillRightNotFound".into(), message: format!("Kill right {kr_id} does not exist") });
                };
                if kr.owner_id != char_id {
                    let restr = check_kill_right_restriction(snap, kr_id, char_id)?;
                    if char_id == kr.target_id || restr.is_none() || kr.activation_fee.0 > 0 {
                        return Err(BridgeRpcError {
                            code: "NotKillRightOwner".into(),
                            message: format!("Kill right {kr_id} belongs to {}, not {char_id}; use BuyKillRight to purchase public activation", kr.owner_id),
                        });
                    }
                }
                if kr.is_active_suspect {
                    return Err(BridgeRpcError { code: "KillRightAlreadyActivated".into(), message: format!("Kill right {kr_id} is already activated") });
                }
                if is_docked {
                    return Err(BridgeRpcError { code: "KillRightCannotActivateDocked".into(), message: "Cannot activate a kill right while docked".into() });
                }
                let caller = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found") })?;
                let target = snap.characters.get(&kr.target_id).ok_or_else(|| BridgeRpcError { code: "TargetNotFound".into(), message: format!("Kill right target {} not found", kr.target_id) })?;
                if caller.station_id.is_some() || caller.structure_id.is_some() || target.station_id.is_some() || target.structure_id.is_some() || caller.solar_system_id != target.solar_system_id {
                    return Err(BridgeRpcError { code: "KillRightTargetNotInSystem".into(), message: "Caller and target must both be undocked in the same solar system".into() });
                }
                super::validate_kill_right_live_preconditions(ctx, snap, kr_id, char_id, kr.target_id)?;
                if let Some(kr_mut) = snap.kill_rights.get_mut(&kr_id) { kr_mut.is_active_suspect = true; }
                Ok(kr.target_id)
            });
            match res {
                Ok(target_id) => {
                    let now_unix = ctx.now_unix();
                    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner())
                        .get_or_create(target_id).suspect_flag_until_unix = now_unix.saturating_add(900);
                    emit_crimewatch_timer_notifications(ctx, target_id, now_unix);
                    ctx.notify(NotificationTarget::Character(char_id), "OnKillRightActivated", "charid", vec![]);
                    ctx.notify(NotificationTarget::Character(char_id), "OnKillRightUsed", "charid", vec![PyRep::Integer(kr_id), PyRep::Integer(target_id)]);
                    ctx.notify(NotificationTarget::Character(target_id), "OnKillRightUsed", "charid", vec![PyRep::Integer(kr_id), PyRep::Integer(target_id)]);
                    Some(Ok(PyRep::keyval([
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("suspectFlagActive", PyRep::Bool(true)),
                        ("suspectFlagged", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("killRightMgr", "BuyKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_docked = session.station_id.is_some() || session.structure_id.is_some();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get(&kr_id).cloned() else {
                    return Err(BridgeRpcError { code: "KillRightNotFound".into(), message: format!("Kill right {kr_id} does not exist") });
                };
                if kr.is_active_suspect {
                    return Err(BridgeRpcError { code: "KillRightAlreadyActivated".into(), message: format!("Kill right {kr_id} is already activated") });
                }
                if char_id == kr.target_id {
                    return Err(BridgeRpcError { code: "CannotBuyKillRightAgainstSelf".into(), message: "Target pilot cannot buy a kill right against themselves".into() });
                }
                if char_id == kr.owner_id {
                    return Err(BridgeRpcError { code: "AlreadyKillRightOwner".into(), message: "Kill right owner cannot buy their own kill right; use ActivateKillRight instead".into() });
                }
                if kr.activation_fee.0 <= 0 {
                    return Err(BridgeRpcError { code: "KillRightNotForSale".into(), message: format!("Kill right {kr_id} is not listed for public sale") });
                }
                check_kill_right_restriction(snap, kr_id, char_id)?;
                if is_docked {
                    return Err(BridgeRpcError { code: "KillRightCannotActivateDocked".into(), message: "Cannot activate or buy a kill right while docked".into() });
                }
                let caller = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found") })?;
                let target = snap.characters.get(&kr.target_id).ok_or_else(|| BridgeRpcError { code: "TargetNotFound".into(), message: format!("Kill right target {} not found", kr.target_id) })?;
                if caller.station_id.is_some() || caller.structure_id.is_some() || target.station_id.is_some() || target.structure_id.is_some() || caller.solar_system_id != target.solar_system_id {
                    return Err(BridgeRpcError { code: "KillRightTargetNotInSystem".into(), message: "Caller and target must both be undocked in the same solar system".into() });
                }
                super::validate_kill_right_live_preconditions(ctx, snap, kr_id, char_id, kr.target_id)?;
                let fee = kr.activation_fee;
                let Some(owner) = snap.characters.get(&kr.owner_id) else {
                    return Err(BridgeRpcError { code: "OwnerNotFound".into(), message: format!("Kill right owner {} not found", kr.owner_id) });
                };
                let new_owner_bal = owner.balance.checked_add(fee).ok_or_else(|| BridgeRpcError { code: "BalanceOverflow".into(), message: "Kill right owner ISK balance would overflow".into() })?;
                let Some(caller_mut) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found") });
                };
                let new_caller_bal = caller_mut.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError { code: "InsufficientFunds".into(), message: "Insufficient ISK to pay kill right activation fee".into() })?;
                caller_mut.balance = new_caller_bal;
                if let Some(owner_mut) = snap.characters.get_mut(&kr.owner_id) { owner_mut.balance = new_owner_bal; }
                if let Some(kr_mut) = snap.kill_rights.get_mut(&kr_id) { kr_mut.is_active_suspect = true; }
                Ok((fee.to_wire_f64(), kr.owner_id, kr.target_id))
            });
            match res {
                Ok((fee_paid, owner_id, target_id)) => {
                    let now_unix = ctx.now_unix();
                    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner())
                        .get_or_create(target_id).suspect_flag_until_unix = now_unix.saturating_add(900);
                    emit_crimewatch_timer_notifications(ctx, target_id, now_unix);
                    ctx.notify(NotificationTarget::Character(owner_id), "OnKillRightForYouSold", "charid", vec![PyRep::Integer(kr_id)]);
                    ctx.notify(NotificationTarget::Character(char_id), "OnKillRightActivated", "charid", vec![]);
                    ctx.notify(NotificationTarget::Character(char_id), "OnKillRightUsed", "charid", vec![PyRep::Integer(kr_id), PyRep::Integer(target_id)]);
                    Some(Ok(PyRep::keyval([
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("feePaidISK", PyRep::Real(fee_paid)),
                        ("suspectFlagActive", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("crimewatch", "GetMySecurityStatus") | ("crimewatchSvc", "GetMySecurityStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let sec = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status)).unwrap_or(0.0);
            Some(Ok(PyRep::Real(sec)))
        }

        // rpc-lint: allow legacy aggressionMgr alias
        ("aggressionMgr", "GetCriminalTimeStamps") | ("aggressionMgr", "CheckLootRightExceptions") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            let active_krs = ctx.db.with_snapshot(|snap| {
                snap.kill_rights.values().filter(|kr| kr.owner_id == char_id || kr.target_id == char_id)
                    .map(|kr| PyRep::Integer(kr.kill_right_id)).collect::<Vec<_>>()
            });
            let _ = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).is_criminal(now_unix);
            Some(Ok(PyRep::List(active_krs)))
        }

        ("crimewatch", "GetClientStates") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(char_id).clone();
            let mut criminals = Vec::new();
            let mut suspects = Vec::new();
            for (&cid, cst) in &cw.states {
                if cst.is_criminal(now_unix) {
                    criminals.push(PyRep::Integer(cid));
                } else if cst.is_suspect(now_unix) {
                    suspects.push(PyRep::Integer(cid));
                }
            }
            drop(cw);
            let (le_entries, dis_exp, npc_exp, safety_lvl) = with_combat_runtime(ctx, |rt| {
                let le: Vec<(PyRep, PyRep)> = rt
                    .limited_engagements
                    .get(&char_id)
                    .map(|m| {
                        m.iter()
                            .filter(|(_, &exp)| now_unix < exp)
                            .map(|(&tid, &exp)| (PyRep::Integer(tid), PyRep::Integer(exp as i64)))
                            .collect()
                    })
                    .unwrap_or_default();
                let dis = rt
                    .disapproval_flags
                    .get(&char_id)
                    .copied()
                    .filter(|&exp| now_unix < exp);
                let npc = rt
                    .npc_timers
                    .get(&char_id)
                    .copied()
                    .filter(|&exp| now_unix < exp);
                let saf = rt
                    .safety_levels
                    .get(&char_id)
                    .copied()
                    .unwrap_or(SafetyLevel::Green);
                (le, dis, npc, saf)
            });
            let timer = |idle: i64, active: i64, exp_opt: Option<u64>| {
                if let Some(exp) = exp_opt {
                    PyRep::Tuple(vec![PyRep::Integer(active), PyRep::Integer(unix_to_ft(exp))])
                } else {
                    PyRep::Tuple(vec![PyRep::Integer(idle), PyRep::None])
                }
            };
            let crim_tuple = if st.is_criminal(now_unix) {
                PyRep::Tuple(vec![
                    PyRep::Integer(301),
                    PyRep::Integer(unix_to_ft(st.criminal_flag_until_unix)),
                ])
            } else if st.is_suspect(now_unix) {
                PyRep::Tuple(vec![
                    PyRep::Integer(302),
                    PyRep::Integer(unix_to_ft(st.suspect_flag_until_unix)),
                ])
            } else {
                PyRep::Tuple(vec![PyRep::Integer(300), PyRep::None])
            };
            // `crimewatchSvc.ProcessSessionChange` (`eve/client/script/ui/services/crimewatchSvc.pyj:126-143`)
            // unpacks `(myCombatTimers, myEngagements, flaggedCharacters, safetyLevel)`.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    timer(
                        100,
                        101,
                        st.is_weapon_timer_active(now_unix)
                            .then_some(st.weapon_timer_until_unix),
                    ),
                    timer(
                        200,
                        201,
                        st.is_pvp_active(now_unix).then_some(st.pvp_timer_until_unix),
                    ),
                    timer(400, 401, npc_exp),
                    crim_tuple,
                    timer(500, 501, dis_exp),
                ]),
                PyRep::Dict(le_entries),
                PyRep::Tuple(vec![PyRep::List(criminals), PyRep::List(suspects)]),
                PyRep::Integer(safety_lvl as i64),
            ])))
        }

        ("crimewatch", "GetCharacterSecurityStatus") => {
            let (_, caller_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_id);
            let sec = ctx
                .db
                .with_snapshot(|s| s.characters.get(&target_char_id).map(|c| c.security_status))
                .unwrap_or(0.0);
            Some(Ok(PyRep::Real(sec)))
        }

        ("crimewatch", "GetSecurityStatusTransactions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status));
            let txs = with_combat_runtime(ctx, |rt| {
                rt.sec_status_transactions
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default()
            });
            let rows: Vec<Vec<PyRep>> = txs
                .into_iter()
                .map(|t| {
                    vec![
                        PyRep::Integer(t.timestamp_unix as i64),
                        PyRep::Real(t.old_sec),
                        PyRep::Real(t.new_sec),
                        PyRep::Real(t.delta),
                        PyRep::String(t.reason),
                        PyRep::Integer(t.solar_system_id as i64),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "timestamp",
                    "oldSecurityStatus",
                    "newSecurityStatus",
                    "modification",
                    "reason",
                    "solarSystemID",
                ],
                rows,
            )))
        }

        ("crimewatch", "SetSafetyLevel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.character_id));
            let raw_lvl = args.first().and_then(|a| a.as_i64()).unwrap_or(2);
            let lvl = match raw_lvl {
                0 => SafetyLevel::Red,
                1 => SafetyLevel::Yellow,
                _ => SafetyLevel::Green,
            };
            with_mut_combat_runtime(ctx, |rt| {
                rt.safety_levels.insert(char_id, lvl);
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSafetyLevelChanged",
                "charid",
                vec![PyRep::Integer(lvl as i64)],
            );
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("safetyLevel", PyRep::Integer(lvl as i64)),
            ])))
        }

        ("crimewatch", "StartDuelChallenge") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "StartDuelChallenge requires target characterID".into(),
                }));
            };
            if target_char_id <= 0 || target_char_id == char_id {
                return Some(Err(BridgeRpcError::user_error("InvalidDuelTarget")));
            }
            let (target_user_opt, from_corp_id, from_alliance_id) = ctx.db.with_snapshot(|s| {
                let tu = s.characters.get(&target_char_id).map(|c| c.user_id);
                let corp = s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let all = s.characters.get(&char_id).and_then(|c| c.alliance_id);
                (tu, corp, all)
            });
            let Some(target_user_id) = target_user_opt else {
                return Some(Err(BridgeRpcError::user_error("InvalidDuelTarget")));
            };
            if !ctx.is_character_online(target_char_id, target_user_id) {
                return Some(Err(BridgeRpcError::user_error("DuelChallengeFailNoSesssionForTarget")));
            }
            let now_unix = ctx.now_unix();
            let expiry_ft = ctx.now_blueos_filetime_ticks() + 600_000_000;
            with_mut_combat_runtime(ctx, |rt| {
                rt.duel_challenges.insert((char_id, target_char_id), now_unix.saturating_add(60));
            });
            ctx.notify(
                NotificationTarget::Character(target_char_id),
                "OnDuelChallenge",
                "charid",
                vec![
                    PyRep::Integer(char_id),
                    PyRep::Integer(from_corp_id),
                    from_alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::Integer(expiry_ft),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("challengerID", PyRep::Integer(char_id)),
                ("targetID", PyRep::Integer(target_char_id)),
                ("challengeIssued", PyRep::Bool(true)),
            ])))
        }

        ("crimewatch", "RespondToDuelChallenge") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(challenger_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RespondToDuelChallenge requires challengerID".into(),
                }));
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&challenger_id).map(|c| c.character_id));
            let accept = args.get(1).and_then(|a| a.as_bool()).unwrap_or(true);
            let now_unix = ctx.now_unix();
            let valid_challenge = with_mut_combat_runtime(ctx, |rt| {
                let Some(expires_at) = rt.duel_challenges.remove(&(challenger_id, char_id)) else {
                    return false;
                };
                if now_unix > expires_at {
                    return false;
                }
                if accept {
                    rt.add_limited_engagement(char_id, challenger_id, now_unix);
                    rt.add_limited_engagement(challenger_id, char_id, now_unix);
                }
                true
            });
            if !valid_challenge {
                return Some(Err(BridgeRpcError {
                    code: "NoPendingDuelChallenge".into(),
                    message: format!("No active duel challenge from {challenger_id} to {char_id}"),
                }));
            }
            if accept {
                let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnCrimewatchEngagementCreated",
                    "charid",
                    vec![PyRep::Integer(challenger_id), PyRep::Integer(timeout_ft)],
                );
                ctx.notify(
                    NotificationTarget::Character(challenger_id),
                    "OnCrimewatchEngagementCreated",
                    "charid",
                    vec![PyRep::Integer(char_id), PyRep::Integer(timeout_ft)],
                );
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnCrimewatchEngagementUpdated",
                    "charid",
                    vec![PyRep::Integer(challenger_id), PyRep::Bool(true)],
                );
                ctx.notify(
                    NotificationTarget::Character(challenger_id),
                    "OnCrimewatchEngagementUpdated",
                    "charid",
                    vec![PyRep::Integer(char_id), PyRep::Bool(true)],
                );
            }
            Some(Ok(PyRep::keyval([
                ("challengerID", PyRep::Integer(challenger_id)),
                ("responderID", PyRep::Integer(char_id)),
                ("accepted", PyRep::Bool(accept)),
                ("limitedEngagementSeconds", PyRep::Integer(if accept { 300 } else { 0 })),
            ])))
        }

        ("crimewatch", "ApplyTagsForSecStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedInStation".into(),
                    message: "Security status tag turn-in requires being docked at a CONCORD station".into(),
                }));
            }
            let cur_sec = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status))
                .unwrap_or(0.0);
            let (tag_type_id, qty) = if args.len() >= 2 {
                let t = args.first().and_then(|a| a.as_i64()).unwrap_or(35661) as u32;
                let q = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as i32;
                (t, q)
            } else {
                let t = if cur_sec < -8.0 {
                    35661
                } else if cur_sec < -5.0 {
                    35662
                } else if cur_sec < -2.0 {
                    35663
                } else {
                    35664
                };
                let q = args.first().and_then(|a| a.as_i64()).unwrap_or(1).max(1) as i32;
                (t, q)
            };
            let now_unix = ctx.now_unix();
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);

            // Enforce Clone Soldier Tag security status tier bands (`feat:crimewatch.sec-tag-tier-bands`):
            // 35661 Trainer:     -10.0 to -8.0
            // 35662 Recruiter:    -8.0 to -5.0
            // 35663 Transporter:  -5.0 to -2.0
            // 35664 Negotiator:   -2.0 to  0.0
            let band_opt = match tag_type_id {
                35661 | 35514 => Some((-10.0, -8.0)),
                35662 | 35515 => Some((-8.0, -5.0)),
                35663 | 35516 => Some((-5.0, -2.0)),
                35664 | 35517 => Some((-2.0, 0.0)),
                _ => None,
            };
            let Some((min_band, max_band)) = band_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSecurityTagType".into(),
                    message: format!("Type {tag_type_id} is not a valid Clone Soldier Tag (35661..35664)"),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let cur_sec = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.security_status)
                    .unwrap_or(0.0);
                if cur_sec < min_band - 1e-6 || cur_sec >= max_band - 1e-6 {
                    return Err(BridgeRpcError {
                        code: "SecurityTagBandMismatch".into(),
                        message: format!(
                            "Tag {tag_type_id} is only valid in security band [{min_band:.1}, {max_band:.1}); character is at {cur_sec:.2}"
                        ),
                    });
                }
                let station_id = session.station_id.unwrap_or(0);
                let ship_id_opt = session.ship_id;
                let tag_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.type_id == tag_type_id
                            && i.quantity >= qty
                            && ((i.location_id == station_id && i.flag == InventoryFlag::Hangar)
                                || (ship_id_opt.is_some_and(|sid| i.location_id == sid)
                                    && i.flag == InventoryFlag::Cargo))
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.item_id);
                let Some(iid) = tag_item_id else {
                    return Err(BridgeRpcError {
                        code: "InsufficientSecurityTags".into(),
                        message: format!("Missing {qty}x unlocked tag type {tag_type_id} in local station hangar or active ship cargo"),
                    });
                };
                if let Some(it) = snap.items.get_mut(&iid) {
                    if it.quantity > qty {
                        it.quantity -= qty;
                        it.version += 1;
                    } else {
                        snap.items.remove(&iid);
                    }
                }
                let new_sec = (cur_sec + 0.5 * (qty as f64)).min(max_band).min(0.0);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.security_status = new_sec;
                }
                Ok((cur_sec, new_sec))
            });

            match res {
                Ok((old_sec, new_sec)) => {
                    with_mut_combat_runtime(ctx, |rt| {
                        rt.sec_status_transactions
                            .entry(char_id)
                            .or_default()
                            .push(SecStatusTransaction {
                                timestamp_unix: now_unix,
                                old_sec,
                                new_sec,
                                delta: new_sec - old_sec,
                                reason: "Clone Soldier Security Tag".into(),
                                solar_system_id: sys_id,
                            });
                    });
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSecurityStatusUpdate",
                        "charid",
                        vec![PyRep::Real(new_sec)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("oldSecurityStatus", PyRep::Real(old_sec)),
                        ("newSecurityStatus", PyRep::Real(new_sec)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bountyProxy", _) => handle_bounty_proxy(ctx, session, service, method, args),


        ("securityMgr", "get_modified_systems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut sec_map: HashMap<u32, f64> = with_combat_runtime(ctx, |rt| {
                rt.modified_system_securities.clone()
            });
            ctx.db.with_snapshot(|snap| {
                for (&sid, st) in &snap.system_states {
                    if st.insurgency_corruption_stage >= 5 {
                        // Havoc Pirate Insurgency Stage 5 sets effective system security to lawless (0.0)
                        sec_map.insert(sid, 0.0);
                    }
                }
            });
            let entries = sec_map.into_iter().map(|(sid, sec)| (PyRep::Integer(sid as i64), PyRep::Real(sec))).collect();
            Some(Ok(PyRep::Dict(entries)))
        }

        ("abyssal_content_manager", "ActivateAbyssalKey") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError { code: "ShipDocked".into(), message: "Cannot activate Abyssal filament while docked".into() }));
            }
            if let Err(e) = crate::sp6_lifecycle::require_no_pvp_timer_for_filament(ctx, char_id) {
                return Some(Err(e));
            }
            let origin_sys = session.solar_system_id.unwrap_or(30_000_142);
            let ship_id = session.ship_id.unwrap_or(100_000_002);
            let filament_arg = args.first().and_then(|a| a.as_i64()).unwrap_or(47_765);
            let now_unix = ctx.now_unix();
            let resolved = ctx.db.try_with_mut_snapshot(|snap| {
                let found = snap.items.values()
                    .find(|i| i.owner_id == char_id && (i.item_id == filament_arg || i.type_id as i64 == filament_arg) && i.quantity >= 1)
                    .map(|i| (i.item_id, i.type_id));
                let Some((iid, tid)) = found else {
                    return Err(BridgeRpcError { code: "FilamentNotFound".into(), message: format!("Abyssal filament {filament_arg} not found in character inventory") });
                };
                // SDE-driven: group 1979 (Abyssal Filaments), attribute 2761 = tier (0..=6)
                let Some(tier) = ctx.sde.types.get(&tid).and_then(|t| (t.group_id == 1979).then(|| t.default_attributes.get(&2761).copied().unwrap_or(1.0) as u8)) else {
                    return Err(BridgeRpcError { code: "InvalidAbyssalFilament".into(), message: format!("Type {tid} is not a valid T0-T6 Abyssal filament") });
                };
                let origin_sec = ctx.sde.solar_systems.get(&origin_sys).map(|s| s.security_status).unwrap_or(0.9);
                if tier >= 4 && origin_sec >= 0.75 {
                    return Err(BridgeRpcError { code: "FilamentDisallowedInHighSec".into(), message: "Activating high-tier (T4-T6) Abyssal filaments is blocked in 0.8 - 1.0 high-security systems".into() });
                }
                if let Some(item) = snap.items.get_mut(&iid) {
                    if item.quantity > 1 { item.quantity -= 1; item.version += 1; } else { snap.items.remove(&iid); }
                }
                Ok((tid, tier, origin_sec))
            });
            let (filament_type_id, tier, origin_sec) = match resolved { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if tier >= 4 && (0.45..0.75).contains(&origin_sec) {
                ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).trigger_suspect_flag(char_id, now_unix);
            }
            crate::sp6_lifecycle::register_abyssal_session(
                ctx, char_id, ship_id, 32_000_001, origin_sys, now_unix, now_unix.saturating_add(1_200),
            );
            let (pocket_id, room1_npcs) = crate::services::pve_special_sites::enter_abyssal_pocket_for_character(
                ctx, session, char_id, filament_type_id, tier, origin_sys,
            ).await;
            Some(Ok(PyRep::keyval([
                ("pocketID", PyRep::Integer(pocket_id)),
                ("solarSystemID", PyRep::Integer(32_000_001)),
                ("originSolarSystemID", PyRep::Integer(origin_sys as i64)),
                ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
                ("tier", PyRep::Integer(tier as i64)),
                ("suspectFlagOnExit", PyRep::Bool(tier >= 4)),
                ("currentRoom", PyRep::Integer(1)),
                ("timerSeconds", PyRep::Integer(1_200)),
                ("npcBallIDs", PyRep::List(room1_npcs.iter().map(|&id| PyRep::Integer(id)).collect())),
            ])))
        }

        _ => None,
    }
}

/// Evaluates ship safety, triggers 60s Weapons Timer, applies CrimeWatch Suspect/Criminal/CONCORD/PvP
/// timers + security-status loss, and creates/refreshes 300s bidirectional Limited Engagements when legal.
pub fn evaluate_and_record_pvp_aggression(
    ctx: &ServerContext, attacker_char_id: i64, attacker_ship_id: i64, sys_id: u32, target_id: i64, is_pod_target: bool,
) -> Result<(), BridgeRpcError> {
    if (32_000_000..=33_000_000).contains(&sys_id) {
        return Ok(());
    }
    let now_unix = ctx.now_unix();
    let sys_sec = with_combat_runtime(ctx, |rt| rt.modified_system_securities.get(&sys_id).copied())
        .unwrap_or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0));
    let (victim_char_opt, is_anchored_cont, is_secure_cont, is_unanchored_jetcan) = ctx.db.with_snapshot(|snap| {
        let vcid = if snap.characters.contains_key(&target_id) {
            Some(target_id)
        } else {
            snap.characters.values().find(|c| c.active_ship_id == target_id).map(|c| c.character_id)
                .or_else(|| snap.items.get(&target_id).and_then(|it| snap.characters.contains_key(&it.owner_id).then_some(it.owner_id)))
        };
        let anchored = snap.sp6_lifecycle.space_object_meta.get(&target_id).is_some_and(|m| m.anchor_start_unix.is_some());
        let it_tid = snap.items.get(&target_id).map(|i| i.type_id).unwrap_or(0);
        let secure = ctx.sde.types.get(&it_tid).is_some_and(|t| matches!(t.group_id, 340 | 448));
        (vcid, anchored, secure, it_tid == 23 && !anchored)
    });

    if let Some(vcid) = victim_char_opt.filter(|&v| v != attacker_char_id) {
        let v_suspect_or_crim = {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(vcid);
            st.is_suspect(now_unix) || st.is_criminal(now_unix)
        };
        let has_le = with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(vcid, attacker_char_id, now_unix));
        let is_legal = ctx.db.with_snapshot(|snap| is_legal_war_or_suspect_target(snap, attacker_char_id, vcid, now_unix))
            || v_suspect_or_crim || has_le;
        let effective_sec = if is_unanchored_jetcan { sys_sec.min(0.40) } else { sys_sec };
        if !is_legal {
            let safety = with_combat_runtime(ctx, |rt| rt.safety_levels.get(&attacker_char_id).copied().unwrap_or(SafetyLevel::Red));
            let would_be_criminal = effective_sec >= 0.45 || (sys_sec > 0.0 && is_pod_target)
                || is_anchored_cont || (is_secure_cont && sys_sec >= 0.45);
            let would_be_suspect = !would_be_criminal && effective_sec > 0.0;
            if safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect) {
                return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
            }
            if safety == SafetyLevel::Yellow && would_be_criminal {
                return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
            }
        }
        {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let _ = cw.handle_combat_aggression(attacker_char_id, vcid, attacker_ship_id, sys_id, effective_sec, is_legal, now_unix);
            cw.get_or_create(attacker_char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
            cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
            if !is_legal && ((sys_sec > 0.0 && sys_sec < 0.45 && is_pod_target) || is_anchored_cont || (is_secure_cont && sys_sec >= 0.45)) {
                let st = cw.get_or_create(attacker_char_id);
                st.criminal_flag_until_unix = now_unix.saturating_add(900);
                st.suspect_flag_until_unix = 0;
            }
        }
        crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(ctx, attacker_char_id, vcid, now_unix);
        let le_created = with_mut_combat_runtime(ctx, |rt| {
            if v_suspect_or_crim || rt.has_limited_engagement_with(attacker_char_id, vcid, now_unix) {
                rt.add_limited_engagement(attacker_char_id, vcid, now_unix);
                rt.add_limited_engagement(vcid, attacker_char_id, now_unix);
                true
            } else { false }
        });
        if le_created {
            let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
            ctx.notify(NotificationTarget::Character(attacker_char_id), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(vcid), PyRep::Integer(timeout_ft)]);
            ctx.notify(NotificationTarget::Character(vcid), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(attacker_char_id), PyRep::Integer(timeout_ft)]);
        }
        if !is_legal && sys_sec > 0.0 {
            let old_sec = ctx.db.with_snapshot(|s| s.characters.get(&attacker_char_id).map(|c| c.security_status).unwrap_or(0.0));
            let loss = crate::combat::crimewatch::compute_sec_status_loss(old_sec, is_pod_target, sys_sec);
            ctx.db.with_mut_snapshot(|s| {
                if let Some(ch) = s.characters.get_mut(&attacker_char_id) { ch.security_status = (old_sec - loss).clamp(-10.0, 10.0); }
            });
        }
    } else {
        crate::sp6_lifecycle::record_npc_aggression_timer(ctx, attacker_char_id, now_unix);
    }
    Ok(())
}

/// Evaluates ship safety and flag inheritance when applying remote assistance (repair / cap transfer) to another pilot.
pub fn evaluate_and_record_remote_assistance(
    ctx: &ServerContext, helper_char_id: i64, helper_ship_id: i64, sys_id: u32, target_id: i64,
) -> Result<(), BridgeRpcError> {
    if (32_000_000..=33_000_000).contains(&sys_id) {
        return Ok(());
    }
    let now_unix = ctx.now_unix();
    let sys_sec = with_combat_runtime(ctx, |rt| rt.modified_system_securities.get(&sys_id).copied())
        .unwrap_or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0));
    let beneficiary_opt = ctx.db.with_snapshot(|snap| {
        if snap.characters.contains_key(&target_id) {
            Some(target_id)
        } else {
            snap.characters.values().find(|c| c.active_ship_id == target_id).map(|c| c.character_id)
                .or_else(|| snap.items.get(&target_id).and_then(|it| snap.characters.contains_key(&it.owner_id).then_some(it.owner_id)))
        }
    });
    let Some(ben_id) = beneficiary_opt.filter(|&b| b != helper_char_id) else { return Ok(()); };
    let (ben_criminal, ben_suspect) = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(ben_id);
        (st.is_criminal(now_unix), st.is_suspect(now_unix))
    };
    let ben_le_partners: Vec<i64> = with_combat_runtime(ctx, |rt| {
        rt.limited_engagements.get(&ben_id)
            .map(|m| m.iter().filter(|&(&pid, &exp)| pid != helper_char_id && now_unix <= exp).map(|(&pid, _)| pid).collect())
            .unwrap_or_default()
    });
    let safety = with_combat_runtime(ctx, |rt| rt.safety_levels.get(&helper_char_id).copied().unwrap_or(SafetyLevel::Red));
    if ben_criminal && sys_sec > 0.0 && matches!(safety, SafetyLevel::Green | SafetyLevel::Yellow) {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }
    if ((ben_suspect && sys_sec > 0.0) || !ben_le_partners.is_empty()) && safety == SafetyLevel::Green {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }
    if ben_criminal && sys_sec > 0.0 {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let _ = cw.handle_combat_aggression(helper_char_id, ben_id, helper_ship_id, sys_id, sys_sec, false, now_unix);
        cw.get_or_create(helper_char_id).criminal_flag_until_unix = now_unix.saturating_add(900);
        drop(cw);
        let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[helper_char_id], &[], &[]);
    } else if ben_suspect && sys_sec > 0.0 {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(helper_char_id);
        st.weapon_timer_until_unix = now_unix.saturating_add(60);
        st.pvp_timer_until_unix = now_unix.saturating_add(900);
        st.suspect_flag_until_unix = now_unix.saturating_add(900);
        drop(cw);
        let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[helper_char_id], &[]);
    }
    if !ben_le_partners.is_empty() {
        {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(helper_char_id);
            st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix.saturating_add(60));
            st.pvp_timer_until_unix = st.pvp_timer_until_unix.max(now_unix.saturating_add(900));
        }
        with_mut_combat_runtime(ctx, |rt| {
            for &pid in &ben_le_partners {
                rt.add_limited_engagement(helper_char_id, pid, now_unix);
                rt.add_limited_engagement(pid, helper_char_id, now_unix);
            }
        });
        let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
        for &pid in &ben_le_partners {
            ctx.notify(NotificationTarget::Character(helper_char_id), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(pid), PyRep::Integer(timeout_ft)]);
            ctx.notify(NotificationTarget::Character(pid), "OnCrimewatchEngagementCreated", "charid", vec![PyRep::Integer(helper_char_id), PyRep::Integer(timeout_ft)]);
        }
    }
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        if let Some(t_state) = cw.states.get(&ben_id).cloned() {
            let l_state = cw.get_or_create(helper_char_id);
            if t_state.is_weapon_timer_active(now_unix) {
                l_state.weapon_timer_until_unix = l_state.weapon_timer_until_unix.max(t_state.weapon_timer_until_unix);
            }
            if t_state.is_pvp_active(now_unix) {
                l_state.pvp_timer_until_unix = l_state.pvp_timer_until_unix.max(t_state.pvp_timer_until_unix);
            }
            if sys_sec <= 0.0 && t_state.is_suspect(now_unix) {
                l_state.suspect_flag_until_unix = l_state.suspect_flag_until_unix.max(t_state.suspect_flag_until_unix);
            }
            if sys_sec <= 0.0 && t_state.is_criminal(now_unix) {
                l_state.criminal_flag_until_unix = l_state.criminal_flag_until_unix.max(t_state.criminal_flag_until_unix);
            }
        }
    }
    Ok(())
}
