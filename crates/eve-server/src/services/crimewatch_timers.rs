//! `crimewatch_timers` — CrimeWatch 2.0 Timers, High-Sec/Low-Sec Restrictions, Duels, Safety, Logi, Faction Police, Kill Rights, Security Tags & Sentry Guns (`C-8`).

#![forbid(unsafe_code)]

use super::*;

/// Evaluates Faction Police security status threshold for a given high-security system (`feat:crimewatch.faction-police-thresholds`).
pub fn faction_police_sec_threshold(system_security: f64) -> Option<f64> {
    let rounded = (system_security * 10.0).round() / 10.0;
    if rounded >= 1.0 { Some(-2.0) }
    else if rounded >= 0.9 { Some(-2.5) }
    else if rounded >= 0.8 { Some(-3.0) }
    else if rounded >= 0.7 { Some(-3.5) }
    else if rounded >= 0.6 { Some(-4.0) }
    else if rounded >= 0.5 { Some(-4.5) }
    else { None }
}

/// Evaluates whether Faction Police engage a pilot in `solar_system_id` (`feat:crimewatch.faction-police`, `feat:crimewatch.faction-police-thresholds`).
/// Below threshold in high-sec: cloak is disabled, docking/tethering in a ship is refused, docking in a capsule (`670`) is allowed, and capsules are not attacked.
pub async fn evaluate_faction_police_response(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id: i64,
    ship_id: i64,
) -> (bool, bool, bool) {
    if crate::services::combat_npc_police::is_faction_police_suppressed(ctx, solar_system_id) {
        return (false, true, false);
    }
    let sys_sec = ctx.sde.solar_systems.get(&solar_system_id).map(|s| s.security_status).unwrap_or(0.0);
    let Some(threshold) = faction_police_sec_threshold(sys_sec) else {
        return (false, true, false);
    };
    let (char_sec, is_capsule) = ctx.db.with_snapshot(|snap| {
        let sec = snap.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0);
        let is_pod = snap.items.get(&ship_id).map(|i| i.type_id == 670).unwrap_or(false);
        (sec, is_pod)
    });
    if char_sec <= threshold {
        let _ = ctx.systems.get_or_spawn(solar_system_id).break_gate_cloak(ship_id).await;
        if is_capsule { (false, true, true) } else { (true, false, true) }
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
    let (recipient_sec, helper_ship_id) = ctx
        .db
        .with_snapshot(|snap| {
            let r_sec = snap
                .characters
                .get(&recipient_char_id)
                .map(|c| c.security_status)
                .unwrap_or(0.0);
            let h_ship = snap
                .characters
                .get(&helper_char_id)
                .map(|c| c.active_ship_id)
                .unwrap_or(0);
            (r_sec, h_ship)
        });
    let is_outlaw = recipient_sec <= -4.95;

    let rec_in_le = with_combat_runtime(ctx, |rt| {
        let helper_les = rt.limited_engagements.get(&helper_char_id);
        rt.limited_engagements
            .get(&recipient_char_id)
            .is_some_and(|m| {
                m.iter().any(|(&pid, &exp)| {
                    pid != helper_char_id
                        && now_unix <= exp
                        && !helper_les
                            .and_then(|hm| hm.get(&pid))
                            .is_some_and(|&hexp| now_unix <= hexp)
                })
            })
    });
    let rec_in_war = is_war_interference_assistance(ctx, helper_char_id, recipient_char_id, now_unix);
    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
    let rec_state = cw.get_or_create(recipient_char_id).clone();
    let rec_is_criminal = rec_state.is_criminal(now_unix);
    let rec_is_suspect = rec_state.is_suspect(now_unix);

    let res = if rec_is_criminal && sys_sec > 0.0 {
        let _ = cw.handle_criminal_remote_assistance(
            helper_char_id,
            recipient_char_id,
            helper_ship_id,
            solar_system_id,
            sys_sec,
            now_unix,
        );
        ("Criminal", true, false)
    } else if rec_in_le || rec_in_war {
        if sys_sec >= 0.45 {
            let _ = cw.handle_criminal_remote_assistance(
                helper_char_id,
                recipient_char_id,
                helper_ship_id,
                solar_system_id,
                sys_sec,
                now_unix,
            );
            ("Criminal", true, false)
        } else if sys_sec > 0.0 {
            let helper_state = cw.get_or_create(helper_char_id);
            helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
            helper_state.pvp_timer_until_unix = helper_state.pvp_timer_until_unix.max(now_unix.saturating_add(900));
            helper_state.suspect_flag_until_unix = now_unix.saturating_add(900);
            ("Suspect", false, true)
        } else {
            let helper_state = cw.get_or_create(helper_char_id);
            helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
            ("Legal", false, false)
        }
    } else if rec_is_suspect || (is_outlaw && sys_sec >= 0.45) {
        let helper_state = cw.get_or_create(helper_char_id);
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        helper_state.pvp_timer_until_unix = helper_state.pvp_timer_until_unix.max(now_unix.saturating_add(900));
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

fn compute_crimewatch_timer_states(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> ([(i64, PyRep); 5], (i64, i64, i64, i64, i64), [u64; 5], Option<(i64, i64, i64, i64, i64)>) {
    let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
    let st = ctx.crimewatch.read().ok().and_then(|cw| cw.states.get(&char_id).cloned()).unwrap_or_default();
    let (npc_exp, dis_exp, prev_opt, trig_at) = with_combat_runtime(ctx, |rt| (
        rt.npc_timers.get(&char_id).copied().filter(|&exp| now_unix < exp),
        rt.disapproval_flags.get(&char_id).copied().filter(|&exp| now_unix < exp),
        rt.crimewatch_timer_states.get(&char_id).copied(),
        rt.crimewatch_timer_triggered_at.get(&char_id).copied().unwrap_or([0; 5]),
    ));
    let prev = prev_opt.unwrap_or((0, 0, 0, 0, 0));
    let mut next_trig = trig_at;
    let eval = |slot: usize, prev_sig: i64, exp: u64, active_c: i64, cd_c: i64, nt: &mut [u64; 5]| -> (i64, PyRep, i64) {
        let same_exp = (prev_sig >> 40) != 0 && (prev_sig & 0xFF_FFFF_FFFF) as u64 == exp;
        let c = if same_exp {
            if now_unix > nt[slot] { cd_c } else { active_c }
        } else {
            nt[slot] = now_unix;
            active_c
        };
        (c, PyRep::Integer(unix_to_ft(exp)), (c << 40) | (exp as i64))
    };
    let (wc, wv, ws) = if st.is_weapon_timer_active(now_unix) {
        eval(0, prev.0, st.weapon_timer_until_unix, 101, 102, &mut next_trig)
    } else { next_trig[0] = 0; (100, PyRep::None, 100) };
    let (pc, pv, ps) = if st.is_pvp_active(now_unix) {
        eval(1, prev.1, st.pvp_timer_until_unix, 201, 202, &mut next_trig)
    } else { next_trig[1] = 0; (200, PyRep::None, 200) };
    let (nc, nv, ns) = if let Some(exp) = npc_exp {
        eval(2, prev.2, exp, 401, 402, &mut next_trig)
    } else { next_trig[2] = 0; (400, PyRep::None, 400) };
    let (cc, cv, cs) = if st.is_criminal(now_unix) {
        eval(3, prev.3, st.criminal_flag_until_unix, 301, 303, &mut next_trig)
    } else if st.is_suspect(now_unix) {
        eval(3, prev.3, st.suspect_flag_until_unix, 302, 304, &mut next_trig)
    } else { next_trig[3] = 0; (300, PyRep::None, 300) };
    let (dc, dv, ds) = if let Some(exp) = dis_exp {
        eval(4, prev.4, exp, 501, 502, &mut next_trig)
    } else { next_trig[4] = 0; (500, PyRep::None, 500) };
    ([(wc, wv), (pc, pv), (nc, nv), (cc, cv), (dc, dv)], (ws, ps, ns, cs, ds), next_trig, prev_opt)
}

/// Pushes `OnWeaponsTimerUpdate`, `OnPvpTimerUpdate`, `OnNpcTimerUpdate`, and `OnCriminalTimerUpdate`
/// (`(state, expiryTime)` with Windows FILETIME expiry) to `char_id` on timer state changes (`feat:crimewatch.crimewatch-states`).
pub fn emit_crimewatch_timer_notifications(ctx: &ServerContext, char_id: i64, now_unix: u64) {
    let ([(weap_code, weap_val), (pvp_code, pvp_val), (npc_code, npc_val), (crim_code, crim_val), (dis_code, dis_val)], (weap_sig, pvp_sig, npc_sig, crim_sig, dis_sig), next_trig, prev_opt) =
        compute_crimewatch_timer_states(ctx, char_id, now_unix);
    let prev = prev_opt.unwrap_or((0, 0, 0, 0, 0));

    let is_all_idle = weap_code == 100 && pvp_code == 200 && npc_code == 400 && crim_code == 300 && dis_code == 500;
    let mut updates: Vec<(&str, Vec<PyRep>)> = Vec::new();
    let mut crim_transition: Option<(Vec<i64>, Vec<i64>, Vec<i64>)> = None;
    let mut dis_transition: Option<(Vec<i64>, Vec<i64>)> = None;
    with_mut_combat_runtime(ctx, |rt| {
        if prev_opt.is_none() && is_all_idle { return; }
        if prev.0 != weap_sig && !(prev.0 == 0 && weap_code == 100) { updates.push(("OnWeaponsTimerUpdate", vec![PyRep::Integer(weap_code), weap_val])); }
        if prev.1 != pvp_sig && !(prev.1 == 0 && pvp_code == 200) { updates.push(("OnPvpTimerUpdate", vec![PyRep::Integer(pvp_code), pvp_val])); }
        if prev.2 != npc_sig && !(prev.2 == 0 && npc_code == 400) { updates.push(("OnNpcTimerUpdate", vec![PyRep::Integer(npc_code), npc_val])); }
        if prev.3 != crim_sig && !(prev.3 == 0 && crim_code == 300) {
            updates.push(("OnCriminalTimerUpdate", vec![PyRep::Integer(crim_code), crim_val]));
            let prev_c = prev.3 >> 40;
            if matches!(crim_code, 301 | 303) && !matches!(prev_c, 301 | 303) {
                crim_transition = Some((vec![], vec![], vec![char_id]));
            } else if matches!(crim_code, 302 | 304) && !matches!(prev_c, 302 | 304) {
                crim_transition = Some((vec![], vec![char_id], vec![]));
            } else if crim_code == 300 && prev.3 != 0 && prev.3 != 300 {
                crim_transition = Some((vec![char_id], vec![], vec![]));
            }
        }
        if prev.4 != dis_sig && !(prev.4 == 0 && dis_code == 500) {
            updates.push(("OnDisapprovalTimerUpdate", vec![PyRep::Integer(dis_code), dis_val]));
            let prev_d = prev.4 >> 40;
            if matches!(dis_code, 501 | 502) && !matches!(prev_d, 501 | 502) {
                dis_transition = Some((vec![], vec![char_id]));
            } else if dis_code == 500 && prev.4 != 0 && prev.4 != 500 {
                dis_transition = Some((vec![char_id], vec![]));
            }
        }
        if is_all_idle {
            rt.crimewatch_timer_states.remove(&char_id);
            rt.crimewatch_timer_triggered_at.remove(&char_id);
        } else {
            rt.crimewatch_timer_states.insert(char_id, (weap_sig, pvp_sig, npc_sig, crim_sig, dis_sig));
            rt.crimewatch_timer_triggered_at.insert(char_id, next_trig);
        }
    });
    for (ev, mut a) in updates {
        let code_fn = |a: &mut Vec<PyRep>, def: i64| (a.pop().unwrap_or(PyRep::None), a.pop().and_then(|v| v.as_i64()).unwrap_or(def));
        match ev {
            "OnWeaponsTimerUpdate" => { let (exp, c) = code_fn(&mut a, 100); crate::services::notify_emitters_fleet_combat::emit_weapons_timer_update(ctx, char_id, c, exp); }
            "OnPvpTimerUpdate" => { let (exp, c) = code_fn(&mut a, 200); crate::services::notify_emitters_fleet_combat::emit_pvp_timer_update(ctx, char_id, c, exp); }
            "OnCriminalTimerUpdate" => { let (exp, c) = code_fn(&mut a, 300); crate::services::notify_emitters_fleet_combat::emit_criminal_timer_update(ctx, char_id, c, exp); }
            _ => { ctx.notify(NotificationTarget::Character(char_id), ev, "charid", a); }
        }
    }
    if let Some(sys_id) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.solar_system_id)) {
        if let Some((new_idles, new_suspects, new_criminals)) = crim_transition {
            crate::services::notify_emitters_fleet_combat::emit_system_criminal_flag_updates(ctx, sys_id, &new_idles, &new_suspects, &new_criminals);
        }
        if let Some((new_idles, new_naughty)) = dis_transition {
            crate::services::notify_emitters_combat_ess_progression::emit_system_disapproval_flag_updates(ctx, sys_id, &new_idles, &new_naughty);
        }
    }
}

/// Broadcasts `OnSecurityModified(solarSystemID, modifierAmount, newSecurity)` when system security changes.
pub fn update_system_security_status(ctx: &ServerContext, sys_id: u32, new_sec: f64) {
    let old_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    let delta = new_sec - old_sec;
    crate::services::notify_emitters_combat_ess_progression::emit_security_modified(ctx, sys_id, delta, new_sec);
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
    // In Insurgency Corruption Stage 5, regular low-sec (`0.0 < sec < 0.45`) gate/station sentry guns stop firing
    // (`Corruption_and_Suppression.md:56`); physical `79507` Counter-Insurgency Sentries act via `combat_npc_ai` on their own grids.
    if sys_sec <= 0.0 || crate::services::combat_npc_police::is_sentry_disabled_by_corruption(ctx, solar_system_id) {
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

/// Client `logConst` security-status transaction event type IDs (`appLogConst.pyj`).
pub const EVENT_SEC_STATUS_KILL_PIRATE_NPC: i64 = 378;
pub const EVENT_SEC_STATUS_ILLEGAL_AGGRESSION: i64 = 379;
pub const EVENT_SEC_STATUS_HAND_IN_TAGS: i64 = 385;

/// Applies standard character security-status loss and appends a `SecStatusTransaction` on illegal PvP aggression in empire space.
pub fn apply_illegal_pvp_sec_status_loss(
    ctx: &ServerContext,
    char_id: i64,
    victim_char_id: i64,
    sys_id: u32,
    sys_sec: f64,
    is_pod_target: bool,
    now_unix: u64,
) {
    if sys_sec <= 0.0 {
        return;
    }
    let (old_sec, target_sec) = ctx.db.with_snapshot(|snap| {
        let old = snap.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0);
        let tgt = snap.characters.get(&victim_char_id).map(|c| c.security_status).unwrap_or(0.0);
        (old, tgt)
    });
    let loss = crate::combat::crimewatch::compute_sec_status_loss(old_sec, target_sec, is_pod_target, sys_sec);
    let new_sec = (old_sec - loss).clamp(-10.0, 10.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = new_sec;
        }
    });
    with_mut_combat_runtime(ctx, |rt| {
        rt.sec_status_transactions
            .entry(char_id)
            .or_default()
            .push(SecStatusTransaction {
                timestamp_unix: now_unix,
                old_sec,
                new_sec,
                delta: new_sec - old_sec,
                reason: if is_pod_target { "Illegal Capsule Aggression".into() } else { "Illegal Ship Aggression".into() },
                solar_system_id: sys_id,
                event_type_id: EVENT_SEC_STATUS_ILLEGAL_AGGRESSION,
                other_owner_id: (victim_char_id > 0).then_some(victim_char_id),
                other_type_id: None,
            });
    });
    ctx.notify(NotificationTarget::Character(char_id), "OnSecurityStatusUpdate", "charid", vec![PyRep::Real(new_sec)]);
}

pub fn is_new_pvp_aggression(ctx: &ServerContext, attacker_char_id: i64, victim_char_id: i64, is_pod: bool, now_unix: u64) -> bool {
    ctx.crimewatch
        .read()
        .map(|cw| cw.get_aggression(attacker_char_id, victim_char_id, now_unix).is_none_or(|r| is_pod && !r.is_pod))
        .unwrap_or(true)
}

fn is_disapproval_system(sde: &eve_sde::SdeRegistry, sys_id: u32) -> bool {
    sde.solar_systems.get(&sys_id).is_some_and(|s| {
        s.region_id == 10_001_000 || s.name.eq_ignore_ascii_case("Zarzakh")
    })
}

pub fn on_weapon_aggression_post_hook(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_opt: Option<i64>,
    _sys_id: u32,
    now_unix: u64,
) {
    if victim_char_opt.is_none() {
        with_mut_combat_runtime(ctx, |rt| rt.trigger_npc_timer(attacker_char_id, now_unix));
    }
    emit_crimewatch_timer_notifications(ctx, attacker_char_id, now_unix);
}

pub fn record_illegal_pvp_penalties_if_new(
    ctx: &ServerContext, attacker_char_id: i64, victim_char_id: i64, sys_id: u32, sys_sec: f64, is_pod: bool, is_new_agg: bool, now_unix: u64,
) {
    if !is_new_agg || sys_sec <= 0.0 {
        return;
    }
    apply_illegal_pvp_sec_status_loss(ctx, attacker_char_id, victim_char_id, sys_id, sys_sec, is_pod, now_unix);
    if (sys_sec >= 0.45 || is_pod) && victim_char_id > 0 {
        let rt_expiries = with_combat_runtime(ctx, |rt| rt.kill_right_expires_unix.clone());
        let has_unexpired_kr = ctx.db.with_snapshot(|snap| {
            snap.kill_rights.values().any(|kr| {
                kr.owner_id == victim_char_id && kr.target_id == attacker_char_id && !kr.is_active_suspect
                    && rt_expiries.get(&kr.kill_right_id)
                        .or_else(|| snap.combat_crimewatch_state.kill_right_expires_unix.get(&kr.kill_right_id))
                        .is_some_and(|&exp| exp > now_unix)
            })
        });
        if !has_unexpired_kr {
            let kr_id = ctx.db.with_mut_snapshot(|snap| { snap.next_entity_id += 1; snap.next_entity_id });
            crate::services::combat_crimewatch::create_kill_right(ctx, kr_id, victim_char_id, attacker_char_id, 0.0);
        }
    }
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
            let expected_price_opt = args.get(1).and_then(|a| a.as_f64()).filter(|p| p.is_finite());
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
                if let Some(expected_isk) = expected_price_opt {
                    if expected_isk < 0.0 || IskCents::from_f64_isk(expected_isk) < kr.activation_fee {
                        return Err(BridgeRpcError { code: "KillRightPriceChanged".into(), message: "Offered price is lower than current kill right sale price".into() });
                    }
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
                snap.append_journal(
                    char_id,
                    "KillRightFee",
                    IskCents(-fee.0),
                    new_caller_bal,
                    format!("Purchased activation of kill right {kr_id} against {}", kr.target_id),
                );
                snap.append_journal(
                    kr.owner_id,
                    "KillRightFee",
                    fee,
                    new_owner_bal,
                    format!("Kill right {kr_id} purchased and activated by {char_id}"),
                );
                if let Some(kr_mut) = snap.kill_rights.get_mut(&kr_id) { kr_mut.is_active_suspect = true; }
                Ok((fee.to_wire_f64(), kr.owner_id, kr.target_id, new_caller_bal.to_wire_f64(), new_owner_bal.to_wire_f64()))
            });
            match res {
                Ok((fee_paid, owner_id, target_id, caller_bal, owner_bal)) => {
                    let now_unix = ctx.now_unix();
                    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner())
                        .get_or_create(target_id).suspect_flag_until_unix = now_unix.saturating_add(900);
                    ctx.notify(NotificationTarget::Character(char_id), "OnAccountChange", "charid", vec![PyRep::String("cash".into()), PyRep::Integer(char_id), PyRep::Real(caller_bal)]);
                    ctx.notify(NotificationTarget::Character(owner_id), "OnAccountChange", "charid", vec![PyRep::String("cash".into()), PyRep::Integer(owner_id), PyRep::Real(owner_bal)]);
                    emit_crimewatch_timer_notifications(ctx, target_id, now_unix);
                    ctx.notify(NotificationTarget::Character(owner_id), "OnKillRightSold", "charid", vec![PyRep::Integer(kr_id)]);
                    ctx.notify(NotificationTarget::Character(owner_id), "OnKillRightForYouSold", "charid", vec![PyRep::Integer(kr_id)]);
                    ctx.notify(NotificationTarget::Character(char_id), "OnKillRightActivated", "charid", vec![]);
                    ctx.notify(NotificationTarget::Character(char_id), "OnKillRightUsed", "charid", vec![PyRep::Integer(kr_id), PyRep::Integer(target_id)]);
                    ctx.notify(NotificationTarget::Character(target_id), "OnKillRightUsed", "charid", vec![PyRep::Integer(kr_id), PyRep::Integer(target_id)]);
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
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let _ = cw.get_or_create(char_id);
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
            let (le_entries, safety_lvl) = with_combat_runtime(ctx, |rt| {
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
                let saf = rt
                    .safety_levels
                    .get(&char_id)
                    .copied()
                    .unwrap_or(SafetyLevel::Green);
                (le, saf)
            });
            let (timers, _, _, _) = compute_crimewatch_timer_states(ctx, char_id, now_unix);
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Tuple(
                    timers
                        .into_iter()
                        .map(|(code, val)| PyRep::Tuple(vec![PyRep::Integer(code), val]))
                        .collect(),
                ),
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
                    // V24 `appLogConst.pyj` `logConst.eventSecStatus*` constants for `GetSecurityStatusTransactions`
                    let event_type_id = if t.event_type_id != 0 {
                        t.event_type_id
                    } else {
                        match t.reason.as_str() {
                            "Clone Soldier Security Tag" | "eventSecStatusHandInTags" => 385_i64,
                            "NPC Pirate Bounty" | "eventSecStatusKillPirateNpc" => 378_i64,
                            "Illegal PvP Aggression"
                            | "Illegal Ship Aggression"
                            | "Illegal Capsule Aggression"
                            | "eventSecStatusIllegalAggression" => 379_i64,
                            "eventSecStatusGmRollback" => 386_i64,
                            _ => 380_i64,
                        }
                    };
                    let event_date = crate::sp6_lifecycle::unix_to_filetime(t.timestamp_unix);
                    vec![
                        PyRep::Integer(event_date),
                        PyRep::Integer(event_type_id),
                        PyRep::Integer(t.solar_system_id as i64),
                        PyRep::Real(t.delta),
                        PyRep::Real(t.new_sec),
                        t.other_owner_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        t.other_type_id.map(|id| PyRep::Integer(id as i64)).unwrap_or(PyRep::None),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "eventDate",
                    "eventTypeID",
                    "locationID",
                    "modification",
                    "newValue",
                    "otherOwnerID",
                    "otherTypeID",
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
            // V24.01 `RespondToDuelChallenge(fromCharID, expiryTime, accept)` passes 3 positional
            // args where `args[1]` is a non-zero FILETIME integer; fail closed (`false`) if
            // `accept` is missing or not a valid boolean/3rd-arg value.
            let accept = if args.len() >= 3 {
                match args.get(2) {
                    Some(PyRep::Bool(b)) => *b,
                    Some(PyRep::Integer(v)) => *v != 0,
                    _ => false,
                }
            } else if let Some(PyRep::Bool(b)) = args.get(1) {
                *b
            } else {
                false
            };
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
            crate::services::combat_crimewatch::crimewatch_bounty::handle_apply_tags_for_sec_status(
                ctx, session, args,
            )
        }

        ("bountyProxy", _)
        | ("killRightMgr", "SellKillRight")
        | ("killRightMgr", "CancelSellKillRight") => {
            handle_bounty_proxy(ctx, session, service, method, args)
        }


        ("securityMgr", method) => {
            crate::services::security_mgr::handle_security_mgr_call(ctx, session, method, args)
        }

        _ => None,
    }
}

pub(crate) fn is_war_interference_assistance(ctx: &ServerContext, helper_char_id: i64, beneficiary_char_id: i64, now_unix: u64) -> bool {
    if !crate::sp6_lifecycle::has_active_pvp_timer(ctx, beneficiary_char_id, now_unix) {
        return false;
    }
    ctx.db.with_snapshot(|snap| {
        let (Some(helper), Some(ben)) = (snap.characters.get(&helper_char_id), snap.characters.get(&beneficiary_char_id)) else { return false; };
        let (h_ent, b_ent) = (helper.alliance_id.unwrap_or(helper.corporation_id), ben.alliance_id.unwrap_or(ben.corporation_id));
        snap.wars.values().any(|w| {
            if !w.active || (!w.mutual && now_unix < w.started_unix.saturating_add(86_400)) { return false; }
            if snap.war_state.wars.get(&w.war_id).and_then(|m| m.finish_at_unix).is_some_and(|f| now_unix >= f) { return false; }
            let is_agg = |ent: i64, corp: i64| w.aggressor_corp_id == ent || w.aggressor_corp_id == corp;
            let is_def_or_ally = |ent: i64, corp: i64| {
                w.defender_corp_id == ent || w.defender_corp_id == corp
                    || snap.war_state.negotiations.iter().any(|n| n.war_id == w.war_id && n.kind == eve_db::PersistedNegKind::Ally && n.status == "Accepted" && now_unix >= n.effective_unix.unwrap_or(0) && (n.from_corp == ent || n.from_corp == corp))
            };
            (is_agg(b_ent, ben.corporation_id) && !is_agg(h_ent, helper.corporation_id))
                || (is_def_or_ally(b_ent, ben.corporation_id) && !is_def_or_ally(h_ent, helper.corporation_id))
        })
    })
}

/// Pure read-only Crimewatch safety-level precheck (`SafetyLevel::Green` / `SafetyLevel::Yellow`)
/// before consuming superweapon isotopes, draining capacitor, or mutating any Crimewatch timer.
pub fn precheck_pvp_aggression_safety(
    ctx: &ServerContext,
    attacker_char_id: i64,
    sys_id: u32,
    target_id: i64,
    is_pod_target: bool,
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
        let is_mobile = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, it_tid);
        let secure = ctx.sde.types.get(&it_tid).is_some_and(|t| matches!(t.group_id, 340 | 448));
        (vcid, anchored && !is_mobile, secure, (it_tid == 23 && !anchored) || is_mobile)
    });

    if let Some(vcid) = victim_char_opt.filter(|&v| v != attacker_char_id) {
        let v_suspect_or_crim = {
            let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
            cw.states
                .get(&vcid)
                .is_some_and(|st| st.is_suspect(now_unix) || st.is_criminal(now_unix))
        };
        let has_le = with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(vcid, attacker_char_id, now_unix));
        let is_legal = ctx.db.with_snapshot(|snap| is_legal_war_or_suspect_target(snap, attacker_char_id, vcid, now_unix))
            || v_suspect_or_crim || has_le;
        let effective_sec = if is_unanchored_jetcan { sys_sec.min(0.40) } else { sys_sec };
        if !is_legal {
            let safety = with_combat_runtime(ctx, |rt| rt.safety_levels.get(&attacker_char_id).copied().unwrap_or(SafetyLevel::Red));
            let concord_suppressed = !is_pod_target
                && crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
            let would_be_criminal = (effective_sec >= 0.45 && !concord_suppressed)
                || (sys_sec > 0.0 && is_pod_target)
                || is_anchored_cont
                || (is_secure_cont && sys_sec >= 0.45);
            let would_be_suspect = !would_be_criminal && effective_sec > 0.0;
            if safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect) {
                return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
            }
            if safety == SafetyLevel::Yellow && would_be_criminal {
                return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
            }
        }
    } else if is_pod_target && sys_sec > 0.0 {
        let safety = with_combat_runtime(ctx, |rt| {
            rt.safety_levels.get(&attacker_char_id).copied().unwrap_or(SafetyLevel::Red)
        });
        if matches!(safety, SafetyLevel::Green | SafetyLevel::Yellow) {
            return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
        }
    }
    Ok(())
}

/// Evaluates ship safety, triggers 60s Weapons Timer, applies CrimeWatch Suspect/Criminal/CONCORD/PvP
/// timers + security-status loss, and creates/refreshes 300s bidirectional Limited Engagements when legal.
pub fn evaluate_and_record_pvp_aggression(
    ctx: &ServerContext, attacker_char_id: i64, attacker_ship_id: i64, sys_id: u32, target_id: i64, is_pod_target: bool,
) -> Result<(), BridgeRpcError> {
    if (32_000_000..=33_000_000).contains(&sys_id) {
        return Ok(());
    }
    precheck_pvp_aggression_safety(ctx, attacker_char_id, sys_id, target_id, is_pod_target)?;
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
        let is_mobile = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, it_tid);
        let secure = ctx.sde.types.get(&it_tid).is_some_and(|t| matches!(t.group_id, 340 | 448));
        (vcid, anchored && !is_mobile, secure, (it_tid == 23 && !anchored) || is_mobile)
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
        let is_new_agg = is_new_pvp_aggression(ctx, attacker_char_id, vcid, is_pod_target, now_unix);
        let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
        {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            cw.system_corruption_stages.insert(sys_id, c_stage);
            let _ = cw.handle_combat_aggression_with_pod(attacker_char_id, vcid, attacker_ship_id, sys_id, effective_sec, is_legal, is_pod_target, now_unix);
            cw.get_or_create(attacker_char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
            cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
            if !is_legal && ((sys_sec > 0.0 && sys_sec < 0.45 && is_pod_target) || is_anchored_cont || (is_secure_cont && sys_sec >= 0.45)) {
                let st = cw.get_or_create(attacker_char_id);
                st.criminal_flag_until_unix = now_unix.saturating_add(900);
                st.suspect_flag_until_unix = 0;
            }
        }
        if is_disapproval_system(&ctx.sde, sys_id) {
            with_mut_combat_runtime(ctx, |rt| {
                rt.disapproval_flags.insert(attacker_char_id, now_unix.saturating_add(900));
            });
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
        if !is_legal {
            record_illegal_pvp_penalties_if_new(ctx, attacker_char_id, vcid, sys_id, sys_sec, is_pod_target, is_new_agg, now_unix);
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
    let ben_outlaw = ctx.db.with_snapshot(|snap| snap.characters.get(&ben_id).map(|c| c.security_status <= -4.95).unwrap_or(false));
    let (ben_criminal, ben_suspect) = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(ben_id);
        (st.is_criminal(now_unix), st.is_suspect(now_unix) || (ben_outlaw && sys_sec >= 0.45))
    };
    let ben_le_partners: Vec<i64> = with_combat_runtime(ctx, |rt| {
        let helper_les = rt.limited_engagements.get(&helper_char_id);
        rt.limited_engagements.get(&ben_id)
            .map(|m| {
                m.iter()
                    .filter(|&(&pid, &exp)| {
                        pid != helper_char_id
                            && now_unix <= exp
                            && !helper_les.and_then(|hm| hm.get(&pid)).is_some_and(|&hexp| now_unix <= hexp)
                    })
                    .map(|(&pid, _)| pid)
                    .collect()
            })
            .unwrap_or_default()
    });
    let war_interference = is_war_interference_assistance(ctx, helper_char_id, ben_id, now_unix);
    let third_party_interference = !ben_le_partners.is_empty() || war_interference;
    let safety = with_combat_runtime(ctx, |rt| rt.safety_levels.get(&helper_char_id).copied().unwrap_or(SafetyLevel::Red));
    if ((ben_criminal && sys_sec > 0.0) || (third_party_interference && sys_sec >= 0.45))
        && matches!(safety, SafetyLevel::Green | SafetyLevel::Yellow)
    {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }
    if ((ben_suspect && sys_sec > 0.0) || (third_party_interference && sys_sec > 0.0)) && safety == SafetyLevel::Green {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }
    if (ben_criminal && sys_sec > 0.0) || (third_party_interference && sys_sec >= 0.45) {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let _ = cw.handle_criminal_remote_assistance(helper_char_id, ben_id, helper_ship_id, sys_id, sys_sec, now_unix);
        drop(cw);
        emit_crimewatch_timer_notifications(ctx, helper_char_id, now_unix);
        if ctx.db.with_snapshot(|snap| snap.characters.get(&helper_char_id).map(|c| c.solar_system_id)) != Some(sys_id) {
            let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[], &[helper_char_id]);
        }
    } else if (ben_suspect || third_party_interference) && sys_sec > 0.0 {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(helper_char_id);
        st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix.saturating_add(60));
        st.pvp_timer_until_unix = st.pvp_timer_until_unix.max(now_unix.saturating_add(900));
        st.suspect_flag_until_unix = st.suspect_flag_until_unix.max(now_unix.saturating_add(900));
        drop(cw);
        emit_crimewatch_timer_notifications(ctx, helper_char_id, now_unix);
        if ctx.db.with_snapshot(|snap| snap.characters.get(&helper_char_id).map(|c| c.solar_system_id)) != Some(sys_id) {
            let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[helper_char_id], &[]);
        }
    } else if third_party_interference {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(helper_char_id);
        st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix.saturating_add(60));
        st.pvp_timer_until_unix = st.pvp_timer_until_unix.max(now_unix.saturating_add(900));
    }
    if !ben_le_partners.is_empty() {
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

#[cfg(test)]
#[path = "crimewatch_timers_tests.rs"]
mod crimewatch_timers_tests;

