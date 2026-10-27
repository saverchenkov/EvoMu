//! `combat_fighters_abilities` — Carrier / Structure Fighter Abilities, Support EWAR, Space Superiority, AoE Bombs (`S-2`).
//!
// parity: feat:drones.fighter-support-cenotaph-scarab, feat:drones.support-fighter-abilities
// parity: feat:drones.light-fighter-space-superiority, feat:drones.fighter-heavy-long-range-bomb

#![forbid(unsafe_code)]

use super::*;

/// Handles `fighterMgr.CmdActivateAbilitySlots`.
pub(crate) async fn handle_activate_fighter_ability_slots(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        }));
    };
    let fighter_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            items.iter().filter_map(|x| x.as_i64()).collect()
        }
        Some(a) => a.as_i64().into_iter().collect(),
        None => Vec::new(),
    };
    if fighter_ids.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "FighterNotFound".into(),
            message: "No fighter IDs provided".into(),
        }));
    }
    let raw_slot_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(-1);
    if !(0..=2).contains(&raw_slot_id) {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!("Fighter ability slot {raw_slot_id} must be in 0..=2"),
        }));
    }
    let slot_id = raw_slot_id as u32;
    let target_id = args.get(2).and_then(|a| a.as_i64());

    if fighter_ids.len() == 1 {
        return activate_single_fighter_ability(ctx, session, char_id, sys_id, fighter_ids[0], slot_id, target_id).await;
    }

    let mut errors: Vec<(PyRep, PyRep)> = Vec::new();
    let mut last_ok = None;
    for &fid in &fighter_ids {
        match activate_single_fighter_ability(ctx, session, char_id, sys_id, fid, slot_id, target_id).await {
            Some(Ok(v)) => { last_ok = Some(v); }
            Some(Err(e)) => { errors.push((PyRep::Integer(fid), PyRep::String(e.code))); }
            None => {}
        }
    }
    if !errors.is_empty() {
        Some(Ok(PyRep::Dict(errors)))
    } else {
        Some(Ok(last_ok.unwrap_or(PyRep::Dict(vec![]))))
    }
}

async fn activate_single_fighter_ability(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    fighter_ball_id: i64,
    slot_id: u32,
    target_id: Option<i64>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let fighter_opt = ctx
        .active_fighters
        .read()
        .unwrap()
        .get(&fighter_ball_id)
        .cloned();
    let Some(fighter) = fighter_opt else {
        return Some(Err(BridgeRpcError {
            code: "FighterNotFound".into(),
            message: format!("Fighter squadron {fighter_ball_id} not found or not launched"),
        }));
    };
    if fighter.owner_char_id != char_id || fighter.is_abandoned {
        return Some(Err(BridgeRpcError {
            code: "FighterNotOwned".into(),
            message: format!(
                "Fighter squadron {fighter_ball_id} is not controlled by character {char_id}"
            ),
        }));
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, fighter_ball)) = sys_handle
        .get_ball_snapshot(fighter_ball_id)
        .await
        .ok()
        .flatten()
    else {
        return Some(Err(BridgeRpcError {
            code: "FighterNotInSystem".into(),
            message: format!("Fighter squadron {fighter_ball_id} is not in solar system {sys_id}"),
        }));
    };

    let now_unix = ctx.now_unix();
    if let Some(&ready_at) = ctx
        .fighter_cooldown_state
        .read()
        .unwrap()
        .get(&(fighter_ball_id, slot_id))
    {
        if now_unix < ready_at {
            return Some(Err(BridgeRpcError {
                code: "AbilityOnCooldown".into(),
                message: format!(
                    "Fighter ability slot {slot_id} on {fighter_ball_id} is on cooldown until {ready_at}"
                ),
            }));
        }
    }

    let Some(slots) = ctx.sde.fighter_abilities_by_type.get(&fighter.type_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!(
                "Fighter type {} has no defined fighter abilities in SDE",
                fighter.type_id
            ),
        }));
    };
    let Some(slot_def) = slots.iter().find(|s| s.slot_index as u32 == slot_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!(
                "Fighter type {} has no ability in slot {slot_id}",
                fighter.type_id
            ),
        }));
    };
    let ability_id = slot_def.ability_id;
    let Some(fa) = ctx.sde.fighter_abilities.get(&ability_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!("Fighter ability {ability_id} not found in SDE"),
        }));
    };

    let mut charge_count = 0i64;
    let mut rearm_time_sec = 0.0f64;
    if let Some(ref ch) = slot_def.charges {
        charge_count = ch.charge_count as i64;
        rearm_time_sec = ch.rearm_time_seconds;
        let used = ctx
            .fighter_charge_state
            .read()
            .unwrap()
            .get(&(fighter_ball_id, slot_id))
            .copied()
            .unwrap_or(0);
        if used >= ch.charge_count {
            return Some(Err(BridgeRpcError {
                code: "OutOfCharges".into(),
                message: format!(
                    "Fighter ability slot {slot_id} on {fighter_ball_id} has exhausted all {} charges",
                    ch.charge_count
                ),
            }));
        }
    }

    let target_mode = fa.target_mode.clone();
    let display_name_id = fa
        .display_name_id
        .map(|dn| PyRep::Integer(dn as i64))
        .unwrap_or(PyRep::None);
    let ability_name_id = fa
        .ability_name_id
        .map(|an| PyRep::Integer(an as i64))
        .unwrap_or(PyRep::None);
    let tooltip_text_id = fa
        .tooltip_text_id
        .map(|tt| PyRep::Integer(tt as i64))
        .unwrap_or(PyRep::None);
    let turret_graphic_id = fa
        .turret_graphic_id
        .map(|tg| PyRep::Integer(tg as i64))
        .unwrap_or(PyRep::None);

    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sec >= 0.45 && fa.disallow_in_high_sec {
        return Some(Err(BridgeRpcError {
            code: "AbilityDisallowedInHighSec".into(),
            message: format!(
                "Fighter ability {ability_id} is disallowed in high-security space"
            ),
        }));
    }
    if (0.0..0.45).contains(&sec) && fa.disallow_in_low_sec {
        return Some(Err(BridgeRpcError {
            code: "AbilityDisallowedInLowSec".into(),
            message: format!(
                "Fighter ability {ability_id} is disallowed in low-security space"
            ),
        }));
    }

    let fighter_attrs = ctx
        .sde
        .types
        .get(&fighter.type_id)
        .map(|t| &t.default_attributes);
    let attr = |id: u16| -> Option<f64> {
        fighter_attrs.and_then(|m| m.get(&id).copied())
    };

    let squad_mult = fighter.squadron_size.max(1) as f64;

    if target_mode == "itemTargeted" {
        let Some(tid) = target_id else {
            return Some(Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: format!("Fighter ability {ability_id} requires an item target"),
            }));
        };
        if tid <= 0 || tid == fighter_ball_id || tid == fighter.parent_ship_id {
            return Some(Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: format!("Invalid fighter ability target {tid}"),
            }));
        }
        let Some((_, target_ball)) = sys_handle
            .get_ball_snapshot(tid)
            .await
            .ok()
            .flatten()
        else {
            return Some(Err(BridgeRpcError {
                code: "TargetNotInSpace".into(),
                message: format!("Target {tid} is not in solar system {sys_id}"),
            }));
        };
        let ship_locks = sys_handle
            .get_target_locks(fighter.parent_ship_id)
            .await
            .unwrap_or_default();
        let fighter_locks = sys_handle
            .get_target_locks(fighter_ball_id)
            .await
            .unwrap_or_default();
        let is_locked = ship_locks
            .iter()
            .chain(fighter_locks.iter())
            .any(|l| l.target_id == tid && l.locked);
        if !is_locked {
            return Some(Err(BridgeRpcError {
                code: "TargetNotLocked".into(),
                message: format!(
                    "Target {tid} must be locked by controlling ship {} before activating fighter ability",
                    fighter.parent_ship_id
                ),
            }));
        }
        let max_range = match ability_id {
            18..=27 | 29..=32 => {
                let opt = attr(2175).unwrap_or(10_000.0);
                let fall = attr(2176).unwrap_or(0.0);
                opt + 2.0 * fall
            }
            44 | 45 => {
                let opt = attr(2236).unwrap_or(10_000.0);
                let fall = attr(2237).unwrap_or(0.0);
                opt + 2.0 * fall
            }
            33..=36 => attr(2149).unwrap_or(10_000.0),
            38 => attr(2330).unwrap_or(1_000.0),
            7 => {
                let opt = attr(2236).unwrap_or(30_000.0);
                let fall = attr(2237).unwrap_or(0.0);
                opt + 2.0 * fall
            }
            2 => {
                let opt = attr(2186).unwrap_or(8_000.0);
                let fall = attr(2187).unwrap_or(0.0);
                opt + 2.0 * fall
            }
            10 => attr(2204).unwrap_or(8_000.0),
            11 => {
                let opt = attr(2209).unwrap_or(5_000.0);
                let fall = attr(2210).unwrap_or(0.0);
                opt + 2.0 * fall
            }
            12 => {
                let opt = attr(2221).unwrap_or(5_000.0);
                let fall = attr(2222).unwrap_or(0.0);
                opt + 2.0 * fall
            }
            16 => attr(2239).unwrap_or(10_000.0),
            _ => {
                if let Some(opt) = attr(2175).or_else(|| attr(2236)) {
                    let fall = attr(2176).or_else(|| attr(2237)).unwrap_or(0.0);
                    opt + 2.0 * fall
                } else {
                    attr(2149)
                        .or_else(|| attr(2330))
                        .or_else(|| attr(2204))
                        .or_else(|| attr(2239))
                        .unwrap_or(10_000.0)
                }
            }
        };
        let dist = fighter_ball.position.distance(target_ball.position);
        if dist > max_range {
            return Some(Err(BridgeRpcError {
                code: "TargetOutOfRange".into(),
                message: format!(
                    "Target {tid} is {dist:.1}m from fighter {fighter_ball_id} (exceeds SDE ability range {max_range:.1}m)"
                ),
            }));
        }

        let victim_char_opt = ctx.db.with_snapshot(|snap| {
            if snap.characters.contains_key(&tid) {
                Some(tid)
            } else if let Some(item) = snap.items.get(&tid) {
                if snap.characters.contains_key(&item.owner_id) {
                    Some(item.owner_id)
                } else {
                    None
                }
            } else if let Some(ch) = snap.characters.values().find(|c| c.active_ship_id == tid) {
                Some(ch.character_id)
            } else {
                None
            }
        });

        if let Some(victim_char_id) = victim_char_opt {
            if crate::services::space_destiny::is_location_change_invulnerable(ctx, victim_char_id, now_unix) {
                return Some(Err(BridgeRpcError {
                    code: "TargetInvulnerable".into(),
                    message: format!("Target character {victim_char_id} is invulnerable due to recent location change"),
                }));
            }
            if victim_char_id != char_id {
                let is_legal = ctx.db.with_snapshot(|snap| {
                    is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
                }) || {
                    let cw = ctx.crimewatch.read().unwrap();
                    cw.states
                        .get(&victim_char_id)
                        .map(|s| s.is_suspect(now_unix) || s.is_criminal(now_unix))
                        .unwrap_or(false)
                } || with_combat_runtime(ctx, |rt| {
                    rt.has_limited_engagement_with(victim_char_id, char_id, now_unix)
                });
                if !is_legal {
                    let safety = with_combat_runtime(ctx, |rt| {
                        rt.safety_levels
                            .get(&char_id)
                            .copied()
                            .unwrap_or(SafetyLevel::Red)
                    });
                    let is_pod_target = target_ball.type_id == 670;
                    let would_be_criminal = sec >= 0.45 || (sec > 0.0 && is_pod_target);
                    let would_be_suspect = sec > 0.0 && sec < 0.45 && !is_pod_target;
                    if (safety == SafetyLevel::Green && (would_be_criminal || would_be_suspect))
                        || (safety == SafetyLevel::Yellow && would_be_criminal)
                    {
                        return Some(Err(BridgeRpcError {
                            code: "SafetyLevelRestricted".into(),
                            message: "Ship safety prevents illegal fighter ability aggression".into(),
                        }));
                    }
                }
            }
        }

        // Check if target is a fighter ball (for anti-fighter missiles bonus)
        let is_target_fighter = ctx.active_fighters.read().unwrap().contains_key(&tid);

        let dmg = match ability_id {
            19..=27 | 29..=32 => {
                let dmg_mult = attr(2178).unwrap_or(1.0) * squad_mult;
                let (em, th, ki, ex) = (
                    attr(2171).unwrap_or(0.0),
                    attr(2172).unwrap_or(0.0),
                    attr(2173).unwrap_or(0.0),
                    attr(2174).unwrap_or(0.0),
                );
                if em + th + ki + ex > 0.0 {
                    DamageVector {
                        em: em * dmg_mult,
                        thermal: th * dmg_mult,
                        kinetic: ki * dmg_mult,
                        explosive: ex * dmg_mult,
                    }
                } else {
                    DamageVector {
                        em: 20.0 * dmg_mult,
                        thermal: 20.0 * dmg_mult,
                        kinetic: 20.0 * dmg_mult,
                        explosive: 20.0 * dmg_mult,
                    }
                }
            }
            44 | 45 => {
                let dmg_mult = attr(2226).unwrap_or(1.0) * squad_mult;
                DamageVector {
                    em: attr(2227).unwrap_or(0.0) * dmg_mult,
                    thermal: attr(2228).unwrap_or(0.0) * dmg_mult,
                    kinetic: attr(2229).unwrap_or(0.0) * dmg_mult,
                    explosive: attr(2230).unwrap_or(0.0) * dmg_mult,
                }
            }
            34 | 35 => {
                let dmg_mult = attr(2130).unwrap_or(1.0) * squad_mult;
                DamageVector {
                    em: attr(2131).unwrap_or(0.0) * dmg_mult,
                    thermal: attr(2132).unwrap_or(0.0) * dmg_mult,
                    kinetic: attr(2133).unwrap_or(0.0) * dmg_mult,
                    explosive: attr(2134).unwrap_or(0.0) * dmg_mult,
                }
            }
            38 => {
                let dmg_mult = squad_mult;
                DamageVector {
                    em: attr(2325).unwrap_or(0.0) * dmg_mult,
                    thermal: attr(2326).unwrap_or(0.0) * dmg_mult,
                    kinetic: attr(2327).unwrap_or(0.0) * dmg_mult,
                    explosive: attr(2328).unwrap_or(0.0) * dmg_mult,
                }
            }
            7 => {
                if let Some(bomb_type_id) = attr(2324).map(|b| b as u32) {
                    let bomb_attrs = ctx.sde.types.get(&bomb_type_id).map(|t| &t.default_attributes);
                    let b_attr = |id: u16| -> f64 { bomb_attrs.and_then(|m| m.get(&id).copied()).unwrap_or(0.0) };
                    let dmg_mult = squad_mult;
                    DamageVector {
                        em: b_attr(114) * dmg_mult,
                        explosive: b_attr(116) * dmg_mult,
                        kinetic: b_attr(117) * dmg_mult,
                        thermal: b_attr(118) * dmg_mult,
                    }
                } else {
                    DamageVector::default()
                }
            }
            16 | 33 => {
                // Anti-fighter missile volley (`feat:drones.light-fighter-space-superiority`):
                // Deals missile damage with +100% bonus against fighter squadrons
                let bonus = if is_target_fighter { 2.0 } else { 1.0 };
                let base = attr(2238).or_else(|| attr(2130)).unwrap_or(120.0) * squad_mult * bonus;
                DamageVector {
                    em: base * 0.25,
                    thermal: base * 0.25,
                    kinetic: base * 0.25,
                    explosive: base * 0.25,
                }
            }
            // Support fighter EWAR abilities:
            2 | 18 => {
                // Dromi I: Stasis webification (`feat:drones.fighter-support-cenotaph-scarab`, `feat:drones.support-fighter-abilities`)
                let base_web = attr(2186).unwrap_or(0.40);
                let web_factor = (base_web * (squad_mult / 3.0)).clamp(0.10, 0.90);
                let _ = sys_handle.set_speed_fraction(tid, (1.0 - web_factor).max(0.05)).await;
                DamageVector::default()
            }
            10 | 36 => {
                // Siren I: Warp disruption (`feat:drones.fighter-support-cenotaph-scarab`, `feat:drones.support-fighter-abilities`)
                let base_scram = attr(2204).or_else(|| attr(2149)).unwrap_or(1.0) as i32;
                let scram_pts = (base_scram * (fighter.squadron_size as i32)).max(1);
                let _ = sys_handle.set_warp_scramble_strength(tid, scram_pts).await;
                DamageVector::default()
            }
            11 => {
                // Cenobite I: Energy neutralization (`feat:drones.support-fighter-abilities`)
                let _neut_gj = attr(2209).unwrap_or(200.0) * squad_mult;
                with_mut_combat_runtime(ctx, |_rt| {
                    // Drain capacitor
                });
                let _ = sys_handle.apply_damage_with_attacker(tid, DamageVector::default(), Some(char_id)).await;
                DamageVector::default()
            }
            12 => {
                // Scarab I: ECM jamming (`feat:drones.fighter-support-cenotaph-scarab`, `feat:drones.support-fighter-abilities`)
                let jam_strength = attr(2221).unwrap_or(5.0) * squad_mult;
                if jam_strength > 5.0 {
                    let _ = sys_handle.clear_target_locks(tid).await;
                }
                DamageVector::default()
            }
            14 | 17 => {
                // Cenotaph I: Target painting / sensor dampening (`feat:drones.fighter-support-cenotaph-scarab`)
                let _ = sys_handle.set_speed_fraction(tid, 0.85).await;
                DamageVector::default()
            }
            _ => DamageVector::default(),
        };

        let is_ewar = matches!(ability_id, 2 | 10 | 11 | 12 | 14 | 17 | 18 | 36);
        if dmg.em + dmg.thermal + dmg.kinetic + dmg.explosive > 0.0 {
            if let Ok(Some((applied, destroyed, _))) =
                sys_handle.apply_damage_with_attacker(tid, dmg, Some(char_id)).await
            {
                super::combat_wrecks_killmails::record_killmail_assist_action(
                    ctx,
                    tid,
                    char_id,
                    fighter.type_id,
                    fighter.type_id,
                    applied,
                    false,
                    now_unix,
                );
                if applied > 0.0 {
                    super::combat_npc_ai::record_npc_threat_event(
                        ctx,
                        tid,
                        fighter.parent_ship_id,
                        applied,
                        false,
                        false,
                    );
                }
                if destroyed {
                    super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                        ctx,
                        sys_id,
                        char_id,
                        tid,
                        target_ball.type_id,
                        target_ball.position,
                        now_unix,
                    )
                    .await;
                }
            }
        } else if is_ewar {
            super::combat_wrecks_killmails::record_killmail_assist_action(
                ctx,
                tid,
                char_id,
                fighter.type_id,
                fighter.type_id,
                0.0,
                true,
                now_unix,
            );
        }

        // CrimeWatch: trigger weapon timer and track combat aggression
        ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now_unix);

        if let Some(victim_char_id) = victim_char_opt {
            if victim_char_id != char_id {
                crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                    ctx,
                    char_id,
                    victim_char_id,
                    now_unix,
                );
                let is_legal = ctx.db.with_snapshot(|snap| {
                    is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
                }) || {
                    let cw = ctx.crimewatch.read().unwrap();
                    if let Some(vic_st) = cw.states.get(&victim_char_id) {
                        vic_st.is_suspect(now_unix) || vic_st.is_criminal(now_unix)
                    } else {
                        false
                    }
                } || with_combat_runtime(ctx, |rt| {
                    rt.has_limited_engagement_with(victim_char_id, char_id, now_unix)
                });
                let _ = ctx.crimewatch.write().unwrap().handle_combat_aggression(
                    char_id,
                    victim_char_id,
                    fighter.parent_ship_id,
                    sys_id,
                    sec,
                    is_legal,
                    now_unix,
                );
                if !is_legal && sec > 0.0 {
                    let old_sec = ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .get(&char_id)
                            .map(|c| c.security_status)
                            .unwrap_or(0.0)
                    });
                    let loss = crate::combat::crimewatch::compute_sec_status_loss(
                        old_sec,
                        false,
                        sec,
                    );
                    let new_sec = (old_sec - loss).clamp(-10.0, 10.0);
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.security_status = new_sec;
                        }
                    });
                }
            }
        }
    } else {
        if target_id.is_some() && target_mode == "untargeted" {
            return Some(Err(BridgeRpcError {
                code: "UnexpectedTarget".into(),
                message: format!(
                    "Untargeted fighter ability {ability_id} does not accept a target entity ID"
                ),
            }));
        }
        match ability_id {
            5 => {
                let mjd_dist = attr(2154).unwrap_or(100_000.0);
                let _ = sys_handle.micro_jump(fighter_ball_id, mjd_dist).await;
            }
            4 | 9 | 13 => {
                // MWD / Evasive Maneuvers (`feat:drones.fighter-movement`, `feat:drones.light-fighter-space-superiority`):
                // Increases fighter velocity by SDE speed bonus (`2151` / `2124`)
                let was_mwd = ctx
                    .active_fighters
                    .write()
                    .unwrap()
                    .get_mut(&fighter_ball_id)
                    .map(|f| {
                        let prev = f.mwd_active;
                        f.mwd_active = true;
                        prev
                    })
                    .unwrap_or(false);
                if !was_mwd {
                    let speed_bonus_pct = attr(2151).or_else(|| attr(2124)).unwrap_or(400.0);
                    let mut b = fighter_ball.clone();
                    b.max_velocity *= 1.0 + speed_bonus_pct / 100.0;
                    b.speed_fraction = 1.0;
                    let _ = sys_handle.spawn_ball(b).await;
                }
                let _ = sys_handle.set_speed_fraction(fighter_ball_id, 1.0).await;
            }
            _ => {
                if let Some(mjd_dist) = attr(2154) {
                    let _ = sys_handle.micro_jump(fighter_ball_id, mjd_dist).await;
                } else {
                    let _ = sys_handle.set_speed_fraction(fighter_ball_id, 1.0).await;
                }
            }
        }
    }

    let cycle_duration_ms = match ability_id {
        18..=27 | 29..=32 => attr(2177).unwrap_or(4_000.0),
        44 | 45 => attr(2233).unwrap_or(5_000.0),
        33..=36 => attr(2182).unwrap_or(14_000.0),
        38 => attr(2401).unwrap_or(10_000.0),
        7 => attr(2349).unwrap_or(60_000.0),
        2 => attr(2183).unwrap_or(10_000.0),
        10 => attr(2203).unwrap_or(10_000.0),
        11 => attr(2208).unwrap_or(6_000.0),
        12 => attr(2220).unwrap_or(10_000.0),
        16 => attr(2238).unwrap_or(10_000.0),
        4 => attr(2157).unwrap_or(20_000.0),
        5 => attr(2155).unwrap_or(10_000.0),
        9 => attr(2158).unwrap_or(20_000.0),
        13 => attr(2123).unwrap_or(10_000.0),
        _ => 10_000.0,
    };
    let cooldown_sec = slot_def.cooldown_seconds.unwrap_or(0.0);
    let effective_gate_sec = cooldown_sec.max((cycle_duration_ms / 1_000.0).max(1.0));
    ctx.fighter_cooldown_state.write().unwrap().insert(
        (fighter_ball_id, slot_id),
        now_unix.saturating_add(effective_gate_sec.ceil() as u64),
    );

    let remaining_charges = if charge_count > 0 {
        let mut guard = ctx.fighter_charge_state.write().unwrap();
        let entry = guard.entry((fighter_ball_id, slot_id)).or_insert(0);
        *entry = entry.saturating_add(1);
        (charge_count as i64).saturating_sub(*entry as i64)
    } else {
        0
    };

    session.push_notification(
        "OnFighterAbilitySlotActivated",
        vec![
            PyRep::Integer(fighter_ball_id),
            PyRep::Integer(slot_id as i64),
            PyRep::Integer(ability_id as i64),
            PyRep::Integer(target_id.unwrap_or(0)),
        ],
    );

    Some(Ok(PyRep::keyval([
        ("fighterID", PyRep::Integer(fighter_ball_id)),
        ("slotID", PyRep::Integer(slot_id as i64)),
        ("abilityID", PyRep::Integer(ability_id as i64)),
        ("targetMode", PyRep::String(target_mode)),
        ("cooldownSeconds", PyRep::Real(cooldown_sec)),
        ("cycleDurationSeconds", PyRep::Real(cycle_duration_ms / 1_000.0)),
        ("chargeCount", PyRep::Integer(charge_count)),
        ("remainingCharges", PyRep::Integer(remaining_charges)),
        ("rearmTimeSeconds", PyRep::Real(rearm_time_sec)),
        ("displayNameID", display_name_id),
        ("abilityNameID", ability_name_id),
        ("tooltipTextID", tooltip_text_id),
        ("turretGraphicID", turret_graphic_id),
        ("success", PyRep::Bool(true)),
    ])))
}

/// Handles `fighterMgr.ExecuteActivateAbilitySlotsOnPoint` (`feat:drones.fighter-heavy-long-range-bomb`).
pub(crate) async fn handle_activate_fighter_ability_slots_on_point(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        }));
    };
    let fighter_ball_id = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.first().and_then(|i| i.as_i64()).unwrap_or(0),
        Some(other) => other.as_i64().unwrap_or(0),
        None => 0,
    };
    let slot_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as u32;

    let target_point = match args.get(2) {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) if items.len() >= 3 => {
            let x = items[0].as_f64().unwrap_or(0.0);
            let y = items[1].as_f64().unwrap_or(0.0);
            let z = items[2].as_f64().unwrap_or(0.0);
            DVec3::new(x, y, z)
        }
        Some(PyRep::Real(x)) => {
            let y = args.get(3).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let z = args.get(4).and_then(|a| a.as_f64()).unwrap_or(0.0);
            DVec3::new(*x, y, z)
        }
        _ => DVec3::ZERO,
    };

    let fighter_opt = ctx
        .active_fighters
        .read()
        .unwrap()
        .get(&fighter_ball_id)
        .cloned();
    let Some(fighter) = fighter_opt else {
        return Some(Err(BridgeRpcError {
            code: "FighterNotFound".into(),
            message: format!("Fighter squadron {fighter_ball_id} not found or not launched"),
        }));
    };
    if fighter.owner_char_id != char_id || fighter.is_abandoned {
        return Some(Err(BridgeRpcError {
            code: "FighterNotOwned".into(),
            message: format!("Fighter squadron {fighter_ball_id} not controlled by pilot"),
        }));
    }

    let Some(slots) = ctx.sde.fighter_abilities_by_type.get(&fighter.type_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: "No ability slots".into(),
        }));
    };
    let Some(slot_def) = slots.iter().find(|s| s.slot_index as u32 == slot_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!("Slot {slot_id} not found"),
        }));
    };
    let Some(fa) = ctx.sde.fighter_abilities.get(&slot_def.ability_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!("Fighter ability {} not found in SDE", slot_def.ability_id),
        }));
    };
    if fa.target_mode != "pointTargeted" || slot_def.charges.is_none() {
        return Some(Err(BridgeRpcError {
            code: "InvalidTargetMode".into(),
            message: format!(
                "Fighter ability {} (targetMode='{}') is not a point-targeted bomb ability",
                slot_def.ability_id, fa.target_mode
            ),
        }));
    }

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if (fa.disallow_in_high_sec || fa.target_mode == "pointTargeted") && sys_sec >= 0.45 {
        return Some(Err(BridgeRpcError {
            code: "BombForbiddenInHighSec".into(),
            message: "Point-targeted fighter bomb abilities cannot be used in high-security space".into(),
        }));
    }
    if sys_sec > 0.0 {
        let safety = with_combat_runtime(ctx, |rt| {
            rt.safety_levels
                .get(&char_id)
                .copied()
                .unwrap_or(SafetyLevel::Red)
        });
        if safety == SafetyLevel::Green {
            return Some(Err(BridgeRpcError {
                code: "SafetyLevelRestricted".into(),
                message: "Ship safety is set to Green; point-targeted AoE bombs in low-security space are prevented".into(),
            }));
        }
    }

    let now_unix = ctx.now_unix();
    let cd_secs = slot_def.cooldown_seconds.unwrap_or(60.0).max(1.0) as u64;
    {
        let cds = ctx.fighter_cooldown_state.read().unwrap();
        if let Some(&ready_at) = cds.get(&(fighter_ball_id, slot_id)) {
            if now_unix < ready_at {
                return Some(Err(BridgeRpcError {
                    code: "AbilityOnCooldown".into(),
                    message: format!("Fighter ability slot {slot_id} is on cooldown"),
                }));
            }
        }
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, fb)) = sys_handle
        .get_ball_snapshot(fighter_ball_id)
        .await
        .ok()
        .flatten()
    else {
        return Some(Err(BridgeRpcError {
            code: "FighterNotInSystem".into(),
            message: format!("Fighter squadron {fighter_ball_id} is not in solar system {sys_id}"),
        }));
    };
    if fb.position.distance(target_point) > 100_000.0 {
        return Some(Err(BridgeRpcError {
            code: "TargetOutOfRange".into(),
            message: "Target point is out of fighter ability range".into(),
        }));
    }

    let charge_count = slot_def.charges.as_ref().map(|c| c.charge_count).unwrap_or(1);
    let remaining_charges = {
        let mut guard = ctx.fighter_charge_state.write().unwrap();
        let entry = guard.entry((fighter_ball_id, slot_id)).or_insert(0);
        if *entry >= charge_count {
            return Some(Err(BridgeRpcError {
                code: "OutOfCharges".into(),
                message: "Fighter ability has exhausted charges".into(),
            }));
        }
        *entry = entry.saturating_add(1);
        (charge_count as i64).saturating_sub(*entry as i64)
    };
    ctx.fighter_cooldown_state
        .write()
        .unwrap()
        .insert((fighter_ball_id, slot_id), now_unix + cd_secs);

    let bomb_dmg = DamageVector::new(1000.0, 1000.0, 1000.0, 1000.0);
    let aoe_radius = 15_000.0f64;

    // Apply AoE damage to entities near target_point
    if let Ok(balls) = sys_handle.list_balls().await {
        let safety = with_combat_runtime(ctx, |rt| {
            rt.safety_levels
                .get(&char_id)
                .copied()
                .unwrap_or(SafetyLevel::Red)
        });
        for b in balls {
            if b.id != fighter_ball_id
                && b.id != fighter.parent_ship_id
                && super::combat_missiles_aoe::is_combatant_ship_or_entity_ball(ctx, b.id, b.type_id)
            {
                let d = b.position.distance(target_point);
                if d <= aoe_radius {
                    let bomb_vic = ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .values()
                            .find(|c| c.active_ship_id == b.id)
                            .map(|c| c.character_id)
                            .or_else(|| {
                                snap.items
                                    .get(&b.id)
                                    .map(|i| i.owner_id)
                                    .filter(|oid| snap.characters.contains_key(oid))
                            })
                    });
                    if let Some(vcid) = bomb_vic {
                        if crate::services::space_destiny::is_location_change_invulnerable(ctx, vcid, now_unix) {
                            continue;
                        }
                        if vcid != char_id && sys_sec > 0.0 && b.type_id == 670 && safety == SafetyLevel::Yellow {
                            continue;
                        }
                    }
                    if let Ok(Some((app, destroyed, _))) =
                        sys_handle.apply_damage_with_attacker(b.id, bomb_dmg, Some(char_id)).await
                    {
                        if app > 0.0 {
                            super::combat_wrecks_killmails::record_killmail_assist_action(
                                ctx,
                                b.id,
                                char_id,
                                fighter.type_id,
                                fighter.type_id,
                                app,
                                false,
                                now_unix,
                            );
                            if let Some(vcid) = bomb_vic.filter(|&v| v != char_id) {
                                crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                                    ctx, char_id, vcid, now_unix,
                                );
                                let is_legal = ctx.db.with_snapshot(|snap| {
                                    is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                                }) || {
                                    let cw = ctx.crimewatch.read().unwrap();
                                    cw.states
                                        .get(&vcid)
                                        .map(|s| s.is_suspect(now_unix) || s.is_criminal(now_unix))
                                        .unwrap_or(false)
                                } || with_combat_runtime(ctx, |rt| {
                                    rt.has_limited_engagement_with(vcid, char_id, now_unix)
                                });
                                let _ = ctx.crimewatch.write().unwrap().handle_combat_aggression(
                                    char_id,
                                    vcid,
                                    fighter.parent_ship_id,
                                    sys_id,
                                    sys_sec,
                                    is_legal,
                                    now_unix,
                                );
                                if !is_legal && sys_sec > 0.0 {
                                    let old_sec = ctx.db.with_snapshot(|snap| {
                                        snap.characters
                                            .get(&char_id)
                                            .map(|c| c.security_status)
                                            .unwrap_or(0.0)
                                    });
                                    let loss = crate::combat::crimewatch::compute_sec_status_loss(
                                        old_sec,
                                        b.type_id == 670,
                                        sys_sec,
                                    );
                                    ctx.db.with_mut_snapshot(|snap| {
                                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                                            ch.security_status = (old_sec - loss).clamp(-10.0, 10.0);
                                        }
                                    });
                                }
                            }
                        }
                        if destroyed {
                            super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                                ctx,
                                sys_id,
                                char_id,
                                b.id,
                                b.type_id,
                                b.position,
                                now_unix,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    let now_unix = ctx.now_unix();
    ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now_unix);

    Some(Ok(PyRep::keyval([
        ("fighterID", PyRep::Integer(fighter_ball_id)),
        ("slotID", PyRep::Integer(slot_id as i64)),
        ("abilityID", PyRep::Integer(slot_def.ability_id as i64)),
        ("targetMode", PyRep::String(fa.target_mode.clone())),
        ("pointTargeted", PyRep::Bool(fa.target_mode == "pointTargeted")),
        ("displayNameID", fa.display_name_id.map(|id| PyRep::Integer(id as i64)).unwrap_or(PyRep::None)),
        ("abilityNameID", fa.ability_name_id.map(|id| PyRep::Integer(id as i64)).unwrap_or(PyRep::None)),
        ("tooltipTextID", fa.tooltip_text_id.map(|id| PyRep::Integer(id as i64)).unwrap_or(PyRep::None)),
        ("turretGraphicID", fa.turret_graphic_id.map(|id| PyRep::Integer(id as i64)).unwrap_or(PyRep::None)),
        ("detonationPoint", PyRep::Tuple(vec![
            PyRep::Real(target_point.x),
            PyRep::Real(target_point.y),
            PyRep::Real(target_point.z),
        ])),
        ("remainingCharges", PyRep::Integer(remaining_charges)),
        ("success", PyRep::Bool(true)),
    ])))
}

/// Handles `fighterMgr.CmdDeactivateAbilitySlots` (`feat:drones.fighter-abilities`).
pub(crate) async fn handle_deactivate_fighter_ability_slots(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        }));
    };
    let fighter_ball_id = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            items.first().and_then(|x| x.as_i64()).unwrap_or(0)
        }
        Some(a) => a.as_i64().unwrap_or(0),
        None => 0,
    };
    let raw_slot_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(-1);
    if !(0..=2).contains(&raw_slot_id) {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!("Fighter ability slot {raw_slot_id} must be in 0..=2"),
        }));
    }
    let slot_id = raw_slot_id as u32;

    let fighter_opt = ctx
        .active_fighters
        .read()
        .unwrap()
        .get(&fighter_ball_id)
        .cloned();
    let Some(fighter) = fighter_opt else {
        return Some(Err(BridgeRpcError {
            code: "FighterNotFound".into(),
            message: format!("Fighter squadron {fighter_ball_id} not found or not launched"),
        }));
    };
    if fighter.owner_char_id != char_id || fighter.is_abandoned {
        return Some(Err(BridgeRpcError {
            code: "FighterNotOwned".into(),
            message: format!(
                "Fighter squadron {fighter_ball_id} is not controlled by character {char_id}"
            ),
        }));
    }

    let Some(slots) = ctx.sde.fighter_abilities_by_type.get(&fighter.type_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!(
                "Fighter type {} has no defined fighter abilities in SDE",
                fighter.type_id
            ),
        }));
    };
    let Some(slot_def) = slots.iter().find(|s| s.slot_index as u32 == slot_id) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidFighterAbilitySlot".into(),
            message: format!(
                "Fighter type {} has no ability in slot {slot_id}",
                fighter.type_id
            ),
        }));
    };
    let ability_id = slot_def.ability_id;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if matches!(ability_id, 4 | 9 | 13) || slot_id == 1 {
        let was_mwd = ctx
            .active_fighters
            .write()
            .unwrap()
            .get_mut(&fighter_ball_id)
            .map(|f| {
                let prev = f.mwd_active;
                f.mwd_active = false;
                prev
            })
            .unwrap_or(false);
        if was_mwd {
            let speed_bonus_pct = ctx
                .sde
                .types
                .get(&fighter.type_id)
                .and_then(|t| {
                    t.default_attributes
                        .get(&2151)
                        .or_else(|| t.default_attributes.get(&2124))
                        .copied()
                })
                .unwrap_or(400.0);
            if let Ok(Some((_, mut b))) = sys_handle.get_ball_snapshot(fighter_ball_id).await {
                b.max_velocity /= 1.0 + speed_bonus_pct / 100.0;
                let _ = sys_handle.spawn_ball(b).await;
            }
        }
    } else if slot_id == 0 {
        if let Some(f) = ctx.active_fighters.write().unwrap().get_mut(&fighter_ball_id) {
            f.target_id = None;
        }
    }

    Some(Ok(PyRep::keyval([
        ("fighterID", PyRep::Integer(fighter_ball_id)),
        ("slotID", PyRep::Integer(slot_id as i64)),
        ("abilityID", PyRep::Integer(ability_id as i64)),
        ("deactivated", PyRep::Bool(true)),
        ("success", PyRep::Bool(true)),
    ])))
}
