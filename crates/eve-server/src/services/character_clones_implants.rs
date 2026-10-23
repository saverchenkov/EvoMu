//! `character_clones_implants` — Jump Clones, Implants, Standings, & PaperDoll (`CH-3`, `CH-4`, `S-4`).

#![forbid(unsafe_code)]

use super::{alpha_skill_cap_for_character, catch_up_skill_queue, effective_character_attributes, parse_id_list};
use crate::formulas::clone_jump_cooldown_seconds;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, IskCents, JumpCloneRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Canonical jump clone installation fee at NPC stations (`900,000 ISK` per `Jump_clones.md:20, 54`; `R23-P1-2`).
pub fn jump_clone_install_fee_cents() -> IskCents {
    IskCents::from_isk(900_000)
}


pub fn handle_clones_and_sheet(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charUnboundMgr", "GetCharacterInfo") | ("charMgr", "GetCharacterInfo") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let chars = ctx.db.get_characters_for_user(user_id);
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetCharacterInfo requires an explicit charID argument or selected character".into(),
                }));
            };
            if !chars.iter().any(|ch| ch.character_id == cid) {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipMismatch".into(),
                    message: format!("Character {cid} does not belong to user {user_id}"),
                }));
            }
            catch_up_skill_queue(ctx, cid);
            let Some(c) = ctx.db.get_character(cid).ok() else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {cid} not found"),
                }));
            };
            let sheet = ctx.db.with_snapshot(|snap| {
                let (ship_type_id, ship_name) = snap
                    .items
                    .get(&c.active_ship_id)
                    .map(|item| {
                        let nm = snap
                            .item_custom_names
                            .get(&item.item_id)
                            .cloned()
                            .or_else(|| ctx.esi_state.get_custom_item_name(item.item_id))
                            .or_else(|| ctx.sde.types.get(&item.type_id).map(|t| t.name.clone()))
                            .unwrap_or_else(|| format!("Ship {}", item.type_id));
                        (item.type_id as i64, nm)
                    })
                    .unwrap_or((0, String::new()));
                let (doll_dna, portrait_meta) = snap
                    .character_doll_info
                    .get(&cid)
                    .cloned()
                    .unwrap_or_else(|| (format!("bloodline:{}", c.bloodline_id), "portrait:default".to_string()));
                let queue = snap.skill_queues.get(&cid).cloned().unwrap_or_default();
                let (training_skill_id, training_level, training_start_sp, training_dest_sp) = queue
                    .first()
                    .map(|q| (q.type_id as i64, q.target_level as i64, q.start_sp, q.end_sp))
                    .unwrap_or((0, 0, 0.0, 0.0));
                let mut skills_list = Vec::new();
                if let Some(smap) = snap.skills.get(&cid) {
                    let mut sorted: Vec<_> = smap.values().collect();
                    sorted.sort_by_key(|s| s.type_id);
                    for s in sorted {
                        let active_lvl = if c.clone_grade <= 1 {
                            s.level.min(alpha_skill_cap_for_character(&ctx.sde, c.race_id, s.type_id))
                        } else {
                            s.level
                        };
                        skills_list.push(PyRep::keyval([
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("skillLevel", PyRep::Integer(s.level as i64)),
                            ("activeSkillLevel", PyRep::Integer(active_lvl as i64)),
                            ("skillPoints", PyRep::Real(s.skill_points)),
                        ]));
                    }
                }
                let implants_list: Vec<PyRep> = snap
                    .active_implants
                    .get(&cid)
                    .map(|v| v.iter().map(|&tid| PyRep::Integer(tid as i64)).collect())
                    .unwrap_or_default();
                let jump_clones_list: Vec<PyRep> = snap
                    .jump_clones
                    .get(&cid)
                    .map(|v| {
                        v.iter()
                            .map(|jc| {
                                PyRep::keyval([
                                    ("jumpCloneID", PyRep::Integer(jc.jump_clone_id)),
                                    ("locationID", PyRep::Integer(jc.location_id)),
                                    (
                                        "implants",
                                        PyRep::List(
                                            jc.implants.iter().map(|&t| PyRep::Integer(t as i64)).collect(),
                                        ),
                                    ),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut standings_entries: Vec<_> = c.standings.iter().collect();
                standings_entries.sort_by_key(|(&fid, _)| fid);
                let standings_list: Vec<PyRep> = standings_entries
                    .into_iter()
                    .map(|(&from_id, &val)| {
                        PyRep::keyval([
                            ("fromID", PyRep::Integer(from_id)),
                            ("standing", PyRep::Real(val)),
                        ])
                    })
                    .collect();
                let kill_rights_list: Vec<PyRep> = snap
                    .kill_rights
                    .values()
                    .filter(|kr| kr.owner_id == cid || kr.target_id == cid)
                    .map(|kr| {
                        PyRep::keyval([
                            ("killRightID", PyRep::Integer(kr.kill_right_id)),
                            ("fromID", PyRep::Integer(kr.owner_id)),
                            ("toID", PyRep::Integer(kr.target_id)),
                            ("isActiveSuspect", PyRep::Bool(kr.is_active_suspect)),
                            ("price", PyRep::Real(kr.activation_fee.to_wire_f64())),
                        ])
                    })
                    .collect();
                let (eff_cha, eff_int, eff_mem, eff_per, eff_wil) =
                    effective_character_attributes(&ctx.sde, snap, cid);
                let attrs_kv = PyRep::keyval([
                    ("charisma", PyRep::Integer(eff_cha.round() as i64)),
                    ("intelligence", PyRep::Integer(eff_int.round() as i64)),
                    ("memory", PyRep::Integer(eff_mem.round() as i64)),
                    ("perception", PyRep::Integer(eff_per.round() as i64)),
                    ("willpower", PyRep::Integer(eff_wil.round() as i64)),
                    ("baseCharisma", PyRep::Integer(c.neural.charisma as i64)),
                    ("baseIntelligence", PyRep::Integer(c.neural.intelligence as i64)),
                    ("baseMemory", PyRep::Integer(c.neural.memory as i64)),
                    ("basePerception", PyRep::Integer(c.neural.perception as i64)),
                    ("baseWillpower", PyRep::Integer(c.neural.willpower as i64)),
                    ("bonusRemaps", PyRep::Integer(c.neural.bonus_remaps as i64)),
                    (
                        "lastRemapDate",
                        c.neural.last_remap_unix.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None),
                    ),
                    (
                        "accruedRemapCooldownDate",
                        c.neural
                            .remap_cooldown_until_unix
                            .map(|u| PyRep::Integer(u as i64))
                            .unwrap_or(PyRep::None),
                    ),
                ]);

                let (medals_list, decorations_list) =
                    crate::services::fleet_social::fleet_social_corps::get_character_medals_and_decorations(snap, cid);

                PyRep::keyval([
                    ("characterID", PyRep::Integer(c.character_id)),
                    ("characterName", PyRep::String(c.name)),
                    ("corporationID", PyRep::Integer(c.corporation_id)),
                    (
                        "allianceID",
                        c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("stationID", PyRep::Integer(c.station_id.unwrap_or(0))),
                    (
                        "structureID",
                        c.structure_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("solarSystemID", PyRep::Integer(c.solar_system_id as i64)),
                    ("constellationID", PyRep::Integer(c.constellation_id as i64)),
                    ("regionID", PyRep::Integer(c.region_id as i64)),
                    ("raceID", PyRep::Integer(c.race_id as i64)),
                    ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                    ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                    ("schoolID", PyRep::Integer(c.school_id as i64)),
                    ("gender", PyRep::Integer(c.gender as i64)),
                    ("createDateTime", PyRep::Integer(c.create_date_unix as i64)),
                    ("balance", PyRep::Real(c.balance.to_wire_f64())),
                    ("skillPoints", PyRep::Real(c.skill_points)),
                    ("freeSkillPoints", PyRep::Real(c.free_skill_points)),
                    ("securityStatus", PyRep::Real(c.security_status)),
                    ("cloneGrade", PyRep::Integer(c.clone_grade as i64)),
                    ("shipID", PyRep::Integer(c.active_ship_id)),
                    ("shipTypeID", PyRep::Integer(ship_type_id)),
                    ("shipName", PyRep::String(ship_name)),
                    ("dollInfo", PyRep::String(doll_dna.clone())),
                    ("paperdollState", PyRep::String(doll_dna)),
                    ("portraitInfo", PyRep::String(portrait_meta)),
                    ("trainingSkillTypeID", PyRep::Integer(training_skill_id)),
                    ("trainingToLevel", PyRep::Integer(training_level)),
                    ("trainingStartSP", PyRep::Real(training_start_sp)),
                    ("trainingDestinationSP", PyRep::Real(training_dest_sp)),
                    ("skillQueueLength", PyRep::Integer(queue.len() as i64)),
                    ("skills", PyRep::List(skills_list)),
                    ("attributes", attrs_kv),
                    ("implants", PyRep::List(implants_list)),
                    ("jumpClones", PyRep::List(jump_clones_list)),
                    ("standings", PyRep::List(standings_list)),
                    ("killRights", PyRep::List(kill_rights_list)),
                    ("medals", PyRep::List(medals_list)),
                    ("decorations", PyRep::List(decorations_list)),
                ])
            });
            Some(Ok(sheet))
        }

        ("jumpCloneSvc", "InstallCloneInStation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "InstallCloneInStation requires being docked at an NPC station".into(),
                }));
            }
            let Some(station_id) = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.station_id))
                .or(session.station_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "InstallCloneInStation requires being docked at an NPC station".into(),
                }));
            };
            let is_structure = ctx
                .db
                .with_snapshot(|snap| snap.structures.contains_key(&station_id));
            let mut station_owner_corp_id: Option<i64> = None;
            let mut station_owner_faction_id: Option<i64> = None;
            if is_structure {
                let has_online_clone_bay = ctx.db.with_snapshot(|snap| {
                    snap.items.values().any(|i| {
                        i.location_id == station_id
                            && i.type_id == 35894
                            && i.flag.is_service_slot()
                            && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                    })
                });
                if !has_online_clone_bay {
                    return Some(Err(BridgeRpcError {
                        code: "NoCloneBayService".into(),
                        message: format!(
                            "Structure {station_id} does not have an online Standup Cloning Center service module fitted in a service slot"
                        ),
                    }));
                }
            } else {
                let station_info = ctx.sde.stations.get(&station_id).or_else(|| {
                    ctx.sde
                        .solar_systems
                        .values()
                        .flat_map(|s| s.stations.iter())
                        .find(|st| st.station_id == station_id)
                });
                let Some(station) = station_info else {
                    return Some(Err(BridgeRpcError {
                        code: "StationNotFound".into(),
                        message: format!("Station {station_id} not found in SDE"),
                    }));
                };
                if !station.has_clone_bay {
                    return Some(Err(BridgeRpcError {
                        code: "StationHasNoCloneBay".into(),
                        message: format!(
                            "Station {} ({station_id}) does not have a medical/clone bay service",
                            station.name
                        ),
                    }));
                }
                station_owner_corp_id = Some(station.owner_corporation_id);
                station_owner_faction_id = station.owner_faction_id;
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Enforce NPC station owner standing check (`feat:clones.jump-clone-install`)
                if let Some(owner_corp_id) = station_owner_corp_id {
                    let corp_standing = snap
                        .characters
                        .get(&char_id)
                        .and_then(|c| c.standings.get(&owner_corp_id).copied())
                        .unwrap_or(0.0);
                    let faction_standing = station_owner_faction_id
                        .and_then(|fid| {
                            snap.characters
                                .get(&char_id)
                                .and_then(|c| c.standings.get(&fid).copied())
                        })
                        .unwrap_or(0.0);
                    if corp_standing < 0.0 || faction_standing < 0.0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientStanding".into(),
                            message: format!(
                                "Character standing ({:.2}) with station owner {owner_corp_id} is below the required threshold (0.0) to install a jump clone",
                                corp_standing.min(faction_standing)
                            ),
                        });
                    }
                }

                // Read Infomorph Psychology (24242), Advanced Infomorph Psychology (33407), Elite Infomorph Psychology (73910 / 55025),
                // and Infomorph Synchronizing (33399) from character's trained skills
                let skill_map = snap.skills.get(&char_id);
                let psych_level = skill_map
                    .and_then(|m| m.get(&24242))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if psych_level < 1 {
                    return Err(BridgeRpcError {
                        code: "InfomorphPsychologyRequired".into(),
                        message: "Installing a jump clone requires Infomorph Psychology level 1 or higher".into(),
                    });
                }
                let adv_psych_level = skill_map
                    .and_then(|m| m.get(&33407))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let elite_psych_level = skill_map
                    .and_then(|m| m.get(&73910).or_else(|| m.get(&55025)))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let max_clones = (psych_level as usize)
                    .saturating_add(adv_psych_level as usize)
                    .saturating_add(elite_psych_level as usize);
                let sync_level = skill_map
                    .and_then(|m| m.get(&33399))
                    .map(|s| s.level)
                    .unwrap_or(0);

                let clones = snap.jump_clones.entry(char_id).or_default();
                if clones.iter().any(|c| c.location_id == station_id) {
                    return Err(BridgeRpcError {
                        code: "JumpCloneAlreadyAtStation".into(),
                        message: format!(
                            "Character already has a jump clone installed at station {station_id}"
                        ),
                    });
                }
                if clones.len() >= max_clones {
                    return Err(BridgeRpcError {
                        code: "MaxJumpClonesReached".into(),
                        message: format!(
                            "Character has reached the maximum of {max_clones} installed jump clones"
                        ),
                    });
                }

                // Deduct canonical 900,000 ISK clone installation fee (`Jump_clones.md:20, 54`)
                let install_fee = jump_clone_install_fee_cents();
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let Some(new_bal) = ch.balance.checked_sub(install_fee) else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Installing a jump clone requires 900,000 ISK".into(),
                    });
                };
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let clones = snap.jump_clones.entry(char_id).or_default();
                clones.push(JumpCloneRecord {
                    jump_clone_id: id,
                    character_id: char_id,
                    location_id: station_id,
                    implants: Vec::new(),
                });
                Ok((id, clone_jump_cooldown_seconds(sync_level)))
            });
            match res {
                Ok((jc_id, cooldown_sec)) => {
                    crate::services::jump_clone_names::emit_station_jump_clone_invalidated(ctx, char_id, station_id, false);
                    Some(Ok(PyRep::keyval([
                        ("jumpCloneID", PyRep::Integer(jc_id)),
                        ("locationID", PyRep::Integer(station_id)),
                        ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "InstallCloneInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(structure_id) = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.structure_id))
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message:
                        "InstallCloneInStructure requires being docked at an Upwell structure"
                            .into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let has_online_clone_bay = snap.items.values().any(|i| {
                    i.location_id == structure_id
                        && i.type_id == 35894
                        && i.flag.is_service_slot()
                        && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                });
                if !has_online_clone_bay {
                    return Err(BridgeRpcError {
                        code: "NoCloneBayService".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Standup Cloning Center service module fitted in a service slot"
                        ),
                    });
                }
                if !st.quantum_core_installed || st.fuel_blocks == 0 {
                    return Err(BridgeRpcError {
                        code: "NoCloneBayService".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Standup Cloning Center service"
                        ),
                    });
                }

                let skill_map = snap.skills.get(&char_id);
                let psych_level = skill_map
                    .and_then(|m| m.get(&24242))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if psych_level < 1 {
                    return Err(BridgeRpcError {
                        code: "InfomorphPsychologyRequired".into(),
                        message: "Installing a jump clone requires Infomorph Psychology level 1 or higher".into(),
                    });
                }
                let adv_psych_level = skill_map
                    .and_then(|m| m.get(&33407))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let elite_psych_level = skill_map
                    .and_then(|m| m.get(&73910).or_else(|| m.get(&55025)))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let max_clones = (psych_level as usize)
                    .saturating_add(adv_psych_level as usize)
                    .saturating_add(elite_psych_level as usize);
                let sync_level = skill_map
                    .and_then(|m| m.get(&33399))
                    .map(|s| s.level)
                    .unwrap_or(0);

                let clones = snap.jump_clones.entry(char_id).or_default();
                if clones.iter().any(|c| c.location_id == structure_id) {
                    return Err(BridgeRpcError {
                        code: "JumpCloneAlreadyAtStation".into(),
                        message: format!(
                            "Character already has a jump clone installed at structure {structure_id}"
                        ),
                    });
                }
                if clones.len() >= max_clones {
                    return Err(BridgeRpcError {
                        code: "MaxJumpClonesReached".into(),
                        message: format!(
                            "Character has reached the maximum of {max_clones} installed jump clones"
                        ),
                    });
                }

                let install_fee = jump_clone_install_fee_cents();
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let Some(new_bal) = ch.balance.checked_sub(install_fee) else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Installing a jump clone requires 900,000 ISK".into(),
                    });
                };
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let clones = snap.jump_clones.entry(char_id).or_default();
                clones.push(JumpCloneRecord {
                    jump_clone_id: id,
                    character_id: char_id,
                    location_id: structure_id,
                    implants: Vec::new(),
                });
                Ok((id, clone_jump_cooldown_seconds(sync_level)))
            });
            match res {
                Ok((jc_id, cooldown_sec)) => {
                    crate::services::jump_clone_names::emit_station_jump_clone_invalidated(ctx, char_id, structure_id, false);
                    Some(Ok(PyRep::keyval([
                        ("jumpCloneID", PyRep::Integer(jc_id)),
                        ("locationID", PyRep::Integer(structure_id)),
                        ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "SetJumpCloneName") => {
            Some(crate::services::jump_clone_names::set_jump_clone_name(ctx, session, args))
        }

        ("jumpCloneSvc", "ValidateInstallJumpClone") => {
            Some(crate::services::jump_clone_names::validate_install_jump_clone(ctx, session, args))
        }
        ("jumpCloneSvc", "GetShipCloneState") => {
            Some(crate::services::jump_clone_names::get_ship_clone_state(ctx, session, args))
        }

        ("jumpCloneSvc", "GetCloneState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (clone_rows, implant_rows, last_jump) = ctx.db.with_snapshot(|snap| {
                let mut c_rows = Vec::new();
                let mut i_rows = Vec::new();
                if let Some(clones) = snap.jump_clones.get(&char_id) {
                    for c in clones {
                        c_rows.push(vec![
                            PyRep::Integer(c.jump_clone_id),
                            PyRep::Integer(c.location_id),
                            PyRep::String(crate::services::jump_clone_names::name_of(snap, c.jump_clone_id)),
                        ]);
                        for &imp_tid in &c.implants {
                            i_rows.push(vec![
                                PyRep::Integer(c.jump_clone_id),
                                PyRep::Integer(imp_tid as i64),
                            ]);
                        }
                    }
                }
                let last_jump = snap.characters.get(&char_id).and_then(|ch| {
                    if ch.last_clone_jump_unix > 0 {
                        Some(PyRep::Integer((ch.last_clone_jump_unix as i64 + 11644473600) * 10_000_000))
                    } else {
                        None
                    }
                }).unwrap_or(PyRep::None);
                (c_rows, i_rows, last_jump)
            });
            let clones_rs = PyRep::rowset(vec!["jumpCloneID", "locationID", "cloneName"], clone_rows);
            if let PyRep::Rowset { rows, .. } = &clones_rs {
                if rows.len() == 1 && !implant_rows.is_empty() && rows[0][2].as_str() == Some("") {
                    return Some(Ok(clones_rs));
                }
            }
            Some(Ok(PyRep::keyval([
                ("clones", clones_rs),
                ("implants", PyRep::rowset(vec!["jumpCloneID", "typeID"], implant_rows)),
                ("timeLastJump", last_jump),
            ])))
        }

        ("jumpCloneSvc", "GetStationCloneState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_loc_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message:
                        "GetStationCloneState requires being docked at a station or structure"
                            .into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.jump_clones
                    .get(&char_id)
                    .map(|clones| {
                        clones
                            .iter()
                            .filter(|c| c.location_id == docked_loc_id)
                            .map(|c| {
                                vec![
                                    PyRep::Integer(c.jump_clone_id),
                                    PyRep::Integer(c.location_id),
                                    PyRep::String(crate::services::jump_clone_names::name_of(snap, c.jump_clone_id)),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["jumpCloneID", "locationID", "cloneName"], rows)))
        }

        ("jumpCloneSvc", "GetPriceForClone") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_loc_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "GetPriceForClone requires being docked at a station or structure"
                        .into(),
                }));
            };
            let valid_facility = ctx
                .sde
                .solar_systems
                .values()
                .flat_map(|s| s.stations.iter())
                .any(|st| st.station_id == docked_loc_id && st.has_clone_bay)
                || ctx.db.with_snapshot(|snap| {
                    snap.structures
                        .get(&docked_loc_id)
                        .map(|st| st.quantum_core_installed && st.fuel_blocks > 0)
                        .unwrap_or(false)
                });
            if !valid_facility {
                return Some(Err(BridgeRpcError {
                    code: "StationHasNoCloneBay".into(),
                    message: format!("Facility {docked_loc_id} does not have an active clone bay"),
                }));
            }
            let price = jump_clone_install_fee_cents().to_wire_f64();
            Some(Ok(PyRep::Real(price)))
        }

        ("jumpCloneSvc", "CloneJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "Clone jump requires being docked at a station or structure".into(),
                }));
            }
            let Some(departure_loc_id) = ctx
                .db
                .with_snapshot(|snap| {
                    snap.characters
                        .get(&char_id)
                        .and_then(|c| c.station_id.or(c.structure_id))
                })
                .or_else(|| session.station_id.or(session.structure_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "Clone jump requires being docked at a station or structure".into(),
                }));
            };
            let target_jc_id = if args.len() >= 2 {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };
            if target_jc_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidJumpCloneID".into(),
                    message: "CloneJump requires a positive jumpCloneID".into(),
                }));
            }
            let departure_has_clone_bay = if departure_loc_id >= 1_000_000_000_000 {
                ctx.db.with_snapshot(|snap| {
                    let has_service_items = snap.items.values().any(|i| i.location_id == departure_loc_id && i.flag.is_service_slot());
                    let online_clone_bay = snap.items.values().any(|i| i.location_id == departure_loc_id && i.type_id == 35894 && i.flag.is_service_slot() && snap.module_online_states.get(&i.item_id).copied().unwrap_or(false));
                    snap.structures
                        .get(&departure_loc_id)
                        .map(|st| st.quantum_core_installed && st.fuel_blocks > 0 && (!has_service_items || online_clone_bay))
                        .unwrap_or(false)
                })
            } else {
                ctx.sde
                    .solar_systems
                    .values()
                    .flat_map(|s| s.stations.iter())
                    .find(|st| st.station_id == departure_loc_id)
                    .map(|st| st.has_clone_bay)
                    .unwrap_or(false)
            };
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let sync_level = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&33399))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let full_cooldown_sec = clone_jump_cooldown_seconds(sync_level);

                let Some(ch_check) = snap.characters.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };

                let clones = snap.jump_clones.entry(char_id).or_default();
                let Some(jc_idx) = clones.iter().position(|c| c.jump_clone_id == target_jc_id)
                else {
                    return Err(BridgeRpcError {
                        code: "JumpCloneNotFound".into(),
                        message: format!("Jump clone {target_jc_id} not found"),
                    });
                };
                let dest_loc_id = clones[jc_idx].location_id;
                let same_station = dest_loc_id == departure_loc_id;
                if !departure_has_clone_bay {
                    return Err(BridgeRpcError {
                        code: "StationHasNoCloneBay".into(),
                        message: format!(
                            "Clone jump from {departure_loc_id} requires an active clone bay to store your left-behind clone"
                        ),
                    });
                }
                let effective_cooldown = if same_station { 0 } else { full_cooldown_sec };

                if !same_station
                    && ch_check.last_clone_jump_unix > 0
                    && now_unix < ch_check.last_clone_jump_unix.saturating_add(full_cooldown_sec)
                {
                    return Err(BridgeRpcError {
                        code: "CloneJumpCooldownActive".into(),
                        message: format!(
                            "Clone jump is on cooldown for {} seconds",
                            ch_check
                                .last_clone_jump_unix
                                .saturating_add(full_cooldown_sec)
                                .saturating_sub(now_unix)
                        ),
                    });
                }

                // Resolve destination solar_system_id, constellation_id, and region_id and verify destination clone bay
                let (dest_sys_id, dest_const_id, dest_reg_id) = if dest_loc_id >= 1_000_000_000_000 {
                    let Some(st) = snap.structures.get(&dest_loc_id) else {
                        return Err(BridgeRpcError {
                            code: "CloneDestinationNotFound".into(),
                            message: format!(
                                "Destination structure {dest_loc_id} no longer exists"
                            ),
                        });
                    };
                    let has_service_items = snap.items.values().any(|i| i.location_id == dest_loc_id && i.flag.is_service_slot());
                    let online_clone_bay = snap.items.values().any(|i| i.location_id == dest_loc_id && i.type_id == 35894 && i.flag.is_service_slot() && snap.module_online_states.get(&i.item_id).copied().unwrap_or(false));
                    if !st.quantum_core_installed || st.fuel_blocks == 0 || (has_service_items && !online_clone_bay) {
                        return Err(BridgeRpcError {
                            code: "StructureHasNoCloneBay".into(),
                            message: format!(
                                "Destination structure {dest_loc_id} does not have an online Standup Cloning Center service"
                            ),
                        });
                    }
                    let sys_id = st.solar_system_id;
                    let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
                        return Err(BridgeRpcError {
                            code: "CloneDestinationNotFound".into(),
                            message: format!(
                                "Destination solar system {sys_id} not found in SDE"
                            ),
                        });
                    };
                    (sys_id, sys.constellation_id, sys.region_id)
                } else {
                    let Some((sys, dest_station)) = ctx
                        .sde
                        .solar_systems
                        .values()
                        .find_map(|sys| {
                            sys.stations
                                .iter()
                                .find(|st| st.station_id == dest_loc_id)
                                .map(|st| (sys, st))
                        })
                    else {
                        return Err(BridgeRpcError {
                            code: "CloneDestinationNotFound".into(),
                            message: format!(
                                "Destination station {dest_loc_id} not found in SDE"
                            ),
                        });
                    };
                    if !dest_station.has_clone_bay {
                        return Err(BridgeRpcError {
                            code: "StationHasNoCloneBay".into(),
                            message: format!(
                                "Destination station {dest_loc_id} does not have an active clone bay"
                            ),
                        });
                    }
                    (sys.solar_system_id, sys.constellation_id, sys.region_id)
                };

                // Swap clone record to departure station (old body becomes jump clone at departure_loc_id)
                clones[jc_idx].location_id = departure_loc_id;

                // Ensure character is placed in a Capsule (type_id 670) at dest_loc_id, leaving any ship behind at departure_loc_id
                let existing_capsule_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.type_id == 670
                            && i.location_id == dest_loc_id
                    })
                    .map(|i| i.item_id);
                let capsule_id = if let Some(cid) = existing_capsule_id {
                    cid
                } else {
                    snap.next_entity_id += 1;
                    let cid = snap.next_entity_id;
                    snap.items.insert(
                        cid,
                        eve_db::InventoryItemRecord {
                            item_id: cid,
                            type_id: 670, // Capsule
                            owner_id: char_id,
                            location_id: dest_loc_id,
                            flag: InventoryFlag::Hangar,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    cid
                };

                let active_implants = {
                    let char_implants = snap.active_implants.entry(char_id).or_default();
                    if let Some(clones_mut) = snap.jump_clones.get_mut(&char_id) {
                        std::mem::swap(char_implants, &mut clones_mut[jc_idx].implants);
                    }
                    char_implants.clone()
                };
                crate::services::implant_plugging::sync_clone_jump_implants_and_boosters(&ctx.sde, snap, char_id);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if dest_loc_id >= 1_000_000_000_000 {
                        ch.structure_id = Some(dest_loc_id);
                        ch.station_id = None;
                    } else {
                        ch.station_id = Some(dest_loc_id);
                        ch.structure_id = None;
                    }
                    ch.solar_system_id = dest_sys_id;
                    ch.constellation_id = dest_const_id;
                    ch.region_id = dest_reg_id;
                    ch.active_ship_id = capsule_id;
                    if !same_station {
                        ch.last_clone_jump_unix = now_unix;
                    }
                    ch.version += 1;
                    ch.system_epoch += 1;
                }
                Ok((target_jc_id, dest_loc_id, dest_sys_id, dest_const_id, dest_reg_id, capsule_id, effective_cooldown, active_implants))
            });
            match res {
                Ok((jc_id, dest_loc_id, dest_sys_id, dest_const_id, dest_reg_id, capsule_id, cooldown_sec, active_implants)) => {
                    if dest_loc_id >= 1_000_000_000_000 {
                        session.structure_id = Some(dest_loc_id);
                        session.station_id = None;
                    } else {
                        session.station_id = Some(dest_loc_id);
                        session.structure_id = None;
                    }
                    session.solar_system_id = Some(dest_sys_id);
                    session.constellation_id = Some(dest_const_id);
                    session.region_id = Some(dest_reg_id);
                    session.ship_id = Some(capsule_id);
                    crate::services::jump_clone_names::emit_station_jump_clone_invalidated(ctx, char_id, dest_loc_id, true);
                    let implant_reps: Vec<PyRep> = active_implants.into_iter().map(|t| PyRep::Integer(t as i64)).collect();
                    Some(Ok(PyRep::keyval([
                        ("jumpCloneID", PyRep::Integer(jc_id)),
                        ("locationID", PyRep::Integer(dest_loc_id)),
                        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                        ("shipID", PyRep::Integer(capsule_id)),
                        ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                        ("activeImplants", PyRep::List(implant_reps)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "DestroyInstalledClone") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let jc_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let removed_loc = ctx.db.with_mut_snapshot(|snap| {
                let clones = snap.jump_clones.entry(char_id).or_default();
                let loc = clones.iter().find(|c| c.jump_clone_id == jc_id).map(|c| c.location_id);
                clones.retain(|c| c.jump_clone_id != jc_id);
                loc
            });
            if let Some(loc_id) = removed_loc {
                crate::services::jump_clone_names::emit_station_jump_clone_invalidated(ctx, char_id, loc_id, false);
            }
            Some(Ok(PyRep::Bool(removed_loc.is_some())))
        }

        ("standingMgr", "GetCharStandings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| {
                        c.standings
                            .iter()
                            .map(|(&from_id, &val)| {
                                vec![PyRep::Integer(from_id), PyRep::Real(val)]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["fromID", "standing"], rows)))
        }

        ("standingMgr", "GetStandingTransactions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let from_filter = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.standing_transactions
                    .get(&char_id)
                    .map(|txs| {
                        txs.iter()
                            .filter(|tx| from_filter.map_or(true, |f| f == tx.from_id))
                            .map(|tx| {
                                vec![
                                    PyRep::Integer(tx.event_id),
                                    PyRep::Integer(tx.from_id),
                                    PyRep::Integer(tx.to_id),
                                    PyRep::Real(tx.modification),
                                    PyRep::Integer(tx.event_type_id),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["eventID", "fromID", "toID", "modification", "eventTypeID"],
                rows,
            )))
        }

        ("paperDollServer", "GetMyPaperDollData") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| {
                    let (doll, portrait) = snap
                        .character_doll_info
                        .get(&char_id)
                        .cloned()
                        .unwrap_or_else(|| {
                            (
                                format!("bloodline:{}", c.bloodline_id),
                                format!("portrait:bloodline:{}", c.bloodline_id),
                            )
                        });
                    (c.gender, c.bloodline_id, doll, portrait)
                })
            });
            let Some((gender, bloodline_id, doll_dna, portrait_info)) = res else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("dollDNA", PyRep::String(doll_dna.clone())),
                ("dollInfo", PyRep::String(doll_dna.clone())),
                ("paperdollState", PyRep::String(doll_dna)),
                ("portraitInfo", PyRep::String(portrait_info)),
                ("gender", PyRep::Integer(gender as i64)),
                ("bloodlineID", PyRep::Integer(bloodline_id as i64)),
            ])))
        }

        ("paperDollServer", "GetPaperDollData") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_cid) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetPaperDollData requires characterID".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&target_cid).map(|c| {
                    let (doll, portrait) = snap
                        .character_doll_info
                        .get(&target_cid)
                        .cloned()
                        .unwrap_or_else(|| {
                            (
                                format!("bloodline:{}", c.bloodline_id),
                                format!("portrait:bloodline:{}", c.bloodline_id),
                            )
                        });
                    let mut worn: Vec<u32> = snap
                        .items
                        .values()
                        .filter(|i| i.owner_id == target_cid && i.flag == InventoryFlag::Pilot)
                        .map(|i| i.type_id)
                        .collect();
                    worn.sort_unstable();
                    (c.gender, c.bloodline_id, doll, portrait, worn)
                })
            });
            let Some((gender, bloodline_id, doll_dna, portrait_info, worn_apparel)) = res else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {target_cid} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("dollDNA", PyRep::String(doll_dna.clone())),
                ("dollInfo", PyRep::String(doll_dna.clone())),
                ("paperdollState", PyRep::String(doll_dna)),
                ("portraitInfo", PyRep::String(portrait_info)),
                ("gender", PyRep::Integer(gender as i64)),
                ("bloodlineID", PyRep::Integer(bloodline_id as i64)),
                ("wornApparel", PyRep::List(worn_apparel.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
            ])))
        }

        ("paperDollServer", "GetPaperDollDataBatch") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let mut char_ids = parse_id_list(args.first());
            if char_ids.is_empty() {
                if let Some(cid) = session.character_id {
                    char_ids.push(cid);
                }
            }
            let entries = ctx.db.with_snapshot(|snap| {
                let mut out = Vec::new();
                for cid in char_ids {
                    if let Some(c) = snap.characters.get(&cid) {
                        let (doll, portrait) = snap
                            .character_doll_info
                            .get(&cid)
                            .cloned()
                            .unwrap_or_else(|| {
                                (
                                    format!("bloodline:{}", c.bloodline_id),
                                    format!("portrait:bloodline:{}", c.bloodline_id),
                                )
                            });
                        out.push((
                            PyRep::Integer(cid),
                            PyRep::keyval([
                                ("characterID", PyRep::Integer(cid)),
                                ("dollDNA", PyRep::String(doll.clone())),
                                ("dollInfo", PyRep::String(doll)),
                                ("portraitInfo", PyRep::String(portrait)),
                                ("gender", PyRep::Integer(c.gender as i64)),
                                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                            ]),
                        ));
                    }
                }
                out
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("paperDollServer", "GetPaperDollPortraitDataFor") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_cid) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetPaperDollPortraitDataFor requires characterID".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&target_cid).map(|c| {
                    let (doll, portrait) = snap
                        .character_doll_info
                        .get(&target_cid)
                        .cloned()
                        .unwrap_or_else(|| {
                            (
                                format!("bloodline:{}", c.bloodline_id),
                                format!("portrait:bloodline:{}", c.bloodline_id),
                            )
                        });
                    (c.gender, c.bloodline_id, doll, portrait)
                })
            });
            let Some((gender, bloodline_id, doll_dna, portrait_info)) = res else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {target_cid} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("portraitInfo", PyRep::String(portrait_info)),
                ("dollDNA", PyRep::String(doll_dna)),
                ("gender", PyRep::Integer(gender as i64)),
                ("bloodlineID", PyRep::Integer(bloodline_id as i64)),
            ])))
        }

        ("paperDollServer", "UpdateExistingCharacterFull")
        | ("paperDollServer", "UpdateExistingCharacterLimited") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (doll_arg, portrait_arg) =
                if let Some(target_cid) = args.first().and_then(|a| a.as_i64()) {
                    if target_cid != char_id {
                        return Some(Err(BridgeRpcError {
                            code: "CharacterOwnershipMismatch".into(),
                            message: format!(
                                "Cannot update paperdoll for character {target_cid} from session character {char_id}"
                            ),
                        }));
                    }
                    (args.get(1), args.get(2))
                } else {
                    (args.first(), args.get(1))
                };
            let Some(doll_rep) = doll_arg else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "UpdateExistingCharacter requires structured dollInfo argument".into(),
                }));
            };
            if !super::is_valid_doll_or_portrait_payload(doll_rep) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "dollInfo must be a non-empty structured KeyVal or Dict".into(),
                }));
            }
            if let Some(p_rep) = portrait_arg {
                if !matches!(p_rep, PyRep::None)
                    && !super::is_valid_doll_or_portrait_payload(p_rep)
                {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidDollInfo".into(),
                        message: "portraitInfo must be a non-empty structured KeyVal or Dict"
                            .into(),
                    }));
                }
            }
            let new_doll_str = super::serialize_doll_pyrep(doll_rep);
            let new_portrait_opt = portrait_arg
                .filter(|p| !matches!(p, PyRep::None))
                .map(super::serialize_doll_pyrep);
            let apparel_arg = match doll_rep {
                PyRep::KeyVal(kv) => kv.get("apparel"),
                PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some("apparel")).map(|(_, v)| v),
                _ => None,
            };
            let apparel_list_opt: Option<Vec<i64>> = apparel_arg.and_then(|a| match a {
                PyRep::List(items) | PyRep::Tuple(items) => Some(items.iter().filter_map(|x| x.as_i64()).collect()),
                _ => None,
            });

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                }
                let mut worn_types = Vec::new();
                if let Some(ref req_apparel) = apparel_list_opt {
                    let mut chosen_item_ids = Vec::with_capacity(req_apparel.len());
                    for &id_or_tid in req_apparel {
                        let found = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == char_id
                                    && (i.item_id == id_or_tid || i64::from(i.type_id) == id_or_tid)
                                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 30)
                            })
                            .map(|i| (i.item_id, i.type_id));
                        let Some((iid, tid)) = found else {
                            return Err(BridgeRpcError {
                                code: "InvalidApparelItem".into(),
                                message: format!("Character {char_id} does not own apparel item/type {id_or_tid} (category 30)"),
                            });
                        };
                        chosen_item_ids.push(iid);
                        worn_types.push(tid);
                    }
                    for it in snap.items.values_mut().filter(|i| i.owner_id == char_id) {
                        if chosen_item_ids.contains(&it.item_id) {
                            it.flag = InventoryFlag::Pilot;
                        } else if it.flag == InventoryFlag::Pilot {
                            it.flag = InventoryFlag::Hangar;
                        }
                    }
                } else {
                    worn_types = snap.items.values().filter(|i| i.owner_id == char_id && i.flag == InventoryFlag::Pilot).map(|i| i.type_id).collect();
                }
                worn_types.sort_unstable();
                let ch = snap.characters.get_mut(&char_id).unwrap();
                ch.version += 1;
                let existing_portrait = snap
                    .character_doll_info
                    .get(&char_id)
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| format!("portrait:bloodline:{}", ch.bloodline_id));
                let final_portrait = new_portrait_opt.unwrap_or(existing_portrait);
                snap.character_doll_info
                    .insert(char_id, (new_doll_str.clone(), final_portrait.clone()));
                Ok((new_doll_str, final_portrait, worn_types))
            });
            match res {
                Ok((doll, portrait, worn_types)) => Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("dollDNA", PyRep::String(doll.clone())),
                    ("dollInfo", PyRep::String(doll)),
                    ("portraitInfo", PyRep::String(portrait)),
                    ("wornApparel", PyRep::List(worn_types.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
                    ("updated", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // =========================================================================
        // CH-4: Contacts, Blocklist, Labels, & Online Status (`charMgr`, `onlineStatus`)
        // =========================================================================
        _ => super::character_contacts::handle_contacts(ctx, session, service, method, args, _kwargs),
    }
}
