//! `combat_crimewatch` — Domain 6: Weapons, EWAR, Drones/Fighters, CrimeWatch 2.0, CONCORD, Killmails, & DBS/ESS (`§3.6`).

#![forbid(unsafe_code)]

use crate::formulas::{
    breacher_pod_tick_damage, compute_dbs_and_ess_bounty_split, concord_response_delay_sec,
    entropic_spool_multiplier, missile_damage_factor, turret_chance_to_hit, turret_shot_damage,
    vorton_arc_targets_with_cap,
};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillRightRecord, KillmailRecord};
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

fn is_legal_war_or_suspect_target(
    snap: &eve_db::WorldStateSnapshot,
    attacker_char_id: i64,
    victim_char_id: i64,
    now_unix: u64,
) -> bool {
    let has_suspect = snap
        .kill_rights
        .values()
        .any(|kr| kr.target_id == victim_char_id && kr.is_active_suspect);
    if has_suspect {
        return true;
    }
    let (Some(att), Some(vic)) = (
        snap.characters.get(&attacker_char_id),
        snap.characters.get(&victim_char_id),
    ) else {
        return false;
    };
    let att_entity = att.alliance_id.unwrap_or(att.corporation_id);
    let vic_entity = vic.alliance_id.unwrap_or(vic.corporation_id);
    snap.wars.values().any(|w| {
        w.active
            && now_unix >= w.started_unix.saturating_add(86_400)
            && ((w.aggressor_corp_id == att_entity && w.defender_corp_id == vic_entity)
                || (w.aggressor_corp_id == vic_entity && w.defender_corp_id == att_entity)
                || (w.aggressor_corp_id == att.corporation_id
                    && w.defender_corp_id == vic.corporation_id)
                || (w.aggressor_corp_id == vic.corporation_id
                    && w.defender_corp_id == att.corporation_id))
    })
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // Dogma Phase 4 Exit Gate: dogmaIM.FireWeapon(ship_id, target_id) is deleted in favor of dogmaIM.Activate
        ("dogmaIM", "FireWeapon") if args.len() >= 2 => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "dogmaIM.FireWeapon is deleted; use dogmaIM.Activate".into(),
            }))
        }

        // Turret / Missile / Vorton / Entropic / Breacher Combat Engagement & CrimeWatch 2.0 Enforcement
        ("dogmaIM", "FireWeapon") | ("beyonce", "CmdAttackTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot fire weapons while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: "FireWeapon requires an explicit integer targetID".into(),
                }));
            };
            if target_id <= 0 || target_id == char_id || target_id == ship_id {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: "Invalid or self combat target".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);

            // Require the attacker's ship ball to exist in the current solar system's Ballpark
            let Some((attacker_tick, attacker_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Attacker ship {ship_id} is not spawned in space"),
                }));
            };

            // Require a fitted, online high-slot offensive weapon (flag 27..=34) on the active ship
            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let requested_mod_or_flag = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("moduleID").and_then(|v| v.as_i64()))
                .or_else(|| kwargs.get("flagID").and_then(|v| v.as_i64()));
            let is_weapon_group = |group_id: u32| -> bool {
                matches!(
                    group_id,
                    53 | 55 | 74 | 506..=511 | 1986 | 4060 | 4500 | 4805 | 4807
                )
            };
            let mut online_weapons: Vec<&eve_dogma::FittedModule> = fit
                .modules
                .values()
                .filter(|m| {
                    (27..=34).contains(&m.flag_id)
                        && m.state != eve_dogma::ModuleState::Offline
                        && ctx
                            .sde
                            .types
                            .get(&m.type_id)
                            .map(|t| is_weapon_group(t.group_id))
                            .unwrap_or(false)
                        && requested_mod_or_flag
                            .map(|req| m.item_id == req || (m.flag_id as i64) == req)
                            .unwrap_or(true)
                })
                .collect();
            online_weapons.sort_by_key(|m| (m.flag_id, m.item_id));
            if online_weapons.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "NoWeaponFitted".into(),
                    message: format!(
                        "Active ship {ship_id} has no fitted online high-slot weapon module (flags 27..=34)"
                    ),
                }));
            }
            let mut armed_weapon: Option<(&eve_dogma::FittedModule, &eve_sde::InvType, i64, u32)> =
                None;
            let mut skill_err: Option<BridgeRpcError> = None;
            ctx.db.with_snapshot(|snap| {
                for m in &online_weapons {
                    let Some(wt) = ctx.sde.types.get(&m.type_id) else {
                        continue;
                    };
                    let loaded_opt = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.quantity > 0
                                && ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .map(|ct| {
                                        ServerContext::is_charge_compatible_with_weapon(wt, ct)
                                    })
                                    .unwrap_or(false)
                        })
                        .map(|i| (i.item_id, i.type_id));
                    if let Some((cid, ctid)) = loaded_opt {
                        match ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid) {
                            Ok(()) => {
                                armed_weapon = Some((*m, wt, cid, ctid));
                                break;
                            }
                            Err(e) => {
                                if skill_err.is_none() {
                                    skill_err = Some(e);
                                }
                            }
                        }
                    }
                }
            });
            if armed_weapon.is_none() {
                if let Some(e) = skill_err {
                    return Some(Err(e));
                }
                let first_wt = online_weapons[0];
                let wt_name = ctx
                    .sde
                    .types
                    .get(&first_wt.type_id)
                    .map(|t| t.name.as_str())
                    .unwrap_or("weapon");
                return Some(Err(BridgeRpcError {
                    code: "NoAmmoLoaded".into(),
                    message: format!(
                        "Weapon {} in slot {} on ship {ship_id} has no compatible charge loaded in its weapon slot",
                        wt_name, first_wt.flag_id
                    ),
                }));
            }
            let (_weapon_mod, weapon_type, loaded_charge_item_id, charge_tid) =
                armed_weapon.unwrap();

            let weapon_kind = match weapon_type.group_id {
                506..=511 => "MissileLauncher",
                1986 => "EntropicDisintegrator",
                4060 => "VortonProjector",
                4500 | 4805 | 4807 => "BreacherPod",
                _ => "ProjectileTurret",
            };
            let dmg_mult = weapon_type
                .default_attributes
                .get(&64)
                .copied()
                .unwrap_or(1.0);
            let charge_type = &ctx.sde.types[&charge_tid];
            let base_dmg = DamageVector {
                em: charge_type
                    .default_attributes
                    .get(&114)
                    .copied()
                    .unwrap_or(0.0)
                    * dmg_mult,
                explosive: charge_type
                    .default_attributes
                    .get(&116)
                    .copied()
                    .unwrap_or(0.0)
                    * dmg_mult,
                kinetic: charge_type
                    .default_attributes
                    .get(&117)
                    .copied()
                    .unwrap_or(0.0)
                    * dmg_mult,
                thermal: charge_type
                    .default_attributes
                    .get(&118)
                    .copied()
                    .unwrap_or(0.0)
                    * dmg_mult,
            };
            let (
                optimal_range,
                falloff_range,
                tracking_speed,
                missile_exp_radius,
                missile_exp_velocity,
                missile_drf,
            ) = if weapon_kind == "MissileLauncher" || weapon_kind == "BreacherPod" {
                let missile_vel = charge_type
                    .default_attributes
                    .get(&37)
                    .copied()
                    .unwrap_or(3_750.0);
                let flight_ms = charge_type
                    .default_attributes
                    .get(&281)
                    .copied()
                    .unwrap_or(5_000.0);
                let exp_vel = charge_type
                    .default_attributes
                    .get(&653)
                    .copied()
                    .unwrap_or(170.0);
                let exp_rad = charge_type
                    .default_attributes
                    .get(&654)
                    .copied()
                    .unwrap_or(40.0);
                let drf = charge_type
                    .default_attributes
                    .get(&1353)
                    .copied()
                    .unwrap_or(0.604);
                (
                    missile_vel * (flight_ms / 1_000.0),
                    0.0,
                    100.0,
                    exp_rad,
                    exp_vel,
                    drf,
                )
            } else if weapon_kind == "VortonProjector" {
                let opt = weapon_type
                    .default_attributes
                    .get(&54)
                    .copied()
                    .unwrap_or(22_460.0);
                let exp_vel = weapon_type
                    .default_attributes
                    .get(&653)
                    .or_else(|| charge_type.default_attributes.get(&653))
                    .copied()
                    .unwrap_or(190.0);
                let exp_rad = weapon_type
                    .default_attributes
                    .get(&654)
                    .or_else(|| charge_type.default_attributes.get(&654))
                    .copied()
                    .unwrap_or(43.0);
                // eve-wiki/articles/Missile_mechanics.md:209-211 specifies Small Vorton Projector DRF (1353) = 0.40
                let drf = weapon_type
                    .default_attributes
                    .get(&1353)
                    .or_else(|| charge_type.default_attributes.get(&1353))
                    .copied()
                    .unwrap_or(0.40);
                (opt, 0.0, 0.0, exp_rad, exp_vel, drf)
            } else {
                let opt = weapon_type
                    .default_attributes
                    .get(&54)
                    .copied()
                    .unwrap_or(5_000.0);
                let falloff = weapon_type
                    .default_attributes
                    .get(&158)
                    .copied()
                    .unwrap_or(5_000.0);
                let track = weapon_type
                    .default_attributes
                    .get(&160)
                    .copied()
                    .unwrap_or(100.0);
                (opt, falloff, track, 40.0, 170.0, 0.604)
            };

            // Resolve whether `target_id` is a player character ID or a player's active ship ID
            let player_target_info = ctx.db.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&target_id) {
                    Some((ch.character_id, ch.active_ship_id))
                } else {
                    snap.characters
                        .values()
                        .find(|c| c.active_ship_id == target_id)
                        .map(|c| (c.character_id, c.active_ship_id))
                }
            });
            let target_ball_id = player_target_info
                .map(|(_, s_id)| s_id)
                .unwrap_or(target_id);

            // Require target ball (NPC or player's active ship) to exist in the current solar system's Ballpark
            let Some((_, target_ball)) = (match sys_handle.get_ball_snapshot(target_ball_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: format!(
                        "Target {target_id} (ball {target_ball_id}) is not present in space in system {sys_id}"
                    ),
                }));
            };

            let active_convoy_opt = ctx
                .living_world
                .get_convoy(target_ball_id)
                .filter(|c| !c.destroyed);
            let npc_base_bounty = if player_target_info.is_none() && active_convoy_opt.is_none() {
                let Some(t) = ctx.sde.types.get(&target_ball.type_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "TargetInvulnerable".into(),
                        message: format!(
                            "Target ball {target_ball_id} (type {}) is not a valid combat target",
                            target_ball.type_id
                        ),
                    }));
                };
                let bounty = t
                    .default_attributes
                    .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                    .copied()
                    .unwrap_or(0.0);
                if t.category_id != 11 || bounty <= 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "TargetInvulnerable".into(),
                        message: format!(
                            "Target ball {target_ball_id} ({}) is invulnerable to direct weapon fire",
                            t.name
                        ),
                    }));
                }
                let belongs_to_other_mission = ctx.db.with_snapshot(|snap| {
                    snap.active_missions.iter().any(|((m_char_id, _), m)| {
                        *m_char_id != char_id
                            && m.state == "Accepted"
                            && m.spawned_npc_ball_ids.contains(&target_ball_id)
                    })
                });
                if belongs_to_other_mission {
                    return Some(Err(BridgeRpcError {
                        code: "MissionPocketInstanceRestricted".into(),
                        message: format!(
                            "Target NPC {target_ball_id} is inside another pilot's instanced mission pocket"
                        ),
                    }));
                }
                bounty
            } else {
                0.0
            };

            let dist = attacker_ball.position.distance(target_ball.position);
            if dist > optimal_range + 3.0 * falloff_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetOutOfRange".into(),
                    message: format!(
                        "Target {target_id} is {:.1} m away (exceeds weapon range {:.1} m)",
                        dist,
                        optimal_range + 3.0 * falloff_range
                    ),
                }));
            }

            // Pre-check weapon rate-of-fire cycle ticks (SDE attr 51 `speed` in ms) BEFORE consuming ammo
            let rof_ms = weapon_type
                .default_attributes
                .get(&51)
                .copied()
                .unwrap_or(1_000.0);
            let cycle_ticks = (rof_ms / 1_000.0).floor().max(1.0) as u64;
            {
                let wguard = ctx
                    .weapon_cycle_state
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let (last_tick, _) = wguard.get(&ship_id).copied().unwrap_or((None, 0));
                if let Some(prev_tick) = last_tick {
                    if attacker_tick >= prev_tick
                        && attacker_tick < prev_tick.saturating_add(cycle_ticks)
                    {
                        return Some(Err(BridgeRpcError {
                            code: "WeaponCycling".into(),
                            message: format!(
                                "Weapon on ship {ship_id} is cycling until tick {} (current tick {attacker_tick})",
                                prev_tick.saturating_add(cycle_ticks)
                            ),
                        }));
                    }
                }
            }

            // Consume 1 round of loaded charge from the weapon slot inside a transactional snapshot
            if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch_item) = snap.items.get_mut(&loaded_charge_item_id) else {
                    return Err(BridgeRpcError {
                        code: "NoAmmoLoaded".into(),
                        message: format!(
                            "Loaded charge item {loaded_charge_item_id} on ship {ship_id} is depleted"
                        ),
                    });
                };
                if ch_item.quantity <= 0 {
                    snap.items.remove(&loaded_charge_item_id);
                    return Err(BridgeRpcError {
                        code: "NoAmmoLoaded".into(),
                        message: format!(
                            "Loaded charge item {loaded_charge_item_id} on ship {ship_id} is depleted"
                        ),
                    });
                };
                if ch_item.quantity > 1 {
                    ch_item.quantity -= 1;
                    ch_item.version += 1;
                } else {
                    snap.items.remove(&loaded_charge_item_id);
                }
                Ok(())
            }) {
                return Some(Err(e));
            }

            // Record weapon cycle start and spool counter only after ammo consumption succeeded
            let spool_cycles = {
                let mut wguard = ctx
                    .weapon_cycle_state
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let (_, prev_spool) = wguard.get(&ship_id).copied().unwrap_or((None, 0));
                let next_spool = prev_spool.saturating_add(1).min(30);
                wguard.insert(ship_id, (Some(attacker_tick), next_spool));
                next_spool
            };

            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            let now_unix = ctx.now_unix();
            let attacker_ship_val = ctx
                .sde
                .types
                .get(&attacker_ball.type_id)
                .map(|t| t.base_price)
                .unwrap_or(230_000.0);
            let target_ship_val = ctx
                .sde
                .types
                .get(&target_ball.type_id)
                .map(|t| t.base_price)
                .unwrap_or(230_000.0);

            // If target is another player capsuleer, evaluate CrimeWatch 2.0 legality in current system
            let mut has_legal_engagement = false;
            if let Some((victim_char_id, _)) = player_target_info {
                has_legal_engagement = ctx.db.with_snapshot(|snap| {
                    is_legal_war_or_suspect_target(snap, char_id, victim_char_id, now_unix)
                });
                if sys_sec >= 0.45 && !has_legal_engagement {
                    // Illegal High-Sec Attack -> Criminal Flag -> CONCORD Response -> Kill Right (`E2E-12`)
                    let concord_delay = concord_response_delay_sec(sys_sec);
                    let _ = sys_handle.remove_ball(ship_id).await;
                    ctx.invalidate_ship_fitting(ship_id);
                    let (kill_id, kr_id) = ctx
                        .db
                        .try_with_mut_snapshot(|snap| {
                            if let Some(att) = snap.characters.get_mut(&char_id) {
                                att.security_status = (att.security_status - 0.5).max(-10.0);
                            }
                            snap.next_entity_id += 1;
                            let kid = snap.next_entity_id;
                            snap.killmails.insert(
                                kid,
                                KillmailRecord {
                                    kill_id: kid,
                                    victim_character_id: char_id,
                                    attacker_character_id: 1_000_125, // CONCORD
                                    solar_system_id: sys_id,
                                    ship_type_id: attacker_ball.type_id,
                                    total_value_isk: attacker_ship_val,
                                },
                            );
                            snap.next_entity_id += 1;
                            let krid = snap.next_entity_id;
                            snap.kill_rights.insert(
                                krid,
                                KillRightRecord {
                                    kill_right_id: krid,
                                    owner_id: victim_char_id,
                                    target_id: char_id,
                                    activation_fee: IskCents::from_isk(1_000_000),
                                    is_active_suspect: false,
                                },
                            );
                            Ok::<_, std::convert::Infallible>((kid, krid))
                        })
                        .unwrap();
                    // Destroy the criminal's ship + cargo/modules and respawn in a Capsule at home_station_id
                    if let Ok((respawned_ch, capsule)) =
                        ctx.db.respawn_character_at_home_station(char_id, ship_id, &ctx.sde)
                    {
                        session.ship_id = Some(capsule.item_id);
                        session.station_id = respawned_ch.station_id;
                        session.structure_id = None;
                        session.solar_system_id = Some(respawned_ch.solar_system_id);
                        session.constellation_id = Some(respawned_ch.constellation_id);
                        session.region_id = Some(respawned_ch.region_id);
                    }
                    return Some(Ok(PyRep::keyval([
                        ("criminalCharacterID", PyRep::Integer(char_id)),
                        ("victimCharacterID", PyRep::Integer(victim_char_id)),
                        ("concordResponseSeconds", PyRep::Real(concord_delay)),
                        ("concordKillmailID", PyRep::Integer(kill_id)),
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("criminalTimerSeconds", PyRep::Integer(900)),
                    ])));
                }
            }

            let target_sig = if target_ball.radius > 0.0 {
                target_ball.radius
            } else {
                35.0
            };
            let angular_vel = eve_destiny::combat::compute_angular_velocity(
                attacker_ball.position,
                attacker_ball.velocity,
                target_ball.position,
                target_ball.velocity,
            );
            let p_hit = turret_chance_to_hit(
                angular_vel,
                tracking_speed,
                target_sig,
                dist.max(500.0),
                optimal_range,
                falloff_range,
            );
            let hit_roll: f64 =
                ctx.next_invention_roll(ship_id as u64, target_ball_id as u64, attacker_tick);
            let (shot_dmg, turret_wrecking) = turret_shot_damage(base_dmg, p_hit, hit_roll);
            let is_wrecking = match weapon_kind {
                "MissileLauncher" | "VortonProjector" | "BreacherPod" => false,
                _ => turret_wrecking,
            };

            let mut vorton_secondary_targets = Vec::new();
            let scaled_dmg = match weapon_kind {
                "MissileLauncher" => {
                    let m_factor = missile_damage_factor(
                        target_sig,
                        target_ball.velocity.length(),
                        missile_exp_radius,
                        missile_exp_velocity,
                        missile_drf,
                    );
                    base_dmg.scaled(m_factor)
                }
                "EntropicDisintegrator" => {
                    let mult = entropic_spool_multiplier(spool_cycles, 30, 0.05);
                    shot_dmg.scaled(mult)
                }
                "BreacherPod" => {
                    let pod_cap = base_dmg.total().max(1.0);
                    let tick_dmg = breacher_pod_tick_damage(pod_cap, 0.01, 20_000.0);
                    base_dmg.scaled(tick_dmg / pod_cap)
                }
                "VortonProjector" => {
                    let other_mission_balls: std::collections::HashSet<i64> =
                        ctx.db.with_snapshot(|snap| {
                            snap.active_missions
                                .iter()
                                .filter(|((m_char_id, _), m)| {
                                    *m_char_id != char_id && m.state == "Accepted"
                                })
                                .flat_map(|(_, m)| m.spawned_npc_ball_ids.iter().copied())
                                .collect()
                        });
                    let all_balls = sys_handle.list_balls().await.unwrap_or_default();
                    let candidates: Vec<(i64, DVec3)> = all_balls
                        .into_iter()
                        .filter(|b| {
                            b.id != ship_id
                                && b.id != target_ball_id
                                && !other_mission_balls.contains(&b.id)
                                && ctx
                                    .sde
                                    .types
                                    .get(&b.type_id)
                                    .map(|t| {
                                        t.category_id == 11
                                            && t.default_attributes
                                                .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                                                .copied()
                                                .unwrap_or(0.0)
                                                > 0.0
                                    })
                                    .unwrap_or(false)
                        })
                        .map(|b| (b.id, b.position))
                        .collect();
                    let max_secondary_targets = weapon_type
                        .default_attributes
                        .get(&2104)
                        .copied()
                        .unwrap_or(9.0) as usize;
                    let hits = vorton_arc_targets_with_cap(
                        target_ball_id,
                        target_ball.position,
                        &candidates,
                        10_000.0,
                        max_secondary_targets,
                    );
                    vorton_secondary_targets = hits.into_iter().skip(1).collect();
                    let v_factor = missile_damage_factor(
                        target_sig,
                        target_ball.velocity.length(),
                        missile_exp_radius,
                        missile_exp_velocity,
                        missile_drf,
                    );
                    base_dmg.scaled(v_factor)
                }
                _ => shot_dmg,
            };

            let Some((applied_dmg, destroyed, _)) =
                (match sys_handle.apply_damage(target_ball_id, scaled_dmg).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "CombatApplicationFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: format!("Target ball {target_ball_id} no longer exists"),
                }));
            };

            // Apply Vorton Projector secondary arcs to chained nearby targets using each arc target's missile_damage_factor
            let mut destroyed_arc_ids = Vec::new();
            for arc_ball_id in &vorton_secondary_targets {
                let arc_dmg = if let Ok(Some((_, arc_ball))) =
                    sys_handle.get_ball_snapshot(*arc_ball_id).await
                {
                    let arc_sig = if arc_ball.radius > 0.0 {
                        arc_ball.radius
                    } else {
                        35.0
                    };
                    let arc_factor = missile_damage_factor(
                        arc_sig,
                        arc_ball.velocity.length(),
                        missile_exp_radius,
                        missile_exp_velocity,
                        missile_drf,
                    );
                    base_dmg.scaled(arc_factor)
                } else {
                    scaled_dmg
                };
                if let Ok(Some((_, arc_destroyed, _))) =
                    sys_handle.apply_damage(*arc_ball_id, arc_dmg).await
                {
                    if arc_destroyed {
                        ctx.living_world.mark_convoy_destroyed(*arc_ball_id);
                        destroyed_arc_ids.push(*arc_ball_id);
                    }
                }
            }

            if active_convoy_opt.is_some() && destroyed {
                let (wreck_item_id, loot_item_id, spiked_ask) = match ctx
                    .living_world
                    .interdict_convoy(target_ball_id, char_id, &ctx.db)
                {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "InterdictionFailed".into(),
                            message: e,
                        }))
                    }
                };
                let _ = sys_handle
                    .spawn_ball_with_combat(
                        eve_destiny::Ball {
                            id: wreck_item_id,
                            type_id: 26557,
                            position: target_ball.position,
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: 10_000.0,
                            radius: 50.0,
                            speed_fraction: 0.0,
                            mode: eve_destiny::MovementMode::Stop,
                        },
                        eve_destiny::CombatState {
                            shield_hp: 0.0,
                            max_shield_hp: 0.0,
                            armor_hp: 0.0,
                            max_armor_hp: 0.0,
                            structure_hp: 500.0,
                            max_structure_hp: 500.0,
                            ..eve_destiny::CombatState::default()
                        },
                    )
                    .await;
                let kill_id = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        snap.next_entity_id += 1;
                        let kid = snap.next_entity_id;
                        snap.killmails.insert(
                            kid,
                            KillmailRecord {
                                kill_id: kid,
                                victim_character_id: target_ball_id,
                                attacker_character_id: char_id,
                                solar_system_id: sys_id,
                                ship_type_id: target_ball.type_id,
                                total_value_isk: target_ship_val,
                            },
                        );
                        Ok::<_, std::convert::Infallible>(kid)
                    })
                    .unwrap();
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("convoyID", PyRep::Integer(target_ball_id)),
                    ("appliedDamage", PyRep::Real(applied_dmg)),
                    ("isWreckingShot", PyRep::Bool(is_wrecking)),
                    ("destroyed", PyRep::Bool(true)),
                    ("killID", PyRep::Integer(kill_id)),
                    ("wreckItemID", PyRep::Integer(wreck_item_id)),
                    ("wreckTypeID", PyRep::Integer(26557)),
                    ("lootItemID", PyRep::Integer(loot_item_id)),
                    ("newDestinationAskISK", PyRep::Real(spiked_ask.to_wire_f64())),
                ])));
            }

            // Handle player target engagement (high-sec suspect/war, low-sec, null-sec, or W-space)
            if let Some((victim_char_id, victim_ship_id)) = player_target_info {
                let is_lowsec_unprovoked =
                    sys_sec > 0.0 && sys_sec < 0.45 && !has_legal_engagement;
                let legal_engagement = !is_lowsec_unprovoked;
                let is_pod_target = target_ball.type_id == 670;

                if destroyed {
                    ctx.invalidate_ship_fitting(victim_ship_id);
                }

                let (kill_id, ejected_capsule_id) = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        if is_lowsec_unprovoked {
                            let sec_penalty = if is_pod_target { 1.5 } else { 0.2 };
                            if let Some(att) = snap.characters.get_mut(&char_id) {
                                att.security_status = (att.security_status - sec_penalty).max(-10.0);
                            }
                            // Issue or refresh Suspect flag / KillRight without unbounded duplication
                            if let Some(existing_kr) = snap.kill_rights.values_mut().find(|kr| {
                                kr.owner_id == victim_char_id && kr.target_id == char_id
                            }) {
                                existing_kr.is_active_suspect = true;
                            } else {
                                snap.next_entity_id += 1;
                                let krid = snap.next_entity_id;
                                snap.kill_rights.insert(
                                    krid,
                                    KillRightRecord {
                                        kill_right_id: krid,
                                        owner_id: victim_char_id,
                                        target_id: char_id,
                                        activation_fee: IskCents::from_isk(1_000_000),
                                        is_active_suspect: true,
                                    },
                                );
                            }
                        }
                        if destroyed {
                            for kr in snap.kill_rights.values_mut() {
                                if kr.target_id == victim_char_id && kr.is_active_suspect {
                                    kr.is_active_suspect = false;
                                }
                            }
                            snap.next_entity_id += 1;
                            let id = snap.next_entity_id;
                            snap.killmails.insert(
                                id,
                                KillmailRecord {
                                    kill_id: id,
                                    victim_character_id: victim_char_id,
                                    attacker_character_id: char_id,
                                    solar_system_id: sys_id,
                                    ship_type_id: target_ball.type_id,
                                    total_value_isk: target_ship_val,
                                },
                            );

                            let mut cap_id = 0;
                            if !is_pod_target {
                                // Recursively delete destroyed victim ship and all items fitted/contained inside it (including nested containers)
                                let mut to_remove =
                                    std::collections::HashSet::from([victim_ship_id]);
                                loop {
                                    let mut added = false;
                                    for (&iid, item) in &snap.items {
                                        if !to_remove.contains(&iid)
                                            && to_remove.contains(&item.location_id)
                                        {
                                            to_remove.insert(iid);
                                            added = true;
                                        }
                                    }
                                    if !added {
                                        break;
                                    }
                                }
                                snap.items.retain(|iid, _| !to_remove.contains(iid));
                                // Reuse an existing in-space Capsule in `sys_id` or allocate a fresh Capsule in space (`location_id = sys_id as i64`)
                                let existing_in_space_cap = snap
                                    .items
                                    .values()
                                    .find(|i| {
                                        i.owner_id == victim_char_id
                                            && i.type_id == 670
                                            && i.location_id == sys_id as i64
                                    })
                                    .map(|i| i.item_id);
                                cap_id = if let Some(cid) = existing_in_space_cap {
                                    if let Some(cap) = snap.items.get_mut(&cid) {
                                        cap.location_id = sys_id as i64;
                                        cap.flag = InventoryFlag::None;
                                        cap.version += 1;
                                    }
                                    cid
                                } else {
                                    snap.next_entity_id += 1;
                                    let cid = snap.next_entity_id;
                                    snap.items.insert(
                                        cid,
                                        InventoryItemRecord {
                                            item_id: cid,
                                            type_id: 670,
                                            owner_id: victim_char_id,
                                            location_id: sys_id as i64,
                                            flag: InventoryFlag::None,
                                            quantity: 1,
                                            singleton: true,
                                            version: 1,
                                            dynamic_attributes: HashMap::new(),
                                            source_mutaplasmid_id: None,
                                            insured_payout_isk: 0.0,
                                            is_blueprint_copy: false,
                                            blueprint_runs_remaining: None,
                                        },
                                    );
                                    cid
                                };
                                if let Some(vic) = snap.characters.get_mut(&victim_char_id) {
                                    vic.active_ship_id = cap_id;
                                    vic.version += 1;
                                    vic.system_epoch += 1;
                                }
                            }
                            Ok::<_, std::convert::Infallible>((id, cap_id))
                        } else {
                            Ok::<_, std::convert::Infallible>((0, 0))
                        }
                    })
                    .unwrap();
                if destroyed {
                    if is_pod_target {
                        let _ = ctx.db.respawn_character_at_home_station(
                            victim_char_id,
                            victim_ship_id,
                            &ctx.sde,
                        );
                    } else if ejected_capsule_id > 0 {
                        let _ = sys_handle
                            .spawn_ball_with_combat(
                                eve_destiny::Ball {
                                    id: ejected_capsule_id,
                                    type_id: 670, // Capsule ejected at wreck location
                                    position: target_ball.position,
                                    velocity: DVec3::ZERO,
                                    max_velocity: 335.0,
                                    inertia: 0.25,
                                    mass: 16_000.0,
                                    radius: 15.0,
                                    speed_fraction: 0.0,
                                    mode: eve_destiny::MovementMode::Stop,
                                },
                                ctx.build_type_combat_state(670),
                            )
                            .await;
                    }
                }
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(victim_char_id)),
                    ("appliedDamage", PyRep::Real(applied_dmg)),
                    ("destroyed", PyRep::Bool(destroyed)),
                    ("killID", PyRep::Integer(kill_id)),
                    ("legalEngagement", PyRep::Bool(legal_engagement)),
                ])));
            }

            if !destroyed {
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("weaponKind", PyRep::String(weapon_kind.into())),
                    ("appliedDamage", PyRep::Real(applied_dmg)),
                    ("isWreckingShot", PyRep::Bool(is_wrecking)),
                    (
                        "secondaryTargetsHit",
                        PyRep::Integer(vorton_secondary_targets.len() as i64),
                    ),
                    ("destroyed", PyRep::Bool(false)),
                ])));
            }

            // Target NPC ball was destroyed in Ballpark: credit bounty (ESS split only in K-space null-sec!), spawn wreck, and update mission/career progress
            let is_kspace_nullsec = sys_sec <= 0.0 && sys_id < 31_000_000;
            let dbs_mult = ctx
                .db
                .with_snapshot(|s| s.system_states.get(&sys_id).map(|st| st.dbs_multiplier))
                .unwrap_or(1.35);
            let (pilot_isk, main_ess, reserve_ess) = if is_kspace_nullsec {
                let (p, _tax, m, r) =
                    compute_dbs_and_ess_bounty_split(npc_base_bounty, dbs_mult, 0.10);
                (p, m, r)
            } else {
                // High-sec, low-sec, and W-space pay 100% of post-tax bounty directly to the pilot
                (npc_base_bounty * (1.0 - 0.10), 0.0, 0.0)
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let pilot_cents = IskCents::from_f64_isk(pilot_isk);
                let main_cents = IskCents::from_f64_isk(main_ess);
                let reserve_cents = IskCents::from_f64_isk(reserve_ess);

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(pilot_cents).unwrap_or(ch.balance);
                }
                if is_kspace_nullsec {
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        st.ess_main_bank = st
                            .ess_main_bank
                            .checked_add(main_cents)
                            .unwrap_or(st.ess_main_bank);
                        st.ess_reserve_bank = st
                            .ess_reserve_bank
                            .checked_add(reserve_cents)
                            .unwrap_or(st.ess_reserve_bank);
                    }
                }

                // Decrement npcs_remaining only on the specific active ("Accepted") mission that spawned `target_ball_id` (or a destroyed Vorton secondary arc target)
                let mut killed_ball_ids = Vec::with_capacity(1 + destroyed_arc_ids.len());
                killed_ball_ids.push(target_ball_id);
                killed_ball_ids.extend_from_slice(&destroyed_arc_ids);
                for ((m_char_id, _), m) in snap.active_missions.iter_mut() {
                    if *m_char_id == char_id
                        && m.state == "Accepted"
                        && m.solar_system_id == sys_id
                    {
                        for &kbid in &killed_ball_ids {
                            if m.npcs_remaining > 0 {
                                if let Some(pos) =
                                    m.spawned_npc_ball_ids.iter().position(|&bid| bid == kbid)
                                {
                                    m.spawned_npc_ball_ids.remove(pos);
                                    m.npcs_remaining -= 1;
                                }
                            }
                        }
                    }
                }

                // Increment AIR Career Goal progress (Goal 1: Soldier of Fortune)
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                *npe.career_goal_progress.entry(1).or_insert(0) += 1;

                // Spawn physical wreck item (type_id 26557) while bounding empty wrecks per solar system to <= 64
                let mut empty_sys_wrecks: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.type_id == 26557
                            && i.location_id == sys_id as i64
                            && !snap.items.values().any(|child| child.location_id == i.item_id)
                    })
                    .map(|i| i.item_id)
                    .collect();
                empty_sys_wrecks.sort_unstable();
                while empty_sys_wrecks.len() >= 64 {
                    let oldest = empty_sys_wrecks.remove(0);
                    snap.items.remove(&oldest);
                }
                snap.next_entity_id += 1;
                let wreck_id = snap.next_entity_id;
                snap.items.insert(
                    wreck_id,
                    InventoryItemRecord {
                        item_id: wreck_id,
                        type_id: 26557,
                        owner_id: char_id,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );

                snap.next_entity_id += 1;
                let kid = snap.next_entity_id;
                snap.killmails.insert(
                    kid,
                    KillmailRecord {
                        kill_id: kid,
                        victim_character_id: target_id,
                        attacker_character_id: char_id,
                        solar_system_id: sys_id,
                        ship_type_id: target_ball.type_id,
                        total_value_isk: 100_000.0,
                    },
                );
                Ok::<(i64, i64), BridgeRpcError>((kid, wreck_id))
            });

            match res {
                Ok((kill_id, wreck_id)) => Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("appliedDamage", PyRep::Real(applied_dmg)),
                    ("isWreckingShot", PyRep::Bool(is_wrecking)),
                    ("destroyed", PyRep::Bool(true)),
                    ("bountyPayoutISK", PyRep::Real(pilot_isk)),
                    ("killID", PyRep::Integer(kill_id)),
                    ("wreckItemID", PyRep::Integer(wreck_id)),
                    ("wreckTypeID", PyRep::Integer(26557)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

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
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr_id)),
                    ("suspectFlagActive", PyRep::Bool(true)),
                ]))),
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

        ("essMgr", "AttemptLinkToMainBank") | ("essMgr", "AttemptLinkToReserveBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot link to ESS while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if !(sys_sec <= 0.0 && sys_id < 31_000_000) {
                return Some(Err(BridgeRpcError {
                    code: "EssOnlyInNullSec".into(),
                    message: format!(
                        "ESS banks only exist in K-space null-sec systems (system {sys_id} has security {sys_sec:.2})"
                    ),
                }));
            }
            let sys_handle = ctx.systems.get_or_spawn(sys_id);

            let Some((current_tick, ship_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship ball is not in space".into(),
                }));
            };

            let Some(ess_ball_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "EssBeaconNotPresent".into(),
                    message: "ESS link requires an explicit integer essBallID".into(),
                }));
            };
            let Some((_, ess_ball)) = (match sys_handle.get_ball_snapshot(ess_ball_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "EssBeaconNotPresent".into(),
                    message: format!("No ESS beacon present in solar system {sys_id}"),
                }));
            };
            if ess_ball.type_id != 32882 {
                return Some(Err(BridgeRpcError {
                    code: "EssBeaconNotPresent".into(),
                    message: format!("Ball {ess_ball_id} is not an Encounter Surveillance System (32882)"),
                }));
            }

            let dist = ship_ball.position.distance(ess_ball.position);
            if dist > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideEssLinkRange".into(),
                    message: format!(
                        "Ship is {:.1} m from ESS beacon (must be <= 2500 m)",
                        dist
                    ),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.system_states.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "SystemStateNotFound".into(),
                        message: format!("No ESS state for solar system {sys_id}"),
                    });
                };
                let is_reserve = method == "AttemptLinkToReserveBank";
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

                // Step 1: If not yet linked, establish link and require 5 elapsed ticks before payout
                if st.ess_linked_character_id != Some(char_id) {
                    st.ess_linked_character_id = Some(char_id);
                    st.ess_link_started_tick = Some(current_tick);
                    return Ok((0.0, true));
                }

                let start_tick = st.ess_link_started_tick.unwrap_or(current_tick);
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
                    let b = st.ess_reserve_bank;
                    st.ess_reserve_bank = IskCents(0);
                    b
                } else {
                    let b = st.ess_main_bank;
                    st.ess_main_bank = IskCents(0);
                    b
                };
                st.ess_linked_character_id = None;
                st.ess_link_started_tick = None;

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(bank).unwrap_or(ch.balance);
                }
                Ok((bank.to_wire_f64(), false))
            });
            match res {
                Ok((payout, link_started_only)) => Some(Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("linked", PyRep::Bool(true)),
                    ("linkStartedOnly", PyRep::Bool(link_started_only)),
                    ("stolenISK", PyRep::Real(payout)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("fighterMgr", "GetFightersForShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == InventoryFlag::FighterBay
                    })
                    .map(|i| {
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.type_id as i64),
                            PyRep::Integer(i.quantity as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["itemID", "typeID", "squadronSize"],
                rows,
            )))
        }

        _ => None,
    }
}
