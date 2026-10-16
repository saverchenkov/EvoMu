//! `combat_crimewatch` — Domain 6: Weapons, EWAR, Drones/Fighters, CrimeWatch 2.0, CONCORD, Killmails, & DBS/ESS (`§3.6`).

#![forbid(unsafe_code)]

use crate::formulas::{
    breacher_pod_tick_damage, compute_dbs_and_ess_bounty_split, compute_fighter_squadron_damage,
    concord_response_delay_sec, entropic_spool_multiplier, turret_chance_to_hit,
    turret_shot_damage, vorton_chain_targets,
};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillRightRecord, KillmailRecord};
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // Turret / Missile / Vorton / Entropic / Breacher Combat Engagement & CrimeWatch 2.0 Enforcement
        ("dogmaIM", "FireWeapon")
        | ("beyonce", "CmdAttackTarget")
        | ("superWeaponMgr", "ActivateSinglePointTargetedModule")
        | ("superWeaponMgr", "ActivateSlashModule") => {
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
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(700_000_001);
            if target_id <= 0 || target_id == char_id || target_id == ship_id {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: "Invalid or self combat target".into(),
                }));
            }
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let sys_handle = ctx.systems.get_or_spawn(sys_id);

            // Require the attacker's ship ball to exist in the current solar system's Ballpark
            let Some((_, attacker_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Attacker ship {ship_id} is not spawned in space"),
                }));
            };

            let weapon_kind = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("ProjectileTurret");
            let spool_cycles = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u32;

            // Check if target_id is another player capsuleer (CrimeWatch 2.0 legal vs illegal aggression)
            let is_player_target = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&target_id));
            if is_player_target {
                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(1.0);
                let has_active_suspect = ctx.db.with_snapshot(|snap| {
                    snap.kill_rights
                        .values()
                        .any(|kr| kr.target_id == target_id && kr.is_active_suspect)
                });
                if sec >= 0.5 && !has_active_suspect {
                    // Illegal High-Sec Attack -> Criminal Flag -> CONCORD Response -> Kill Right (`E2E-12`)
                    let concord_delay = concord_response_delay_sec(sec);
                    let _ = sys_handle.remove_ball(ship_id).await;
                    let (kill_id, kr_id) = ctx.db.with_mut_snapshot(|snap| {
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
                                total_value_isk: 230_000.0,
                            },
                        );
                        snap.next_entity_id += 1;
                        let krid = snap.next_entity_id;
                        snap.kill_rights.insert(
                            krid,
                            KillRightRecord {
                                kill_right_id: krid,
                                owner_id: target_id,
                                target_id: char_id,
                                activation_fee: IskCents::from_isk(1_000_000),
                                is_active_suspect: false,
                            },
                        );
                        (kid, krid)
                    });
                    return Some(Ok(PyRep::keyval([
                        ("criminalCharacterID", PyRep::Integer(char_id)),
                        ("victimCharacterID", PyRep::Integer(target_id)),
                        ("concordResponseSeconds", PyRep::Real(concord_delay)),
                        ("concordKillmailID", PyRep::Integer(kill_id)),
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("criminalTimerSeconds", PyRep::Integer(900)),
                    ])));
                } else {
                    // Legal engagement against Suspect / LowSec / NullSec capsuleer
                    let kid = ctx.db.with_mut_snapshot(|snap| {
                        for kr in snap.kill_rights.values_mut() {
                            if kr.target_id == target_id && kr.is_active_suspect {
                                kr.is_active_suspect = false;
                            }
                        }
                        snap.next_entity_id += 1;
                        let id = snap.next_entity_id;
                        snap.killmails.insert(
                            id,
                            KillmailRecord {
                                kill_id: id,
                                victim_character_id: target_id,
                                attacker_character_id: char_id,
                                solar_system_id: sys_id,
                                ship_type_id: 587,
                                total_value_isk: 230_000.0,
                            },
                        );
                        id
                    });
                    return Some(Ok(PyRep::keyval([
                        ("targetID", PyRep::Integer(target_id)),
                        ("killID", PyRep::Integer(kid)),
                        ("legalEngagement", PyRep::Bool(true)),
                    ])));
                }
            }

            // Require target ball to exist in the current solar system's Ballpark
            let Some((_, target_ball)) = (match sys_handle.get_ball_snapshot(target_id).await {
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
                    message: format!("Target ball {target_id} is not present in system {sys_id}"),
                }));
            };

            let dist = attacker_ball.position.distance(target_ball.position);
            let optimal_range = 20_000.0;
            let falloff_range = 10_000.0;
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

            let p_hit = turret_chance_to_hit(
                0.01,
                240.0,
                target_ball.radius.max(35.0),
                dist.max(500.0),
                optimal_range,
                falloff_range,
            );
            let base_dmg = DamageVector {
                em: 50.0,
                thermal: 120.0,
                kinetic: 120.0,
                explosive: 60.0,
            };
            let (shot_dmg, is_wrecking) = turret_shot_damage(base_dmg, p_hit, 0.25);

            let mult = match weapon_kind {
                "EntropicDisintegrator" => entropic_spool_multiplier(spool_cycles, 30, 0.05),
                "BreacherPod" => {
                    breacher_pod_tick_damage(200.0, 0.01, 20_000.0) / shot_dmg.total().max(1.0)
                }
                "VortonProjector" => {
                    let hits = vorton_chain_targets(
                        target_id,
                        target_ball.position,
                        &[
                            (
                                target_id + 1,
                                target_ball.position + DVec3::new(1000.0, 0.0, 0.0),
                            ),
                            (
                                target_id + 2,
                                target_ball.position + DVec3::new(2000.0, 0.0, 0.0),
                            ),
                        ],
                        5000.0,
                    );
                    hits.len().max(1) as f64
                }
                _ => 1.0,
            };

            let scaled_dmg = DamageVector {
                em: shot_dmg.em * mult,
                thermal: shot_dmg.thermal * mult,
                kinetic: shot_dmg.kinetic * mult,
                explosive: shot_dmg.explosive * mult,
            };

            let Some((applied_dmg, destroyed, _)) =
                (match sys_handle.apply_damage(target_id, scaled_dmg).await {
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
                    message: format!("Target ball {target_id} no longer exists"),
                }));
            };

            if !destroyed {
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("appliedDamage", PyRep::Real(applied_dmg)),
                    ("isWreckingShot", PyRep::Bool(is_wrecking)),
                    ("destroyed", PyRep::Bool(false)),
                ])));
            }

            // Target ball was destroyed in Ballpark: credit DBS/ESS bounty, spawn wreck, and update mission/career progress
            let dbs_mult = ctx
                .db
                .with_snapshot(|s| s.system_states.get(&sys_id).map(|st| st.dbs_multiplier))
                .unwrap_or(1.35);
            let (pilot_isk, _tax, main_ess, reserve_ess) =
                compute_dbs_and_ess_bounty_split(100_000.0, dbs_mult, 0.10);

            let res = ctx.db.with_mut_snapshot(|snap| {
                let pilot_cents = IskCents::from_f64_isk(pilot_isk);
                let main_cents = IskCents::from_f64_isk(main_ess);
                let reserve_cents = IskCents::from_f64_isk(reserve_ess);

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(pilot_cents).unwrap_or(ch.balance);
                }
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

                // Decrement npcs_remaining on any active ("Accepted") mission for this character
                for ((m_char_id, _), m) in snap.active_missions.iter_mut() {
                    if *m_char_id == char_id && m.state == "Accepted" && m.npcs_remaining > 0 {
                        m.npcs_remaining -= 1;
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

                // Spawn physical wreck item (type_id 26557)
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
                Ok((kid, wreck_id))
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

        ("killRightMgr", "ActivateKillRight") | ("killRightMgr", "BuyKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_mut_snapshot(|snap| {
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
                if char_id != kr.owner_id {
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

        ("essMgr", "AttemptLinkToMainBank")
        | ("essMgr", "AttemptLinkToReserveBank")
        | ("essMgr", "GetDataForClientSolarSystem")
        | ("essMgr", "GetMainBankTheftsForClientSolarSystem") => {
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
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
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

            let Some((_, ess_ball)) = (match sys_handle.get_ball_snapshot(750_000_002).await {
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

            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(st) = snap.system_states.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "SystemStateNotFound".into(),
                        message: format!("No ESS state for solar system {sys_id}"),
                    });
                };
                if st.ess_main_bank.0 <= 0 {
                    return Err(BridgeRpcError {
                        code: "EssBankEmpty".into(),
                        message: format!("ESS Main Bank in system {sys_id} is empty"),
                    });
                }

                // Step 1: If not yet linked, establish link and require elapsed ticks before payout
                if st.ess_linked_character_id != Some(char_id) {
                    st.ess_linked_character_id = Some(char_id);
                    st.ess_link_started_tick = Some(current_tick);
                    if method == "AttemptLinkToMainBank"
                        && args.first().and_then(|a| a.as_bool()) == Some(true)
                    {
                        return Ok((0.0, true));
                    }
                }

                let start_tick = st.ess_link_started_tick.unwrap_or(current_tick);
                if method == "AttemptLinkToReserveBank"
                    && current_tick < start_tick.saturating_add(5)
                {
                    return Err(BridgeRpcError {
                        code: "EssLinkTimerNotElapsed".into(),
                        message: format!(
                            "ESS link requires 5 ticks to complete (started at {}, current {})",
                            start_tick, current_tick
                        ),
                    });
                }

                let bank = st.ess_main_bank;
                st.ess_main_bank = IskCents(0);
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

        ("fighterMgr", "CmdActivateAbilitySlots")
        | ("fighterMgr", "ExecuteMovementCommandOnFighters")
        | ("fighterMgr", "GetFightersForShip") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let dmg = compute_fighter_squadron_damage(50.0, 9, 4.5);
            Some(Ok(PyRep::keyval([
                ("squadronSize", PyRep::Integer(9)),
                ("volleyDamage", PyRep::Real(dmg)),
            ])))
        }

        ("crimewatch", "GetClientStates")
        | ("crimewatch", "GetMySecurityStatus")
        | ("crimewatch", "GetCharacterSecurityStatus")
        | ("crimewatch", "GetSecurityStatusTransactions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sec = ctx
                .db
                .get_character(char_id)
                .map(|c| c.security_status)
                .unwrap_or(0.0);
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("securityStatus", PyRep::Real(sec)),
            ])))
        }

        ("warStatisticMgr", "GetKillMail")
        | ("warStatisticMgr", "GetKills")
        | ("warStatisticMgr", "GetBaseInfo")
        | ("warsInfoMgr", "GetWarsByOwnerID")
        | ("warsInfoMgr", "GetPublicWarInfo")
        | ("bountyProxy", "GetBounties")
        | ("bountyProxy", "GetBountiesAndKillRights") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.killmails
                    .values()
                    .filter(|k| {
                        k.attacker_character_id == char_id || k.victim_character_id == char_id
                    })
                    .map(|k| {
                        vec![
                            PyRep::Integer(k.kill_id),
                            PyRep::Integer(k.victim_character_id),
                            PyRep::Integer(k.attacker_character_id),
                            PyRep::Integer(k.solar_system_id as i64),
                            PyRep::Real(k.total_value_isk),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "killID",
                    "victimCharacterID",
                    "attackerCharacterID",
                    "solarSystemID",
                    "totalValueISK",
                ],
                rows,
            )))
        }

        _ => None,
    }
}
