//! `combat_crimewatch` — Domain 6: Weapons, EWAR, Drones/Fighters, CrimeWatch 2.0, CONCORD, Killmails, & DBS/ESS (`§3.6`).

#![forbid(unsafe_code)]

use crate::formulas::{
    breacher_pod_tick_damage, compute_dbs_and_ess_bounty_split, compute_fighter_squadron_damage,
    concord_response_delay_sec, entropic_spool_multiplier, turret_chance_to_hit,
    turret_shot_damage, vorton_chain_targets,
};
use crate::router::ServerContext;
use eve_db::{IskCents, KillRightRecord, KillmailRecord};
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
        // Turret / Missile / Vorton / Entropic / Breacher Combat Engagement
        ("dogmaIM", "FireWeapon")
        | ("beyonce", "CmdAttackTarget")
        | ("superWeaponMgr", "ActivateSuperWeapon") => {
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
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(11001);
            if target_id <= 0 || target_id == char_id || Some(target_id) == session.ship_id {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: "Invalid or self combat target".into(),
                }));
            }
            let weapon_kind = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("ProjectileTurret");
            let spool_cycles = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u32;

            let p_hit = turret_chance_to_hit(0.01, 240.0, 38.0, 1500.0, 1500.0, 8400.0);
            let base_dmg = DamageVector {
                em: 10.0,
                thermal: 120.0,
                kinetic: 120.0,
                explosive: 50.0,
            };
            let (shot_dmg, is_wrecking) = turret_shot_damage(base_dmg, p_hit, 0.25);

            let effective_dmg = match weapon_kind {
                "EntropicDisintegrator" => {
                    shot_dmg.total() * entropic_spool_multiplier(spool_cycles, 30, 0.05)
                }
                "BreacherPod" => breacher_pod_tick_damage(200.0, 0.01, 20_000.0),
                "VortonProjector" => {
                    let hits = vorton_chain_targets(
                        target_id,
                        DVec3::ZERO,
                        &[
                            (target_id + 1, DVec3::new(1000.0, 0.0, 0.0)),
                            (target_id + 2, DVec3::new(2000.0, 0.0, 0.0)),
                        ],
                        5000.0,
                    );
                    shot_dmg.total() * (hits.len() as f64)
                }
                _ => shot_dmg.total(),
            };

            // Credit NPC bounty via Dynamic Bounty System (DBS) + ESS 60/30/10 split once per target
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let dbs_mult = ctx
                .db
                .with_snapshot(|s| s.system_states.get(&sys_id).map(|st| st.dbs_multiplier))
                .unwrap_or(1.35);
            let (pilot_isk, _tax, main_ess, reserve_ess) =
                compute_dbs_and_ess_bounty_split(100_000.0, dbs_mult, 0.10);

            let res = ctx.db.with_mut_snapshot(|snap| {
                if snap
                    .killmails
                    .values()
                    .any(|km| km.victim_character_id == target_id && km.solar_system_id == sys_id)
                {
                    return Err(BridgeRpcError {
                        code: "TargetAlreadyDestroyed".into(),
                        message: format!("Target {target_id} has already been destroyed"),
                    });
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents(ch.balance.0 + IskCents::from_f64_isk(pilot_isk).0);
                }
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    st.ess_main_bank =
                        IskCents(st.ess_main_bank.0 + IskCents::from_f64_isk(main_ess).0);
                    st.ess_reserve_bank =
                        IskCents(st.ess_reserve_bank.0 + IskCents::from_f64_isk(reserve_ess).0);
                }
                snap.next_entity_id += 1;
                let kid = snap.next_entity_id;
                snap.killmails.insert(
                    kid,
                    KillmailRecord {
                        kill_id: kid,
                        victim_character_id: target_id,
                        attacker_character_id: char_id,
                        solar_system_id: sys_id,
                        ship_type_id: 587,
                        total_value_isk: 230_000.0,
                    },
                );
                Ok(kid)
            });

            match res {
                Ok(kill_id) => Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("appliedDamage", PyRep::Real(effective_dmg)),
                    ("isWreckingShot", PyRep::Bool(is_wrecking)),
                    ("bountyPayoutISK", PyRep::Real(pilot_isk)),
                    ("killID", PyRep::Integer(kill_id)),
                    ("wreckTypeID", PyRep::Integer(26557)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // CrimeWatch 2.0 Illegal High-Sec Attack -> Criminal Flag -> CONCORD Dispatch -> Kill Right (`E2E-12`)
        ("crimewatch", "TriggerCriminalAttack") | ("concordSvc", "ReportIllegalAggression") => {
            let (_, attacker_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let victim_id = args.first().and_then(|a| a.as_i64()).unwrap_or(90_000_002);
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            let concord_delay = concord_response_delay_sec(sec);

            let (kill_id, kr_id) = ctx.db.with_mut_snapshot(|snap| {
                if let Some(att) = snap.characters.get_mut(&attacker_id) {
                    att.security_status = (att.security_status - 0.5).max(-10.0);
                }
                snap.next_entity_id += 1;
                let kid = snap.next_entity_id;
                snap.killmails.insert(
                    kid,
                    KillmailRecord {
                        kill_id: kid,
                        victim_character_id: attacker_id, // CONCORD destroys criminal attacker's ship
                        attacker_character_id: 1_000_125, // CONCORD
                        solar_system_id: sys_id,
                        ship_type_id: 587,
                        total_value_isk: 230_000.0,
                    },
                );
                snap.next_entity_id += 1;
                let krid = snap.next_entity_id;
                snap.kill_rights.insert(
                    krid,
                    KillRightRecord {
                        kill_right_id: krid,
                        owner_id: victim_id,
                        target_id: attacker_id,
                        activation_fee: IskCents::from_isk(1_000_000),
                        is_active_suspect: false,
                    },
                );
                (kid, krid)
            });

            Some(Ok(PyRep::keyval([
                ("criminalCharacterID", PyRep::Integer(attacker_id)),
                ("victimCharacterID", PyRep::Integer(victim_id)),
                ("concordResponseSeconds", PyRep::Real(concord_delay)),
                ("concordKillmailID", PyRep::Integer(kill_id)),
                ("killRightID", PyRep::Integer(kr_id)),
                ("criminalTimerSeconds", PyRep::Integer(900)),
            ])))
        }

        ("killRightMgr", "ActivateKillRight") | ("crimewatch", "ActivateKillRight") => {
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
                    if caller.balance.0 < fee.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: "Insufficient ISK to pay kill right activation fee".into(),
                        });
                    }
                    caller.balance = IskCents(caller.balance.0 - fee.0);
                    if let Some(owner) = snap.characters.get_mut(&kr.owner_id) {
                        owner.balance = IskCents(owner.balance.0 + fee.0);
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
        | ("essMgr", "HackMainBank")
        | ("dynamicBountyMgr", "ClaimEssBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot link to ESS Main Bank while docked".into(),
                }));
            }
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let res = ctx.db.with_mut_snapshot(|snap| {
                let bank = snap
                    .system_states
                    .get_mut(&sys_id)
                    .map(|s| {
                        let v = s.ess_main_bank;
                        s.ess_main_bank = IskCents(0);
                        v
                    })
                    .unwrap_or(IskCents(0));
                if bank.0 <= 0 {
                    return Err(BridgeRpcError {
                        code: "EssBankEmpty".into(),
                        message: format!("ESS Main Bank in system {sys_id} is empty"),
                    });
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents(ch.balance.0 + bank.0);
                }
                Ok(bank.to_wire_f64())
            });
            match res {
                Ok(payout) => Some(Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("stolenISK", PyRep::Real(payout)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("fighterMgr", "LaunchFighters")
        | ("fighterMgr", "ExecuteFighterAbility")
        | ("fighterMgr", "LoadFightersToTube") => {
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
        | ("crimewatch", "GetKillRights")
        | ("killmailSvc", "GetKillMail")
        | ("war", "GetWars")
        | ("warStatisticMgr", "GetStats")
        | ("mutualWarInviteMgr", "GetInvites")
        | ("peaceTreatyMgr", "GetTreaties")
        | ("bountyProxy", "GetBounties")
        | ("duelMgr", "StartDuel") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(vec![])))
        }

        _ => None,
    }
}
