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
                    let st_powered = snap
                        .structures
                        .get(&station_id)
                        .is_some_and(|st| st.quantum_core_installed && st.fuel_blocks > 0);
                    st_powered
                        && snap.items.values().any(|i| {
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
                if !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 10)
                    && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 24)
                {
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
                if station_id == 60_015_187 {
                    let war_fac = snap.characters.get(&char_id).and_then(|c| c.war_faction_id);
                    if !matches!(war_fac, Some(500_010 | 500_011)) {
                        return Err(BridgeRpcError {
                            code: "FulcrumCloneRequiresPirateEnlistment".into(),
                            message: "Clone installation at The Fulcrum requires pirate militia enlistment".into(),
                        });
                    }
                }
                // Enforce NPC station owner standing check (`feat:clones.jump-clone-install`)
                crate::services::character_clone_jump_rules::check_alpha_clone_install_capacity(snap, char_id)?;
                let custom_acl_fee = if is_structure {
                    crate::services::character_clone_jump_rules::check_structure_clone_bay_access_at(
                        snap,
                        station_id,
                        char_id,
                        Some(ctx.now_unix()),
                    )?
                } else {
                    None
                };
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

                // Deduct canonical 900,000 ISK clone installation fee (`Jump_clones.md:20, 54`), or per-ACL-group fee on structures
                let install_fee = custom_acl_fee.unwrap_or_else(jump_clone_install_fee_cents);
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let Some(new_bal) = ch.balance.checked_sub(install_fee) else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!("Installing a jump clone requires {} ISK", install_fee.to_wire_f64()),
                    });
                };
                ch.balance = new_bal;
                if install_fee.0 > 0 {
                    snap.append_journal(
                        char_id,
                        "JumpCloneInstallationFee",
                        IskCents(-install_fee.0),
                        new_bal,
                        format!("Installed jump clone at station {station_id}"),
                    );
                    if is_structure {
                        if let Some(owner_corp_id) = snap.structures.get(&station_id).map(|s| s.owner_corp_id) {
                            crate::services::station_structure::structure_upwell_ops::credit_structure_owner_corp_div1(
                                snap, owner_corp_id, install_fee, 10, "JumpCloneInstallationFee", char_id,
                                format!("Jump clone installation fee from {char_id} at structure {station_id}"), ctx.now_unix(),
                            );
                        }
                    }
                }

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
                crate::services::character_clone_jump_rules::check_alpha_clone_install_capacity(snap, char_id)?;
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
                let custom_acl_fee =
                    crate::services::character_clone_jump_rules::check_structure_clone_bay_access_at(
                        snap,
                        structure_id,
                        char_id,
                        Some(ctx.now_unix()),
                    )?;

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

                let install_fee = custom_acl_fee.unwrap_or_else(jump_clone_install_fee_cents);
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let Some(new_bal) = ch.balance.checked_sub(install_fee) else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!("Installing a jump clone requires {} ISK", install_fee.to_wire_f64()),
                    });
                };
                ch.balance = new_bal;
                if install_fee.0 > 0 {
                    snap.append_journal(
                        char_id,
                        "JumpCloneInstallationFee",
                        IskCents(-install_fee.0),
                        new_bal,
                        format!("Installed jump clone at structure {structure_id}"),
                    );
                    if let Some(owner_corp_id) = snap.structures.get(&structure_id).map(|s| s.owner_corp_id) {
                        crate::services::station_structure::structure_upwell_ops::credit_structure_owner_corp_div1(
                            snap, owner_corp_id, install_fee, 10, "JumpCloneInstallationFee", char_id,
                            format!("Jump clone installation fee from {char_id} at structure {structure_id}"), ctx.now_unix(),
                        );
                    }
                }

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

        ("jumpCloneSvc", "ResetLastCloneJumpTime") => {
            Some(crate::services::sov_services::handle_reset_last_clone_jump_time(ctx, session))
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
            crate::services::character_clone_jump_rules::handle_clone_jump(ctx, session, args)
        }

        ("jumpCloneSvc", "DestroyInstalledClone") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let jc_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if jc_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidJumpCloneID".into(),
                    message: "DestroyInstalledClone requires a positive jump clone ID".into(),
                }));
            }
            let removed_loc = ctx.db.with_mut_snapshot(|snap| {
                let clones = snap.jump_clones.entry(char_id).or_default();
                let loc = clones.iter().find(|c| c.jump_clone_id == jc_id).map(|c| c.location_id);
                clones.retain(|c| c.jump_clone_id != jc_id);
                loc
            });
            let Some(loc_id) = removed_loc else {
                return Some(Err(BridgeRpcError {
                    code: "CloneNotFound".into(),
                    message: format!("Jump clone {jc_id} not found for character {char_id}"),
                }));
            };
            crate::services::jump_clone_names::emit_station_jump_clone_invalidated(ctx, char_id, loc_id, false);
            Some(Ok(PyRep::Bool(true)))
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
                    let mut worn: Vec<u32> = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.owner_id == char_id
                                && matches!(i.flag, InventoryFlag::Wardrobe | InventoryFlag::Pilot)
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 30)
                        })
                        .map(|i| i.type_id)
                        .collect();
                    worn.sort_unstable();
                    (c.gender, c.bloodline_id, doll, portrait, worn)
                })
            });
            let Some((gender, bloodline_id, doll_dna, portrait_info, worn_apparel)) = res else {
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
                ("wornApparel", PyRep::List(worn_apparel.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
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
                        .filter(|i| {
                            i.owner_id == target_cid
                                && matches!(i.flag, InventoryFlag::Wardrobe | InventoryFlag::Pilot)
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 30)
                        })
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
            let gender_label = if gender == 0 { "female" } else { "male" };
            let details = PyRep::keyval([
                ("characterID", PyRep::Integer(target_cid)),
                ("dollDNA", PyRep::String(doll_dna.clone())),
                ("dollInfo", PyRep::String(doll_dna.clone())),
                ("paperdollState", PyRep::String(doll_dna)),
                ("portraitInfo", PyRep::String(portrait_info)),
                ("gender", PyRep::Integer(gender as i64)),
                ("bloodlineID", PyRep::Integer(bloodline_id as i64)),
                ("wornApparel", PyRep::List(worn_apparel.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
            ]);
            Some(Ok(PyRep::List(vec![
                PyRep::String(gender_label.into()),
                details,
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
                        let mut worn: Vec<u32> = snap
                            .items
                            .values()
                            .filter(|i| {
                                i.owner_id == cid
                                    && matches!(i.flag, InventoryFlag::Wardrobe | InventoryFlag::Pilot)
                                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 30)
                            })
                            .map(|i| i.type_id)
                            .collect();
                        worn.sort_unstable();
                        out.push((
                            PyRep::Integer(cid),
                            PyRep::keyval([
                                ("characterID", PyRep::Integer(cid)),
                                ("dollDNA", PyRep::String(doll.clone())),
                                ("dollInfo", PyRep::String(doll)),
                                ("portraitInfo", PyRep::String(portrait)),
                                ("gender", PyRep::Integer(c.gender as i64)),
                                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                                ("wornApparel", PyRep::List(worn.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
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
            let (doll_arg, portrait_arg) = match args.first().and_then(|a| a.as_i64()) {
                Some(tid) if tid != char_id => return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipMismatch".into(),
                    message: format!("Cannot update paperdoll for character {tid} from session {char_id}"),
                })),
                Some(_) => (args.get(1), args.get(2)),
                None => (args.first(), args.get(1)),
            };
            let Some(doll_rep) = doll_arg else {
                return Some(Err(BridgeRpcError { code: "InvalidDollInfo".into(), message: "UpdateExistingCharacter requires structured dollInfo argument".into() }));
            };
            if !super::is_valid_doll_or_portrait_payload(doll_rep) {
                return Some(Err(BridgeRpcError { code: "InvalidDollInfo".into(), message: "dollInfo must be a non-empty structured KeyVal or Dict".into() }));
            }
            if let Some(p_rep) = portrait_arg {
                if !matches!(p_rep, PyRep::None) && !super::is_valid_doll_or_portrait_payload(p_rep) {
                    return Some(Err(BridgeRpcError { code: "InvalidDollInfo".into(), message: "portraitInfo must be a non-empty structured KeyVal or Dict".into() }));
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
                if method == "UpdateExistingCharacterFull" {
                    let mut store = crate::services::account_services::load_store(snap);
                    let uid = session.user_id.unwrap_or(0);
                    let account_tokens = snap.service_state_blobs.get(&format!("account_resculpt_tokens:{uid}")).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    let is_pending = store.resculpt_pending.contains(&char_id)
                        || snap.service_state_blobs.get(&format!("resculpt_pending:{char_id}")).map(|s| s == "1" || s == "true").unwrap_or(false)
                        || account_tokens > 0
                        || session.has_gm_role();
                    if !is_pending {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "NoResculptAvailable".into(),
                        });
                    }
                    store.resculpt_pending.remove(&char_id);
                    if let Ok(j) = serde_json::to_string(&store) {
                        snap.item_custom_names.insert(crate::services::account_services::ACCOUNT_SERVICES_STORE_KEY, j);
                    }
                    snap.service_state_blobs.remove(&format!("resculpt_pending:{char_id}"));
                    if account_tokens > 0 {
                        snap.service_state_blobs.insert(format!("account_resculpt_tokens:{uid}"), (account_tokens - 1).to_string());
                    }
                }
                let mut worn_types = Vec::new();
                let mut item_changes: Vec<(eve_db::InventoryItemRecord, i64, i64, i32)> = Vec::new();
                if let Some(ref req_apparel) = apparel_list_opt {
                    let docked_loc = session
                        .station_id
                        .or(session.structure_id)
                        .ok_or_else(|| BridgeRpcError {
                            code: "NotDocked".into(),
                            message: "Must be docked at a station or structure to equip or unequip apparel".into(),
                        })?;
                    let mut equipped_item_ids: Vec<i64> = Vec::with_capacity(req_apparel.len());
                    let mut seen_requests: std::collections::HashSet<i64> =
                        std::collections::HashSet::with_capacity(req_apparel.len());
                    let mut consumed_source_ids: std::collections::HashSet<i64> =
                        std::collections::HashSet::with_capacity(req_apparel.len());
                    let mut occupied_apparel_groups: std::collections::HashSet<u32> =
                        std::collections::HashSet::with_capacity(req_apparel.len());
                    for &id_or_tid in req_apparel {
                        if !seen_requests.insert(id_or_tid) {
                            return Err(BridgeRpcError {
                                code: "InvalidApparelItem".into(),
                                message: format!("Duplicate apparel entry {id_or_tid} in request"),
                            });
                        }
                        let mut candidates: Vec<(i64, bool, bool)> = snap
                            .items
                            .values()
                            .filter(|i| {
                                i.owner_id == char_id
                                    && i.quantity > 0
                                    && !equipped_item_ids.contains(&i.item_id)
                                    && !consumed_source_ids.contains(&i.item_id)
                                    && (i.item_id == id_or_tid || i64::from(i.type_id) == id_or_tid)
                                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 30)
                            })
                            .map(|i| {
                                let already_worn = matches!(i.flag, InventoryFlag::Wardrobe | InventoryFlag::Pilot)
                                    && (i.location_id == char_id || i.location_id == docked_loc);
                                let in_local_hangar =
                                    i.location_id == docked_loc && i.flag == InventoryFlag::Hangar;
                                let locked = snap.is_item_locked_in_contract_or_job(i.item_id);
                                (i.item_id, already_worn || in_local_hangar, locked)
                            })
                            .collect();
                        candidates.sort_by_key(|&(iid, valid_loc, locked)| (!valid_loc, locked, iid));
                        let Some(&(iid, valid_loc, locked)) = candidates.first() else {
                            return Err(BridgeRpcError {
                                code: "InvalidApparelItem".into(),
                                message: format!("Character {char_id} does not own apparel item/type {id_or_tid} (category 30)"),
                            });
                        };
                        if !valid_loc {
                            return Err(BridgeRpcError {
                                code: "InvalidApparelItem".into(),
                                message: format!("Apparel item/type {id_or_tid} is not in current docked hangar ({docked_loc}) or wardrobe"),
                            });
                        }
                        if locked {
                            return Err(BridgeRpcError {
                                code: "ItemLockedInContractOrJob".into(),
                                message: format!("Apparel item/type {id_or_tid} is locked in an open contract or industry job"),
                            });
                        }
                        let src = snap.items.get(&iid).unwrap().clone();
                        let apparel_group = ctx.sde.types.get(&src.type_id).map(|t| t.group_id).unwrap_or(0);
                        if apparel_group > 0 && !occupied_apparel_groups.insert(apparel_group) {
                            return Err(BridgeRpcError {
                                code: "InvalidApparelItem".into(),
                                message: format!("Cannot equip multiple garments in wearable group {apparel_group}"),
                            });
                        }
                        consumed_source_ids.insert(iid);
                        worn_types.push(src.type_id);
                        if src.quantity > 1 {
                            if let Some(it) = snap.items.get_mut(&iid) {
                                let old_qty = it.quantity;
                                it.quantity -= 1;
                                it.version += 1;
                                item_changes.push((it.clone(), src.location_id, src.flag as i64, old_qty));
                            }
                            let max_existing = snap.items.keys().copied().max().unwrap_or(0);
                            let split_id = snap
                                .next_entity_id
                                .max(1_000_000_000)
                                .max(max_existing.saturating_add(1));
                            snap.next_entity_id = split_id.saturating_add(1);
                            let worn_unit = eve_db::InventoryItemRecord {
                                item_id: split_id,
                                type_id: src.type_id,
                                owner_id: char_id,
                                location_id: char_id,
                                flag: InventoryFlag::Wardrobe,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                ..Default::default()
                            };
                            snap.items.insert(split_id, worn_unit.clone());
                            item_changes.push((worn_unit, src.location_id, src.flag as i64, 0));
                            equipped_item_ids.push(split_id);
                        } else if let Some(it) = snap.items.get_mut(&iid) {
                            let old_loc = it.location_id;
                            let old_flag = it.flag as i64;
                            let old_qty = it.quantity;
                            if it.location_id != char_id || it.flag != InventoryFlag::Wardrobe {
                                it.location_id = char_id;
                                it.flag = InventoryFlag::Wardrobe;
                                it.singleton = true;
                                it.version += 1;
                                item_changes.push((it.clone(), old_loc, old_flag, old_qty));
                            }
                            equipped_item_ids.push(iid);
                        }
                    }
                    let unequip_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|it| {
                            it.owner_id == char_id
                                && !equipped_item_ids.contains(&it.item_id)
                                && matches!(it.flag, InventoryFlag::Wardrobe | InventoryFlag::Pilot)
                                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 30)
                        })
                        .map(|it| it.item_id)
                        .collect();
                    for uid in unequip_ids {
                        if let Some(it) = snap.items.get_mut(&uid) {
                            let old_loc = it.location_id;
                            let old_flag = it.flag as i64;
                            let old_qty = it.quantity;
                            it.location_id = docked_loc;
                            it.flag = InventoryFlag::Hangar;
                            it.version += 1;
                            item_changes.push((it.clone(), old_loc, old_flag, old_qty));
                        }
                    }
                } else {
                    worn_types = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.owner_id == char_id
                                && matches!(i.flag, InventoryFlag::Wardrobe | InventoryFlag::Pilot)
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 30)
                        })
                        .map(|i| i.type_id)
                        .collect();
                }
                worn_types.sort_unstable();
                let ch = snap.characters.get_mut(&char_id).unwrap();
                ch.version += 1;
                let existing_portrait = snap.character_doll_info.get(&char_id).map(|(_, p)| p.clone()).unwrap_or_else(|| format!("portrait:bloodline:{}", ch.bloodline_id));
                let final_portrait = new_portrait_opt.unwrap_or(existing_portrait);
                snap.character_doll_info.insert(char_id, (new_doll_str.clone(), final_portrait.clone()));
                Ok((new_doll_str, final_portrait, worn_types, item_changes))
            });
            match res {
                Ok((doll, portrait, worn_types, item_changes)) => {
                    for (it, old_loc, old_flag, old_qty) in item_changes {
                        let mut change_entries = Vec::new();
                        if it.location_id != old_loc {
                            change_entries.push((PyRep::Integer(3), PyRep::Integer(old_loc)));
                        }
                        if (it.flag as i64) != old_flag {
                            change_entries.push((PyRep::Integer(4), PyRep::Integer(old_flag)));
                        }
                        if it.quantity != old_qty {
                            change_entries.push((PyRep::Integer(5), PyRep::Integer(i64::from(old_qty))));
                        }
                        let notif_args = vec![
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(it.item_id)),
                                ("typeID", PyRep::Integer(i64::from(it.type_id))),
                                ("ownerID", PyRep::Integer(it.owner_id)),
                                ("locationID", PyRep::Integer(it.location_id)),
                                ("flagID", PyRep::Integer(it.flag as i64)),
                                ("quantity", PyRep::Integer(i64::from(it.quantity))),
                                ("singleton", PyRep::Integer(if it.singleton { 1 } else { 0 })),
                            ]),
                            PyRep::Dict(change_entries),
                        ];
                        session.push_notification("OnItemChange", notif_args.clone());
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnItemChange",
                            "charid",
                            notif_args,
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(char_id)),
                        ("dollDNA", PyRep::String(doll.clone())),
                        ("dollInfo", PyRep::String(doll)),
                        ("portraitInfo", PyRep::String(portrait)),
                        ("wornApparel", PyRep::List(worn_types.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
                        ("updated", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charMgr", "ListStations") | ("charUnboundMgr", "ListStations") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut counts_by_station: std::collections::BTreeMap<i64, (i64, i64)> =
                    std::collections::BTreeMap::new();
                for item in snap.items.values() {
                    if item.owner_id != char_id {
                        continue;
                    }
                    let loc_id = item.location_id;
                    let solar_system_id = if (60_000_000..=69_999_999).contains(&loc_id) {
                        ctx.sde
                            .stations
                            .get(&loc_id)
                            .map(|st| i64::from(st.solar_system_id))
                    } else if loc_id >= 1_000_000_000_000 {
                        snap.structures
                            .get(&loc_id)
                            .map(|st| i64::from(st.solar_system_id))
                    } else {
                        None
                    };
                    if let Some(sys_id) = solar_system_id {
                        let entry = counts_by_station.entry(loc_id).or_insert((sys_id, 0));
                        entry.1 += 1;
                    }
                }
                counts_by_station
                    .into_iter()
                    .map(|(station_id, (solar_system_id, item_count))| {
                        vec![
                            PyRep::Integer(station_id),
                            PyRep::Integer(solar_system_id),
                            PyRep::Integer(item_count),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["stationID", "solarSystemID", "itemCount"],
                rows,
            )))
        }

        // =========================================================================
        // CH-4: Contacts, Blocklist, Labels, & Online Status (`charMgr`, `onlineStatus`)
        // =========================================================================
        _ => super::character_contacts::handle_contacts(ctx, session, service, method, args, _kwargs),
    }
}

#[cfg(test)]
#[path = "character_clones_implants_tests.rs"]
mod character_clones_implants_tests;

