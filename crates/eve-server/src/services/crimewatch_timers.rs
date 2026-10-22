//! `crimewatch_timers` — CrimeWatch 2.0 Timers, High-Sec/Low-Sec Restrictions, Duels, Safety, Logi, Faction Police, Kill Rights, Security Tags & Sentry Guns (`C-8`).
// parity: feat:combat.player-bounties
// parity: feat:combat.bounty-leaderboards
// parity: feat:admin.gm-bounty-tools

#![forbid(unsafe_code)]

use super::*;

/// Claims up to 20% of the destroyed ship's reference value from the victim's (and corp/alliance)
/// bounty pool and credits the final-blow killer (`feat:combat.player-bounties`, `feat:combat.bounty-leaderboards`).
pub fn claim_player_bounty_on_kill(
    ctx: &ServerContext,
    killer_char_id: i64,
    victim_char_id: i64,
    victim_ship_type_id: u32,
) -> f64 {
    if killer_char_id <= 0 || victim_char_id <= 0 || killer_char_id == victim_char_id {
        return 0.0;
    }
    let (vic_corp, vic_all, kil_corp, kil_all) = ctx.db.with_snapshot(|snap| {
        let vc = snap
            .characters
            .get(&victim_char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let va = snap
            .corporations
            .get(&vc)
            .and_then(|co| co.alliance_id)
            .unwrap_or(0);
        let kc = snap
            .characters
            .get(&killer_char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let ka = snap
            .corporations
            .get(&kc)
            .and_then(|co| co.alliance_id)
            .unwrap_or(0);
        (vc, va, kc, ka)
    });
    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, victim_ship_type_id).max(250_000.0);
    let max_claim = 0.20 * ref_val;
    let claimed_isk = with_mut_combat_runtime(ctx, |rt| {
        let mut remaining_cap = max_claim;
        let mut claimed = 0.0;
        for tid in [victim_char_id, vic_corp, vic_all] {
            if tid <= 0 || remaining_cap <= 0.0 {
                continue;
            }
            if let Some(pool) = rt.player_bounty_pool.get_mut(&tid) {
                let take = (*pool).min(remaining_cap);
                if take > 0.0 {
                    *pool -= take;
                    remaining_cap -= take;
                    claimed += take;
                }
                if *pool <= 1e-6 {
                    rt.player_bounty_pool.remove(&tid);
                }
            }
        }
        if claimed > 0.0 {
            *rt.bounty_hunter_claimed.entry(killer_char_id).or_insert(0.0) += claimed;
            if kil_corp > 0 {
                *rt.bounty_hunter_claimed.entry(kil_corp).or_insert(0.0) += claimed;
            }
            if kil_all > 0 {
                *rt.bounty_hunter_claimed.entry(kil_all).or_insert(0.0) += claimed;
            }
        }
        claimed
    });
    if claimed_isk > 0.0 {
        let new_bal = ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
                let add = IskCents::from_f64_isk(claimed_isk);
                ch.balance = ch.balance.checked_add(add).unwrap_or(ch.balance);
                Some(ch.balance.to_wire_f64())
            } else {
                None
            }
        });
        if let Some(bal) = new_bal {
            let _ = ctx.notify_account_change(killer_char_id, "cash", killer_char_id, bal);
        }
        ctx.notify(
            NotificationTarget::Character(killer_char_id),
            "OnBountyAddedToPayout",
            "charid",
            vec![PyRep::Integer(victim_char_id), PyRep::Real(claimed_isk)],
        );
    }
    claimed_isk
}

/// GM/Admin bounty manipulation tool (`set`, `clear`, `reimburse`) (`feat:admin.gm-bounty-tools`).
pub fn gm_admin_bounty_action(
    ctx: &ServerContext,
    session: &SessionState,
    action: &str,
    target_id: i64,
    amount_isk: f64,
) -> Result<f64, BridgeRpcError> {
    if !session.has_gm_role() && (session.role & eve_machonet::service_roles::ROLE_QA) == 0 {
        return Err(BridgeRpcError {
            code: "InsufficientRole".into(),
            message: "GM bounty tools require a GM/Admin/QA role".into(),
        });
    }
    match action {
        "set" => {
            let val = amount_isk.max(0.0);
            with_mut_combat_runtime(ctx, |rt| {
                if val > 0.0 {
                    rt.player_bounty_pool.insert(target_id, val);
                } else {
                    rt.player_bounty_pool.remove(&target_id);
                }
            });
            Ok(val)
        }
        "clear" => {
            let prev = with_mut_combat_runtime(ctx, |rt| {
                if target_id > 0 {
                    rt.bounty_contributions.remove(&target_id);
                    rt.player_bounty_pool.remove(&target_id).unwrap_or(0.0)
                } else {
                    let sum: f64 = rt.player_bounty_pool.values().sum();
                    rt.player_bounty_pool.clear();
                    rt.bounty_contributions.clear();
                    sum
                }
            });
            Ok(prev)
        }
        "reimburse" => {
            let refunds = with_mut_combat_runtime(ctx, |rt| {
                if target_id > 0 {
                    rt.player_bounty_pool.remove(&target_id);
                    rt.bounty_contributions.remove(&target_id).unwrap_or_default()
                } else {
                    rt.player_bounty_pool.clear();
                    let all = std::mem::take(&mut rt.bounty_contributions);
                    all.into_values().flatten().collect()
                }
            });
            let mut total_reimbursed = 0.0;
            ctx.db.with_mut_snapshot(|snap| {
                for (cid, amt) in refunds {
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        let add = IskCents::from_f64_isk(amt);
                        ch.balance = ch.balance.checked_add(add).unwrap_or(ch.balance);
                        total_reimbursed += amt;
                    }
                }
            });
            Ok(total_reimbursed)
        }
        _ => Err(BridgeRpcError {
            code: "InvalidGmBountyAction".into(),
            message: format!("Unknown GM bounty action '{action}'"),
        }),
    }
}

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
    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
    let rec_state = cw.get_or_create(recipient_char_id).clone();
    let rec_is_criminal = rec_state.is_criminal(now_unix);
    let rec_is_suspect = rec_state.is_suspect(now_unix);
    let rec_in_war = ctx.db.with_snapshot(|snap| {
        let Some(rec) = snap.characters.get(&recipient_char_id) else {
            return false;
        };
        let rec_ent = rec.alliance_id.unwrap_or(rec.corporation_id);
        snap.wars.values().any(|w| {
            w.active && (w.aggressor_corp_id == rec_ent || w.defender_corp_id == rec_ent)
        })
    });

    let helper_state = cw.get_or_create(helper_char_id);
    if rec_is_criminal {
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        helper_state.criminal_flag_until_unix = now_unix.saturating_add(900);
        return ("Criminal", true, false);
    }
    if rec_in_le || rec_in_war {
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        if sys_sec >= 0.45 {
            helper_state.criminal_flag_until_unix = now_unix.saturating_add(900);
            return ("Criminal", true, false);
        } else if sys_sec > 0.0 {
            helper_state.suspect_flag_until_unix = now_unix.saturating_add(900);
            return ("Suspect", false, true);
        }
    }
    if rec_is_suspect || (is_outlaw && sys_sec >= 0.45) {
        helper_state.weapon_timer_until_unix = now_unix.saturating_add(60);
        helper_state.suspect_flag_until_unix = now_unix.saturating_add(900);
        return ("Suspect", false, true);
    }
    // Assisting an outlaw in low-sec (< 0.45) is not an offence
    ("Legal", false, false)
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
    let has_weapon_timer = st.is_weapon_timer_active(now_unix);
    let has_pvp_timer = st.is_pvp_active(now_unix);
    let has_criminal_timer = st.is_criminal(now_unix);

    if has_criminal_timer && sys_sec >= 0.45 {
        if action == "warp" && !is_capsule {
            return Err(BridgeRpcError {
                code: "CriminalWarpRestricted".into(),
                message: "Ships with an active criminal timer in high-security space cannot warp".into(),
            });
        }
        if matches!(
            action,
            "eject" | "stargate_jump" | "dock" | "store_ship" | "switch_ship" | "wormhole_jump"
        ) && !is_capsule
        {
            return Err(BridgeRpcError {
                code: "CriminalActionRestricted".into(),
                message: format!(
                    "Action '{action}' is blocked by an active high-security criminal timer / CONCORD response"
                ),
            });
        }
    }

    if has_weapon_timer {
        // Wormhole jumping IS allowed under a weapons/PvP timer (unless criminal in high-sec above)
        if action == "wormhole_jump" {
            return Ok(());
        }
        if matches!(
            action,
            "stargate_jump"
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
            return Err(BridgeRpcError {
                code: "WeaponsTimerActive".into(),
                message: format!("Action '{action}' is prohibited while a 60s Weapons Timer is active"),
            });
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get(&kr_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "KillRightNotFound".into(),
                        message: format!("Kill right {kr_id} does not exist"),
                    });
                };
                if kr.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "NotKillRightOwner".into(),
                        message: format!(
                            "Kill right {kr_id} belongs to {}, not {char_id}; use BuyKillRight to purchase public activation",
                            kr.owner_id
                        ),
                    });
                }
                if kr.is_active_suspect {
                    return Err(BridgeRpcError {
                        code: "KillRightAlreadyActivated".into(),
                        message: format!("Kill right {kr_id} is already activated"),
                    });
                }
                if let Some(kr_mut) = snap.kill_rights.get_mut(&kr_id) {
                    kr_mut.is_active_suspect = true;
                }
                Ok(kr.target_id)
            });
            match res {
                Ok(target_id) => {
                    let now_unix = ctx.now_unix();
                    ctx.crimewatch
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_create(target_id)
                        .suspect_flag_until_unix = now_unix.saturating_add(900);
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get(&kr_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "KillRightNotFound".into(),
                        message: format!("Kill right {kr_id} does not exist"),
                    });
                };
                if kr.is_active_suspect {
                    return Err(BridgeRpcError {
                        code: "KillRightAlreadyActivated".into(),
                        message: format!("Kill right {kr_id} is already activated"),
                    });
                }
                if kr.activation_fee.0 <= 0 {
                    return Err(BridgeRpcError {
                        code: "KillRightNotForSale".into(),
                        message: format!("Kill right {kr_id} is not listed for public sale"),
                    });
                }
                let fee = kr.activation_fee;
                let Some(caller) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_caller_bal =
                    caller.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Insufficient ISK to pay kill right activation fee".into(),
                    })?;
                caller.balance = new_caller_bal;
                if let Some(owner) = snap.characters.get_mut(&kr.owner_id) {
                    owner.balance = owner.balance.checked_add(fee).unwrap_or(owner.balance);
                }
                if let Some(kr_mut) = snap.kill_rights.get_mut(&kr_id) {
                    kr_mut.is_active_suspect = true;
                }
                Ok(fee.to_wire_f64())
            });
            match res {
                Ok(fee_paid) => Some(Ok(PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr_id)),
                    ("feePaidISK", PyRep::Real(fee_paid)),
                    ("suspectFlagActive", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("crimewatch", "GetMySecurityStatus") | ("crimewatchSvc", "GetMySecurityStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sec = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status))
                .unwrap_or(0.0);
            Some(Ok(PyRep::Real(sec)))
        }

        // rpc-lint: allow legacy aggressionMgr alias
        ("aggressionMgr", "GetCriminalTimeStamps") | ("aggressionMgr", "CheckLootRightExceptions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let active_krs = ctx.db.with_snapshot(|snap| {
                snap.kill_rights
                    .values()
                    .filter(|kr| kr.owner_id == char_id || kr.target_id == char_id)
                    .map(|kr| PyRep::Integer(kr.kill_right_id))
                    .collect::<Vec<_>>()
            });
            let _ = ctx
                .crimewatch
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_create(char_id)
                .is_criminal(now_unix);
            Some(Ok(PyRep::List(active_krs)))
        }

        ("crimewatch", "GetClientStates") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let now_ft = ctx.now_blueos_filetime_ticks();
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
            let (le_entries, dis_active, npc_active, safety_lvl) = with_combat_runtime(ctx, |rt| {
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
                    .is_some_and(|&exp| now_unix < exp);
                let npc = rt.is_npc_timer_active(char_id, now_unix);
                let saf = rt
                    .safety_levels
                    .get(&char_id)
                    .copied()
                    .unwrap_or(SafetyLevel::Green);
                (le, dis, npc, saf)
            });
            let timer = |idle: i64, active: i64, is_on: bool| {
                if is_on {
                    PyRep::Tuple(vec![PyRep::Integer(active), PyRep::Integer(now_ft)])
                } else {
                    PyRep::Tuple(vec![PyRep::Integer(idle), PyRep::None])
                }
            };
            let crim_tuple = if st.is_criminal(now_unix) {
                PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(now_ft)])
            } else if st.is_suspect(now_unix) {
                PyRep::Tuple(vec![PyRep::Integer(302), PyRep::Integer(now_ft)])
            } else {
                PyRep::Tuple(vec![PyRep::Integer(300), PyRep::None])
            };
            // `crimewatchSvc.ProcessSessionChange` (`eve/client/script/ui/services/crimewatchSvc.pyj:126-143`)
            // unpacks `(myCombatTimers, myEngagements, flaggedCharacters, safetyLevel)`.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    timer(100, 101, st.is_weapon_timer_active(now_unix)),
                    timer(200, 201, st.is_pvp_active(now_unix)),
                    timer(400, 401, npc_active),
                    crim_tuple,
                    timer(500, 501, dis_active),
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
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&target_char_id).map(|c| c.character_id));
            let now_unix = ctx.now_unix();
            with_mut_combat_runtime(ctx, |rt| {
                rt.duel_challenges.insert((char_id, target_char_id), now_unix);
            });
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
            let tag_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(35661) as u32;
            let qty = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as i32;
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
                let tag_item_id = snap
                    .items
                    .values()
                    .find(|i| i.owner_id == char_id && i.type_id == tag_type_id && i.quantity >= qty)
                    .map(|i| i.item_id);
                let Some(iid) = tag_item_id else {
                    return Err(BridgeRpcError {
                        code: "InsufficientSecurityTags".into(),
                        message: format!("Missing {qty}x tag type {tag_type_id}"),
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
                    Some(Ok(PyRep::keyval([
                        ("oldSecurityStatus", PyRep::Real(old_sec)),
                        ("newSecurityStatus", PyRep::Real(new_sec)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bountyProxy", "SellKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let price_isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(1_000_000.0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get_mut(&kr_id) else {
                    return Err(BridgeRpcError {
                        code: "KillRightNotFound".into(),
                        message: format!("Kill right {kr_id} not found"),
                    });
                };
                if kr.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "NotKillRightOwner".into(),
                        message: "Only the kill right owner can list it for sale".into(),
                    });
                }
                kr.activation_fee = IskCents::from_f64_isk(price_isk.max(0.0));
                Ok(kr.activation_fee.to_wire_f64())
            });
            match res {
                Ok(fee) => Some(Ok(PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr_id)),
                    ("priceISK", PyRep::Real(fee)),
                    ("forSale", PyRep::Bool(fee > 0.0)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bountyProxy", "CancelSellKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get_mut(&kr_id) else {
                    return Err(BridgeRpcError {
                        code: "KillRightNotFound".into(),
                        message: format!("Kill right {kr_id} not found"),
                    });
                };
                if kr.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "NotKillRightOwner".into(),
                        message: "Only the kill right owner can cancel public sale".into(),
                    });
                }
                kr.activation_fee = IskCents(0);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr_id)),
                    ("forSale", PyRep::Bool(false)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bountyProxy", "GetMyKillRights") | ("bountyProxy", "GetKillRightsOnCharacters") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list = ctx.db.with_snapshot(|snap| {
                snap.kill_rights
                    .values()
                    .filter(|kr| kr.owner_id == char_id || kr.target_id == char_id)
                    .map(|kr| {
                        PyRep::keyval([
                            ("killRightID", PyRep::Integer(kr.kill_right_id)),
                            ("fromID", PyRep::Integer(kr.owner_id)),
                            ("toID", PyRep::Integer(kr.target_id)),
                            ("price", PyRep::Real(kr.activation_fee.to_wire_f64())),
                            ("activeSuspect", PyRep::Bool(kr.is_active_suspect)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        ("securityMgr", "get_modified_systems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut entries: Vec<(PyRep, PyRep)> = with_combat_runtime(ctx, |rt| {
                rt.modified_system_securities
                    .iter()
                    .map(|(&sid, &sec)| (PyRep::Integer(sid as i64), PyRep::Real(sec)))
                    .collect()
            });
            ctx.db.with_snapshot(|snap| {
                for (&sid, st) in &snap.system_states {
                    if st.insurgency_corruption_stage > 0 {
                        let base_sec = ctx
                            .sde
                            .solar_systems
                            .get(&sid)
                            .map(|s| s.security_status)
                            .unwrap_or(0.5);
                        let eff_sec = if st.insurgency_corruption_stage >= 5 {
                            0.0
                        } else {
                            (base_sec - 0.1 * (st.insurgency_corruption_stage as f64)).max(0.0)
                        };
                        entries.push((PyRep::Integer(sid as i64), PyRep::Real(eff_sec)));
                    }
                }
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("bountyProxy", "AddToBounty") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let amount_isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            if target_id <= 0 || amount_isk < 100_000.0 {
                return Some(Err(BridgeRpcError {
                    code: "BountyAmountTooLow".into(),
                    message: "Minimum bounty placement is 100,000 ISK on a valid target ID".into(),
                }));
            }
            let deduct_res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let cost = IskCents::from_f64_isk(amount_isk);
                let new_bal = ch.balance.checked_sub(cost).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Insufficient ISK to place bounty".into(),
                })?;
                ch.balance = new_bal;
                Ok(new_bal.to_wire_f64())
            });
            let new_bal = match deduct_res {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx.notify_account_change(char_id, "cash", char_id, new_bal);
            let total_pool = with_mut_combat_runtime(ctx, |rt| {
                rt.bounty_contributions
                    .entry(target_id)
                    .or_default()
                    .push((char_id, amount_isk));
                let entry = rt.player_bounty_pool.entry(target_id).or_insert(0.0);
                *entry += amount_isk;
                *entry
            });
            ctx.notify(
                NotificationTarget::Character(target_id),
                "OnBountyPlaced",
                "charid",
                vec![
                    PyRep::Integer(char_id),
                    PyRep::Integer(target_id),
                    PyRep::Real(amount_isk),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("addedISK", PyRep::Real(amount_isk)),
                ("totalBountyISK", PyRep::Real(total_pool)),
            ])))
        }

        ("bountyProxy", "GetBounty") | ("bountyProxy", "GetBounties") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if method == "GetBounty" {
                let tid = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
                let amt = with_combat_runtime(ctx, |rt| {
                    rt.player_bounty_pool.get(&tid).copied().unwrap_or(0.0)
                });
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(tid)),
                    ("bountyISK", PyRep::Real(amt)),
                ])));
            }
            let rows = with_combat_runtime(ctx, |rt| {
                rt.player_bounty_pool
                    .iter()
                    .map(|(&tid, &amt)| {
                        PyRep::keyval([
                            ("targetID", PyRep::Integer(tid)),
                            ("bountyISK", PyRep::Real(amt)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "GetMyBounties") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = with_combat_runtime(ctx, |rt| {
                rt.player_bounty_pool
                    .iter()
                    .filter(|(&tid, _)| {
                        tid == char_id
                            || rt
                                .bounty_contributions
                                .get(&tid)
                                .is_some_and(|c| c.iter().any(|&(cid, _)| cid == char_id))
                    })
                    .map(|(&tid, &amt)| {
                        PyRep::keyval([
                            ("targetID", PyRep::Integer(tid)),
                            ("bountyISK", PyRep::Real(amt)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "SearchBounties") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let query_id = args.first().and_then(|a| a.as_i64());
            let query_str = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_lowercase();
            let matching_ids: std::collections::HashSet<i64> = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|c| query_str.is_empty() || c.name.to_lowercase().contains(&query_str))
                    .map(|c| c.character_id)
                    .collect()
            });
            let rows = with_combat_runtime(ctx, |rt| {
                rt.player_bounty_pool
                    .iter()
                    .filter(|(&tid, _)| {
                        if let Some(qid) = query_id {
                            tid == qid
                        } else if !query_str.is_empty() {
                            matching_ids.contains(&tid) || tid.to_string().contains(&query_str)
                        } else {
                            true
                        }
                    })
                    .map(|(&tid, &amt)| {
                        PyRep::keyval([
                            ("targetID", PyRep::Integer(tid)),
                            ("bountyISK", PyRep::Real(amt)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "GetTopPilotBounties")
        | ("bountyProxy", "GetTopCorpBounties")
        | ("bountyProxy", "GetTopAllianceBounties")
        | ("bountyProxy", "GetTopPilotBountyHunters")
        | ("bountyProxy", "GetTopCorporationBountyHunters")
        | ("bountyProxy", "GetTopAllianceBountyHunters")
        | ("bountyProxy", "GetTopBountyHunters") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_hunters = method.contains("Hunter");
            let is_pilot = method.contains("Pilot");
            let is_corp = method.contains("Corp");
            let is_alliance = method.contains("Alliance");
            let mut entries: Vec<(i64, f64)> = with_combat_runtime(ctx, |rt| {
                let src = if is_hunters {
                    &rt.bounty_hunter_claimed
                } else {
                    &rt.player_bounty_pool
                };
                src.iter().map(|(&id, &amt)| (id, amt)).collect()
            });
            entries.retain(|(id, _)| {
                ctx.db.with_snapshot(|snap| {
                    if is_pilot {
                        snap.characters.contains_key(id)
                    } else if is_corp {
                        snap.corporations.contains_key(id)
                    } else if is_alliance {
                        snap.alliances.contains_key(id)
                    } else {
                        true
                    }
                })
            });
            entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let rows = entries
                .into_iter()
                .take(50)
                .map(|(id, amt)| {
                    PyRep::keyval([
                        ("targetID", PyRep::Integer(id)),
                        ("bountyISK", PyRep::Real(amt)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "GMReimburseBounties")
        | ("bountyProxy", "ClearBounty")
        | ("bountyProxy", "GMClearBountyCache") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let set_amt = args.get(1).and_then(|a| a.as_f64());
            let action = if method == "GMReimburseBounties" {
                "reimburse"
            } else if set_amt.is_some() {
                "set"
            } else {
                "clear"
            };
            match gm_admin_bounty_action(ctx, session, action, target_id, set_amt.unwrap_or(0.0)) {
                Ok(amount) => Some(Ok(PyRep::keyval([
                    ("action", PyRep::String(action.into())),
                    ("targetID", PyRep::Integer(target_id)),
                    ("amountISK", PyRep::Real(amount)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // parity: feat:crimewatch.abyssal-suspect-flag-tier
        ("abyssal_content_manager", "ActivateAbyssalKey") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot activate Abyssal filament while docked".into(),
                }));
            }
            let origin_sys = session.solar_system_id.unwrap_or(30_000_142);
            let filament_arg = args.first().and_then(|a| a.as_i64()).unwrap_or(47_765);
            let now_unix = ctx.now_unix();
            let resolved = ctx.db.try_with_mut_snapshot(|snap| {
                let found = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && (i.item_id == filament_arg || i.type_id as i64 == filament_arg)
                            && i.quantity >= 1
                    })
                    .map(|i| (i.item_id, i.type_id));
                let Some((iid, tid)) = found else {
                    return Err(BridgeRpcError {
                        code: "FilamentNotFound".into(),
                        message: format!(
                            "Abyssal filament {filament_arg} not found in character inventory"
                        ),
                    });
                };
                // SDE-driven: group 1979 (Abyssal Filaments), attribute 2761 = tier (0..=6)
                let Some(tier) = ctx.sde.types.get(&tid).and_then(|t| {
                    (t.group_id == 1979)
                        .then(|| t.default_attributes.get(&2761).copied().unwrap_or(1.0) as u8)
                }) else {
                    return Err(BridgeRpcError {
                        code: "InvalidAbyssalFilament".into(),
                        message: format!("Type {tid} is not a valid T0-T6 Abyssal filament"),
                    });
                };
                let origin_sec = ctx
                    .sde
                    .solar_systems
                    .get(&origin_sys)
                    .map(|s| s.security_status)
                    .unwrap_or(0.9);
                if tier >= 4 && origin_sec >= 0.75 {
                    return Err(BridgeRpcError {
                        code: "FilamentDisallowedInHighSec".into(),
                        message: "Activating high-tier (T4-T6) Abyssal filaments is blocked in 0.8 - 1.0 high-security systems".into(),
                    });
                }
                if let Some(item) = snap.items.get_mut(&iid) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&iid);
                    }
                }
                Ok((tid, tier, origin_sec))
            });
            let (filament_type_id, tier, origin_sec) = match resolved {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let suspect_on_exit = tier >= 4 && origin_sec >= 0.45 && origin_sec < 0.75;
            if suspect_on_exit {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                cw.trigger_suspect_flag(char_id, now_unix);
            }
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(32_000_001)),
                ("originSolarSystemID", PyRep::Integer(origin_sys as i64)),
                ("filamentTypeID", PyRep::Integer(filament_type_id as i64)),
                ("tier", PyRep::Integer(tier as i64)),
                ("suspectFlagOnExit", PyRep::Bool(tier >= 4)),
                ("timerSeconds", PyRep::Integer(1_200)),
            ])))
        }

        _ => None,
    }
}
