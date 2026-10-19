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

        ("aggressionMgr", "GetCriminalTimeStamps") | ("aggressionMgr", "CheckLootRightExceptions") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("ship", "SelfDestruct") | ("beyonce", "CmdSelfDestruct") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no solar system".into(),
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            let res = crate::combat::death_pipeline::execute_self_destruct(
                ctx,
                sys_id,
                char_id,
                ship_id,
                pos,
                ctx.now_unix(),
            )
            .await;

            match res {
                Ok(death) => {
                    if let Some(cap_id) = death.ejected_capsule_id {
                        session.ship_id = Some(cap_id);
                    } else if let Ok(ch) = ctx.db.get_character(char_id) {
                        session.ship_id = Some(ch.active_ship_id);
                        session.station_id = ch.station_id;
                        session.structure_id = None;
                        session.solar_system_id = Some(ch.solar_system_id);
                    }
                    Some(Ok(PyRep::keyval([
                        ("wreckID", PyRep::Integer(death.wreck_item_id)),
                        ("killmailID", PyRep::Integer(death.killmail_id)),
                        ("insurancePayoutISK", PyRep::Real(death.insurance_payout_isk)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "SelfDestructFailed".into(),
                    message: e,
                })),
            }
        }

        ("ship", "AbortSelfDestruct") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        ("ship", "LaunchDrones") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let drone_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.iter().filter_map(|it| it.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let mut ctrl = crate::combat::drones_salvage_tractor::DroneController::new();
            match ctrl.launch_drones(ctx, sys_id, char_id, ship_id, &drone_ids).await {
                Ok(launched) => Some(Ok(PyRep::List(launched.into_iter().map(PyRep::Integer).collect()))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "LaunchDronesFailed".into(),
                    message: e,
                })),
            }
        }

        ("entity", "CmdReturnBay") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let mut ctrl = crate::combat::drones_salvage_tractor::DroneController::new();
            match ctrl.recall_drones(ctx, sys_id, ship_id).await {
                Ok(recalled) => Some(Ok(PyRep::List(recalled.into_iter().map(PyRep::Integer).collect()))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "RecallDronesFailed".into(),
                    message: e,
                })),
            }
        }

        ("entity", "CmdReturnHome") | ("entity", "CmdReturnAndOrbit") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let mut ctrl = crate::combat::drones_salvage_tractor::DroneController::new();
            match ctrl.return_and_orbit(ctx, sys_id, ship_id).await {
                Ok(count) => Some(Ok(PyRep::Integer(count as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "ReturnHomeFailed".into(),
                    message: e,
                })),
            }
        }

        ("fighterMgr", "LoadFightersToTube") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let (tube_idx, fighter_item_id) = if (0..10).contains(&a0) {
                (a0 as usize, a1)
            } else if (0..10).contains(&a1) {
                (a1 as usize, a0)
            } else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFighterTube".into(),
                    message: "LoadFightersToTube requires a valid tube index".into(),
                }));
            };

            let carrier_type_id = if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| snap.structures.get(&struct_id).map(|s| s.type_id))
                    .unwrap_or(0)
            } else {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
                    .unwrap_or(0)
            };
            let tube_count = ctx
                .sde
                .types
                .get(&carrier_type_id)
                .and_then(|t| t.default_attributes.get(&2216).copied())
                .map(|v| v as usize)
                .unwrap_or(0);
            if !(0..tube_count).contains(&tube_idx) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFighterTube".into(),
                    message: format!(
                        "Tube index {tube_idx} is invalid (ship/structure has {tube_count} fighter tubes)"
                    ),
                }));
            }

            {
                let tube_loads = ctx.fighter_tube_loads.read().unwrap();
                if tube_loads
                    .iter()
                    .any(|(&(sid, tidx), &fid)| sid == ship_id && fid == fighter_item_id && tidx != tube_idx)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FighterAlreadyLoaded".into(),
                        message: format!("Fighter item {fighter_item_id} is already loaded in another tube"),
                    }));
                }
            }
            {
                let active = ctx.active_fighters.read().unwrap();
                if active
                    .values()
                    .any(|f| f.item_id == fighter_item_id && !f.is_abandoned)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FighterAlreadyLaunched".into(),
                        message: format!("Fighter item {fighter_item_id} is already launched in space"),
                    }));
                }
                if active
                    .values()
                    .any(|f| f.parent_ship_id == ship_id && f.tube_index == tube_idx && !f.is_abandoned)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FighterAlreadyLaunched".into(),
                        message: format!("Tube {tube_idx} currently has a squadron deployed in space"),
                    }));
                }
            }

            let item_opt = ctx
                .db
                .with_snapshot(|snap| snap.items.get(&fighter_item_id).cloned());
            let Some(item) = item_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Fighter item {fighter_item_id} not found"),
                }));
            };
            if item.owner_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "OwnershipViolation".into(),
                    message: format!("Fighter item {fighter_item_id} not owned by pilot"),
                }));
            }
            if item.location_id != ship_id || item.flag != InventoryFlag::FighterBay {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotInFighterBay".into(),
                    message: format!("Fighter item {fighter_item_id} not in pilot's fighter bay"),
                }));
            }
            let is_fighter_category = ctx
                .sde
                .types
                .get(&item.type_id)
                .map(|t| t.category_id == 87)
                .unwrap_or(false);
            if !is_fighter_category {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotAFighter".into(),
                    message: format!(
                        "Item {fighter_item_id} (type {}) is not a fighter squadron (category 87)",
                        item.type_id
                    ),
                }));
            }

            let max_squadron_size = ctx
                .sde
                .types
                .get(&item.type_id)
                .and_then(|t| t.default_attributes.get(&2215).copied())
                .map(|v| v as u32)
                .unwrap_or(9);

            let loaded_fighter_id = if item.quantity > max_squadron_size as i32 {
                let excess_qty = item.quantity - max_squadron_size as i32;
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(it) = snap.items.get_mut(&fighter_item_id) {
                        it.quantity = max_squadron_size as i32;
                        it.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let rem_id = snap.next_entity_id;
                    let rem_item = InventoryItemRecord {
                        item_id: rem_id,
                        type_id: item.type_id,
                        owner_id: item.owner_id,
                        location_id: item.location_id,
                        flag: item.flag,
                        quantity: excess_qty,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    };
                    snap.items.insert(rem_id, rem_item);
                    fighter_item_id
                })
            } else {
                fighter_item_id
            };

            ctx.fighter_tube_loads
                .write()
                .unwrap()
                .insert((ship_id, tube_idx), loaded_fighter_id);
            Some(Ok(PyRep::keyval([
                ("tubeIndex", PyRep::Integer(tube_idx as i64)),
                ("fighterID", PyRep::Integer(loaded_fighter_id)),
                ("success", PyRep::Bool(true)),
            ])))
        }

        ("fighterMgr", "UnloadTubeToFighterBay") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let raw_tube = args.first().and_then(|a| a.as_i64()).unwrap_or(-1);
            let carrier_type_id = if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| snap.structures.get(&struct_id).map(|s| s.type_id))
                    .unwrap_or(0)
            } else {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
                    .unwrap_or(0)
            };
            let tube_count = ctx
                .sde
                .types
                .get(&carrier_type_id)
                .and_then(|t| t.default_attributes.get(&2216).copied())
                .map(|v| v as usize)
                .unwrap_or(0);
            if raw_tube < 0 || raw_tube as usize >= tube_count {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFighterTube".into(),
                    message: format!(
                        "Tube index {raw_tube} is invalid (ship/structure has {tube_count} tubes)"
                    ),
                }));
            }
            let tube_idx = raw_tube as usize;
            {
                let active = ctx.active_fighters.read().unwrap();
                if active
                    .values()
                    .any(|f| f.parent_ship_id == ship_id && f.tube_index == tube_idx && !f.is_abandoned)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FighterAlreadyLaunched".into(),
                        message: format!("Cannot unload tube {tube_idx} while squadron is deployed in space"),
                    }));
                }
            }
            let removed = ctx
                .fighter_tube_loads
                .write()
                .unwrap()
                .remove(&(ship_id, tube_idx));
            Some(Ok(PyRep::Bool(removed.is_some())))
        }

        ("fighterMgr", "LaunchFightersFromTubes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in space".into(),
                }));
            };

            let requested_tubes: Option<Vec<usize>> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) if !items.is_empty() => {
                    Some(items.iter().filter_map(|x| x.as_i64()).map(|t| t as usize).collect())
                }
                Some(PyRep::Integer(t)) => Some(vec![*t as usize]),
                _ => None,
            };

            let carrier_type_id = if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| snap.structures.get(&struct_id).map(|s| s.type_id))
                    .unwrap_or(0)
            } else {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id))
                    .unwrap_or(0)
            };
            let tube_count = ctx
                .sde
                .types
                .get(&carrier_type_id)
                .and_then(|t| t.default_attributes.get(&2216).copied())
                .map(|v| v as usize)
                .unwrap_or(0);

            let tubes_to_launch: Vec<usize> = if let Some(req) = requested_tubes {
                for &tidx in &req {
                    if !(0..tube_count).contains(&tidx) {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFighterTube".into(),
                            message: format!("Tube index {tidx} is invalid (ship/structure has {tube_count} tubes)"),
                        }));
                    }
                    let active = ctx.active_fighters.read().unwrap();
                    if active
                        .values()
                        .any(|f| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned)
                    {
                        return Some(Err(BridgeRpcError {
                            code: "FighterAlreadyLaunched".into(),
                            message: format!("Fighter squadron in tube {tidx} is already launched in space"),
                        }));
                    }
                    let tube_loads = ctx.fighter_tube_loads.read().unwrap();
                    if !tube_loads.contains_key(&(ship_id, tidx)) {
                        return Some(Err(BridgeRpcError {
                            code: "EmptyFighterTube".into(),
                            message: format!("Fighter tube {tidx} is empty"),
                        }));
                    }
                }
                req
            } else {
                let tube_loads = ctx.fighter_tube_loads.read().unwrap();
                let available: Vec<usize> = (0..tube_count)
                    .filter(|&tidx| tube_loads.contains_key(&(ship_id, tidx)))
                    .collect();
                if available.is_empty() {
                    let active = ctx.active_fighters.read().unwrap();
                    if (0..tube_count).any(|tidx| {
                        active
                            .values()
                            .any(|f| f.parent_ship_id == ship_id && f.tube_index == tidx && !f.is_abandoned)
                    }) {
                        return Some(Err(BridgeRpcError {
                            code: "FighterAlreadyLaunched".into(),
                            message: "All loaded fighter squadrons are already launched in space".into(),
                        }));
                    }
                    return Some(Err(BridgeRpcError {
                        code: "EmptyFighterTube".into(),
                        message: "No fighters loaded in tubes".into(),
                    }));
                }
                available
            };

            let mut launched_ball_ids = Vec::new();
            for tube_idx in tubes_to_launch {
                let fighter_item_id = {
                    let mut guard = ctx.fighter_tube_loads.write().unwrap();
                    guard.remove(&(ship_id, tube_idx))
                };
                let Some(fighter_item_id) = fighter_item_id else { continue };
                let item_opt = ctx
                    .db
                    .with_snapshot(|snap| snap.items.get(&fighter_item_id).cloned());
                let Some(item) = item_opt else { continue };

                let ball_id = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    snap.next_entity_id
                });
                let offset = DVec3::new((tube_idx as f64 + 1.0) * 50.0, 0.0, 0.0);
                let ball = eve_destiny::Ball {
                    id: ball_id,
                    type_id: item.type_id,
                    position: ship_ball.position + offset,
                    velocity: DVec3::ZERO,
                    max_velocity: 2_500.0,
                    inertia: 0.5,
                    mass: 25_000.0,
                    radius: 20.0,
                    speed_fraction: 1.0,
                    mode: eve_destiny::MovementMode::Orbit {
                        target_id: ship_id,
                        range: 1_000.0,
                    },
                };
                let cs = ctx.build_type_combat_state(item.type_id);
                let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

                let max_squadron_size = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .and_then(|t| t.default_attributes.get(&2215).copied())
                    .map(|v| v as u32)
                    .unwrap_or(9);

                let squad = crate::router::context::ActiveFighterSquadron {
                    ball_id,
                    item_id: fighter_item_id,
                    type_id: item.type_id,
                    owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    tube_index: tube_idx,
                    squadron_size: (item.quantity as u32).min(max_squadron_size),
                    target_id: None,
                    is_abandoned: false,
                    mwd_active: false,
                };
                ctx.active_fighters.write().unwrap().insert(ball_id, squad);
                launched_ball_ids.push(PyRep::Integer(ball_id));
            }
            Some(Ok(PyRep::List(launched_ball_ids)))
        }

        ("fighterMgr", "RecallFightersToTubes") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let filter_ids: Option<Vec<i64>> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) if !items.is_empty() => {
                    Some(items.iter().filter_map(|x| x.as_i64()).collect())
                }
                Some(PyRep::Integer(t)) => Some(vec![*t]),
                _ => None,
            };

            let fighters_to_recall: Vec<crate::router::context::ActiveFighterSquadron> = ctx
                .active_fighters
                .read()
                .unwrap()
                .values()
                .filter(|f| {
                    f.parent_ship_id == ship_id
                        && filter_ids
                            .as_ref()
                            .map(|ids| {
                                ids.contains(&(f.tube_index as i64))
                                    || ids.contains(&f.ball_id)
                                    || ids.contains(&f.item_id)
                            })
                            .unwrap_or(true)
                })
                .cloned()
                .collect();

            let mut recalled = Vec::new();
            for f in fighters_to_recall {
                ctx.active_fighters.write().unwrap().remove(&f.ball_id);
                ctx.fighter_tube_loads
                    .write()
                    .unwrap()
                    .insert((ship_id, f.tube_index), f.item_id);
                let _ = sys_handle.remove_ball(f.ball_id).await;
                recalled.push(PyRep::Integer(f.ball_id));
            }
            Some(Ok(PyRep::List(recalled)))
        }

        ("fighterMgr", "ExecuteMovementCommandOnFighters") => {
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
                    code: "InvalidArgument".into(),
                    message: "ExecuteMovementCommandOnFighters requires fighterIDs".into(),
                }));
            }
            {
                let fighters = ctx.active_fighters.read().unwrap();
                for &fid in &fighter_ids {
                    let Some(f) = fighters.get(&fid) else {
                        return Some(Err(BridgeRpcError {
                            code: "FighterNotFound".into(),
                            message: format!("Fighter squadron {fid} not found"),
                        }));
                    };
                    if f.owner_char_id != char_id || f.is_abandoned {
                        return Some(Err(BridgeRpcError {
                            code: "FighterNotOwned".into(),
                            message: format!("Fighter squadron {fid} is not controlled by pilot"),
                        }));
                    }
                }
            }
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let cmd = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("GOTO_POINT")
                .to_ascii_uppercase();
            let mode = match cmd.as_str() {
                "ORBIT" => {
                    let target_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                    let range = args.get(3).and_then(|a| a.as_f64()).unwrap_or(1_000.0);
                    eve_destiny::MovementMode::Orbit { target_id, range }
                }
                "FOLLOW" | "APPROACH" => {
                    let target_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                    let range = args.get(3).and_then(|a| a.as_f64()).unwrap_or(500.0);
                    eve_destiny::MovementMode::Follow { target_id, range }
                }
                "STOP" => eve_destiny::MovementMode::Stop,
                _ => {
                    let x = args
                        .get(2)
                        .or_else(|| args.get(1))
                        .and_then(|a| a.as_f64())
                        .unwrap_or(1.0);
                    let y = args
                        .get(3)
                        .or_else(|| args.get(2))
                        .and_then(|a| a.as_f64())
                        .unwrap_or(0.0);
                    let z = args
                        .get(4)
                        .or_else(|| args.get(3))
                        .and_then(|a| a.as_f64())
                        .unwrap_or(0.0);
                    eve_destiny::MovementMode::GotoDirection {
                        direction: DVec3::new(x, y, z),
                    }
                }
            };
            for fid in fighter_ids {
                let _ = sys_handle.set_movement_mode(fid, mode.clone()).await;
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("fighterMgr", "CmdActivateAbilitySlots") => {
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
            let target_id = args.get(2).and_then(|a| a.as_i64());

            let fighter_opt = ctx
                .active_fighters
                .read()
                .unwrap()
                .get(&fighter_ball_id)
                .cloned();
            let Some(fighter) = fighter_opt else {
                return Some(Err(BridgeRpcError {
                    code: "FighterNotFound".into(),
                    message: format!(
                        "Fighter squadron {fighter_ball_id} not found or not launched"
                    ),
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
                    message: format!(
                        "Fighter squadron {fighter_ball_id} is not in solar system {sys_id}"
                    ),
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

                let squad_mult = fighter.squadron_size.max(1) as f64;
                let dmg = match ability_id {
                    18..=27 | 29..=32 => {
                        let dmg_mult = attr(2178).unwrap_or(1.0) * squad_mult;
                        DamageVector {
                            em: attr(2171).unwrap_or(0.0) * dmg_mult,
                            thermal: attr(2172).unwrap_or(0.0) * dmg_mult,
                            kinetic: attr(2173).unwrap_or(0.0) * dmg_mult,
                            explosive: attr(2174).unwrap_or(0.0) * dmg_mult,
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
                    33..=36 => {
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
                    2 | 10 | 11 | 12 | 16 => DamageVector::default(),
                    _ => {
                        if attr(2227).is_some() || attr(2228).is_some() || attr(2229).is_some() || attr(2230).is_some() {
                            let dmg_mult = attr(2226).unwrap_or(1.0) * squad_mult;
                            DamageVector {
                                em: attr(2227).unwrap_or(0.0) * dmg_mult,
                                thermal: attr(2228).unwrap_or(0.0) * dmg_mult,
                                kinetic: attr(2229).unwrap_or(0.0) * dmg_mult,
                                explosive: attr(2230).unwrap_or(0.0) * dmg_mult,
                            }
                        } else if attr(2171).is_some() || attr(2172).is_some() || attr(2173).is_some() || attr(2174).is_some() {
                            let dmg_mult = attr(2178).unwrap_or(1.0) * squad_mult;
                            DamageVector {
                                em: attr(2171).unwrap_or(0.0) * dmg_mult,
                                thermal: attr(2172).unwrap_or(0.0) * dmg_mult,
                                kinetic: attr(2173).unwrap_or(0.0) * dmg_mult,
                                explosive: attr(2174).unwrap_or(0.0) * dmg_mult,
                            }
                        } else if attr(2131).is_some() || attr(2132).is_some() || attr(2133).is_some() || attr(2134).is_some() {
                            let dmg_mult = attr(2130).unwrap_or(1.0) * squad_mult;
                            DamageVector {
                                em: attr(2131).unwrap_or(0.0) * dmg_mult,
                                thermal: attr(2132).unwrap_or(0.0) * dmg_mult,
                                kinetic: attr(2133).unwrap_or(0.0) * dmg_mult,
                                explosive: attr(2134).unwrap_or(0.0) * dmg_mult,
                            }
                        } else {
                            DamageVector::default()
                        }
                    }
                };
                if dmg.em + dmg.thermal + dmg.kinetic + dmg.explosive > 0.0 {
                    let _ = sys_handle.apply_damage(tid, dmg).await;
                }

                // CrimeWatch: trigger weapon timer and track combat aggression
                ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now_unix);

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
                    } else if let Some(st) = snap.structures.get(&tid) {
                        if !ctx.sde.npc_corporations.contains_key(&st.owner_corp_id) {
                            snap.corporations
                                .get(&st.owner_corp_id)
                                .map(|c| c.ceo_id)
                                .filter(|cid| snap.characters.contains_key(cid))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });

                if let Some(victim_char_id) = victim_char_opt {
                    if victim_char_id != char_id {
                        let is_legal = {
                            let cw = ctx.crimewatch.read().unwrap();
                            if let Some(vic_st) = cw.states.get(&victim_char_id) {
                                vic_st.is_suspect(now_unix) || vic_st.is_criminal(now_unix)
                            } else {
                                false
                            }
                        };
                        let eval = ctx.crimewatch.write().unwrap().handle_combat_aggression(
                            char_id,
                            victim_char_id,
                            fighter.parent_ship_id,
                            sys_id,
                            sec,
                            is_legal,
                            now_unix,
                        );
                        if matches!(
                            eval,
                            crate::combat::crimewatch::AggressionEvaluation::CriminalFlagTriggered { .. }
                                | crate::combat::crimewatch::AggressionEvaluation::SuspectFlagTriggered
                        ) {
                            ctx.db.with_mut_snapshot(|snap| {
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    let loss = crate::combat::crimewatch::compute_sec_status_loss(
                                        ch.security_status,
                                        false,
                                        sec,
                                    );
                                    ch.security_status = (ch.security_status - loss).max(-10.0);
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
                        let _ = sys_handle.set_speed_fraction(fighter_ball_id, 1.0).await;
                        if let Some(f) = ctx
                            .active_fighters
                            .write()
                            .unwrap()
                            .get_mut(&fighter_ball_id)
                        {
                            f.mwd_active = true;
                        }
                    }
                    _ => {
                        if let Some(mjd_dist) = attr(2154) {
                            let _ = sys_handle.micro_jump(fighter_ball_id, mjd_dist).await;
                        } else if attr(2152).is_some() || attr(2151).is_some() || attr(2224).is_some() {
                            let _ = sys_handle.set_speed_fraction(fighter_ball_id, 1.0).await;
                            if let Some(f) = ctx
                                .active_fighters
                                .write()
                                .unwrap()
                                .get_mut(&fighter_ball_id)
                            {
                                f.mwd_active = true;
                            }
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
                _ => {
                    attr(2177)
                        .or_else(|| attr(2233))
                        .or_else(|| attr(2182))
                        .or_else(|| attr(2157))
                        .or_else(|| attr(2158))
                        .or_else(|| attr(2123))
                        .or_else(|| attr(2155))
                        .or_else(|| attr(2183))
                        .or_else(|| attr(2203))
                        .or_else(|| attr(2208))
                        .or_else(|| attr(2220))
                        .or_else(|| attr(2238))
                        .unwrap_or(10_000.0)
                }
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
                ("tooltipTextID", tooltip_text_id),
                ("turretGraphicID", turret_graphic_id),
                ("success", PyRep::Bool(true)),
            ])))
        }

        ("fighterMgr", "CmdAbandonFighter") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let target_fighter_id = args.first().and_then(|a| a.as_i64());
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let mut abandoned = Vec::new();
            let mut abandoned_item_ids = Vec::new();
            {
                let mut fighters = ctx.active_fighters.write().unwrap();
                for f in fighters.values_mut() {
                    if f.parent_ship_id == ship_id
                        && target_fighter_id.map(|tid| tid == f.ball_id).unwrap_or(true)
                    {
                        f.is_abandoned = true;
                        abandoned.push(f.ball_id);
                        abandoned_item_ids.push(f.item_id);
                    }
                }
            }
            if !abandoned_item_ids.is_empty() {
                ctx.db.with_mut_snapshot(|snap| {
                    for iid in &abandoned_item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.location_id = sys_id as i64;
                            item.flag = InventoryFlag::None;
                            item.version += 1;
                        }
                    }
                });
            }
            for &bid in &abandoned {
                let _ = sys_handle
                    .set_movement_mode(bid, eve_destiny::MovementMode::Stop)
                    .await;
            }
            Some(Ok(PyRep::List(
                abandoned.into_iter().map(PyRep::Integer).collect(),
            )))
        }

        _ => None,
    }
}
