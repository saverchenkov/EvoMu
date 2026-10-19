//! `character_skills` — Domain 2: Character Creation, Lineage, Clones, Implants, Skill Queue, & Standings (`§3.2`).

#![forbid(unsafe_code)]

use crate::formulas::{
    apply_security_tag_repair, clone_jump_cooldown_seconds, skill_injector_sp_yield,
    skill_training_time_seconds, sp_for_skill_level,
};
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, SkillQueueEntry,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

/// Canonical jump clone installation fee at NPC stations (`900,000 ISK` per `Jump_clones.md:20, 54`; `R23-P1-2`).
pub fn jump_clone_install_fee_cents() -> IskCents {
    IskCents::from_isk(900_000)
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charUnboundMgr", "GetCharactersToSelect") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let chars = ctx.db.get_characters_for_user(user_id);
            let rows = ctx.db.with_snapshot(|snap| {
                chars
                    .into_iter()
                    .map(|c| {
                        let (ship_type_rep, ship_name_rep) = snap
                            .items
                            .get(&c.active_ship_id)
                            .map(|item| {
                                let name = ctx
                                    .sde
                                    .types
                                    .get(&item.type_id)
                                    .map(|t| PyRep::String(t.name.clone()))
                                    .unwrap_or(PyRep::None);
                                (PyRep::Integer(item.type_id as i64), name)
                            })
                            .unwrap_or((PyRep::None, PyRep::None));
                        vec![
                            PyRep::Integer(c.character_id),
                            PyRep::String(c.name),
                            PyRep::Integer(c.corporation_id),
                            c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            c.station_id
                                .or(c.structure_id)
                                .map(PyRep::Integer)
                                .unwrap_or(PyRep::None),
                            PyRep::Integer(c.solar_system_id as i64),
                            PyRep::Integer(c.constellation_id as i64),
                            PyRep::Integer(c.region_id as i64),
                            PyRep::Real(c.balance.to_wire_f64()),
                            PyRep::Real(c.skill_points),
                            ship_type_rep,
                            ship_name_rep,
                            PyRep::Real(c.security_status),
                        ]
                    })
                    .collect()
            });

            Some(Ok(
                DBRowDescriptor::character_select_list_header().build_crowset(rows),
            ))
        }

        ("charUnboundMgr", "GetCharacterInfo") => {
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
            let Some(c) = chars.into_iter().find(|ch| ch.character_id == cid) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipMismatch".into(),
                    message: format!("Character {cid} does not belong to user {user_id}"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("characterName", PyRep::String(c.name)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                (
                    "allianceID",
                    c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("stationID", PyRep::Integer(c.station_id.unwrap_or(0))),
                ("solarSystemID", PyRep::Integer(c.solar_system_id as i64)),
                ("constellationID", PyRep::Integer(c.constellation_id as i64)),
                ("regionID", PyRep::Integer(c.region_id as i64)),
                ("raceID", PyRep::Integer(c.race_id as i64)),
                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                ("schoolID", PyRep::Integer(c.school_id as i64)),
                ("balance", PyRep::Real(c.balance.to_wire_f64())),
                ("skillPoints", PyRep::Real(c.skill_points)),
                ("freeSkillPoints", PyRep::Real(c.free_skill_points)),
                ("securityStatus", PyRep::Real(c.security_status)),
                ("cloneGrade", PyRep::Integer(c.clone_grade as i64)),
            ])))
        }

        ("charUnboundMgr", "ValidateNameEx") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let trimmed_len = name.trim().len();
            let valid = (3..=37).contains(&trimmed_len)
                && name.len() <= 37
                && ctx.db.with_snapshot(|s| {
                    !s.characters
                        .values()
                        .any(|c| c.name.eq_ignore_ascii_case(name))
                });
            Some(Ok(PyRep::Bool(valid)))
        }

        ("charUnboundMgr", "CreateCharacterWithDoll") | ("charUnboundMgr", "CreateCharacter") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("New Capsuleer");
            if !(3..=37).contains(&name.trim().len()) || name.len() > 37 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCharacterName".into(),
                    message: "Character name must be between 3 and 37 characters".into(),
                }));
            }
            let race_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2) as u32;
            let bloodline_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(3) as u32;
            let ancestry_id = args.get(3).and_then(|a| a.as_i64()).unwrap_or(19) as u32;
            let school_id = args.get(4).and_then(|a| a.as_i64()).unwrap_or(14) as u32;

            let res = ctx.db.create_character(
                user_id,
                name,
                race_id,
                bloodline_id,
                ancestry_id,
                school_id,
                &ctx.sde,
            );
            match res {
                Ok(ch) => Some(Ok(PyRep::Integer(ch.character_id))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "CharacterCreationFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("charUnboundMgr", "SelectCharacterID") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SelectCharacterID requires integer characterID".into(),
                }));
            };

            let c = match ctx.db.select_character_for_user(user_id, char_id) {
                Ok(ch) => ch,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "CharacterSelectionDenied".into(),
                        message: e.to_string(),
                    }))
                }
            };

            ctx.release_session_bindings(session.session_id);
            if ctx
                .db
                .get_online_session(user_id)
                .is_some_and(|os| os.session_id == session.session_id)
            {
                let _ = ctx.db.acquire_online_lock(
                    user_id,
                    session.session_id,
                    Some(c.character_id),
                    false,
                    ctx.now_unix(),
                );
            }
            session.character_id = Some(c.character_id);
            session.corporation_id = Some(c.corporation_id);
            session.alliance_id = c.alliance_id;
            session.war_faction_id = c.war_faction_id;
            session.station_id = c.station_id;
            session.structure_id = c.structure_id;
            session.solar_system_id = Some(c.solar_system_id);
            session.constellation_id = Some(c.constellation_id);
            session.region_id = Some(c.region_id);
            session.ship_id = Some(c.active_ship_id);
            session.race_id = Some(c.race_id);

            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("shipID", PyRep::Integer(c.active_ship_id)),
                ("solarSystemID", PyRep::Integer(c.solar_system_id as i64)),
            ])))
        }

        ("skillMgr2", "GetSkills") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .map(|m| {
                        m.values()
                            .map(|s| {
                                vec![
                                    PyRep::Integer(s.type_id as i64),
                                    PyRep::Integer(s.level as i64),
                                    PyRep::Real(s.skill_points),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["typeID", "skillLevel", "skillPoints"],
                rows,
            )))
        }

        ("skillMgr2", "GetSkillQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.skill_queues
                    .get(&char_id)
                    .map(|q| {
                        q.iter()
                            .map(|e| {
                                vec![
                                    PyRep::Integer(e.type_id as i64),
                                    PyRep::Integer(e.target_level as i64),
                                    PyRep::Real(e.start_sp),
                                    PyRep::Real(e.end_sp),
                                    PyRep::Real(e.duration_seconds),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "trainingTypeID",
                    "trainingToLevel",
                    "trainingStartSP",
                    "trainingDestinationSP",
                    "durationSeconds",
                ],
                rows,
            )))
        }

        ("skillMgr2", "SaveSkillQueue") | ("skillMgr2", "AddToEndOfSkillQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (raw_skill_type_opt, raw_target_level) = match args.first() {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => match items.first() {
                    Some(PyRep::Tuple(pair) | PyRep::List(pair)) => (
                        pair.first().and_then(|a| a.as_i64()),
                        pair.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    ),
                    Some(first_num) => (
                        first_num.as_i64(),
                        items.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    ),
                    None => (None, 1),
                },
                _ => (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                ),
            };
            let Some(raw_skill_type) = raw_skill_type_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkillType".into(),
                    message: "SaveSkillQueue/AddToEndOfSkillQueue requires an explicit skill typeID".into(),
                }));
            };
            let Ok(skill_type_id) = TypeId::try_from(raw_skill_type) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkillType".into(),
                    message: format!("Invalid skill type ID {raw_skill_type}"),
                }));
            };
            let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkillType".into(),
                    message: format!("Skill type {skill_type_id} not found in SDE"),
                }));
            };
            if skill_type.category_id != 16 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkillType".into(),
                    message: format!("Type {skill_type_id} is not a skill (category 16)"),
                }));
            }
            let target_level = raw_target_level.clamp(1, 5) as u8;

            let rank = skill_type
                .default_attributes
                .get(&attr_ids::SKILL_TIME_CONSTANT)
                .copied()
                .unwrap_or(1.0);
            let end_sp = sp_for_skill_level(rank, target_level);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };

                // Enforce skill prerequisites (`requiredSkill1..3` and `requiredSkill1..3Level`)
                // against trained skills or earlier entries in the character's skill queue.
                for (skill_attr, level_attr) in [
                    (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
                    (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
                    (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
                ] {
                    if let Some(&req_skill_f) = skill_type.default_attributes.get(&skill_attr) {
                        let req_skill_id = req_skill_f as TypeId;
                        if req_skill_id > 0 {
                            let req_lvl = skill_type
                                .default_attributes
                                .get(&level_attr)
                                .copied()
                                .unwrap_or(1.0)
                                .max(1.0) as u8;
                            let trained_lvl = snap
                                .skills
                                .get(&char_id)
                                .and_then(|m| m.get(&req_skill_id))
                                .map(|s| s.level)
                                .unwrap_or(0);
                            let queued_lvl = snap
                                .skill_queues
                                .get(&char_id)
                                .and_then(|q| {
                                    q.iter()
                                        .filter(|e| e.type_id == req_skill_id)
                                        .map(|e| e.target_level)
                                        .max()
                                })
                                .unwrap_or(0);
                            if trained_lvl.max(queued_lvl) < req_lvl {
                                return Err(BridgeRpcError {
                                    code: "SkillPrerequisiteNotMet".into(),
                                    message: format!(
                                        "Skill {} (type {skill_type_id}) requires prerequisite skill {req_skill_id} at level {req_lvl}",
                                        skill_type.name
                                    ),
                                });
                            }
                        }
                    }
                }

                let current_lvl = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&skill_type_id))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let (max_queued_lvl, max_queued_sp) = snap
                    .skill_queues
                    .get(&char_id)
                    .and_then(|q| {
                        q.iter()
                            .filter(|e| e.type_id == skill_type_id)
                            .max_by_key(|e| e.target_level)
                            .map(|e| (e.target_level, Some(e.end_sp)))
                    })
                    .unwrap_or((0, None));
                let effective_base_lvl = current_lvl.max(max_queued_lvl);
                if target_level <= effective_base_lvl {
                    return Err(BridgeRpcError {
                        code: "SkillAlreadyTrained".into(),
                        message: format!(
                            "Skill {} (type {skill_type_id}) is already trained or queued to level {effective_base_lvl}",
                            skill_type.name
                        ),
                    });
                }

                let start_sp = max_queued_sp.unwrap_or_else(|| {
                    snap.skills
                        .get(&char_id)
                        .and_then(|m| m.get(&skill_type_id))
                        .map(|s| s.skill_points)
                        .unwrap_or(0.0)
                });
                let sp_delta = (end_sp - start_sp).max(0.0);
                let queued_sp: f64 = snap
                    .skill_queues
                    .get(&char_id)
                    .map(|q| q.iter().map(|e| (e.end_sp - e.start_sp).max(0.0)).sum())
                    .unwrap_or(0.0);
                // Enforce 5,000,000 SP Alpha Clone cap (grade 1) using incremental SP delta
                if ch.clone_grade == 1 && ch.skill_points + queued_sp + sp_delta > 5_000_000.0 {
                    return Err(BridgeRpcError {
                        code: "AlphaCloneSpCapExceeded".into(),
                        message: "Alpha clones cannot queue skills beyond 5,000,000 SP".into(),
                    });
                }
                let clone_mult = if ch.clone_grade == 2 { 1.0 } else { 0.5 };
                let duration_sec = skill_training_time_seconds(
                    start_sp,
                    end_sp,
                    ch.neural.perception as f64,
                    ch.neural.willpower as f64,
                    clone_mult,
                );
                snap.skill_queues
                    .entry(char_id)
                    .or_default()
                    .push(SkillQueueEntry {
                        type_id: skill_type_id,
                        target_level,
                        start_sp,
                        end_sp,
                        duration_seconds: duration_sec,
                    });
                Ok(duration_sec)
            });

            match res {
                Ok(dur) => Some(Ok(PyRep::keyval([
                    ("typeID", PyRep::Integer(skill_type_id as i64)),
                    ("targetLevel", PyRep::Integer(target_level as i64)),
                    ("endSP", PyRep::Real(end_sp)),
                    ("durationSeconds", PyRep::Real(dur)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "ApplyFreeSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let requested_sp_opt = args.first().and_then(|a| a.as_f64());
            if let Some(req_sp) = requested_sp_opt {
                if !req_sp.is_finite() || req_sp <= 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillPointAmount".into(),
                        message: "Requested free skill points to apply must be finite and positive"
                            .into(),
                    }));
                }
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let available_free_sp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.free_skill_points)
                    .unwrap_or(0.0);
                if available_free_sp <= 0.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFreeSP".into(),
                        message: "No unallocated free skill points available to apply".into(),
                    });
                }

                let mut remaining_budget = requested_sp_opt
                    .map(|req| available_free_sp.min(req))
                    .unwrap_or(available_free_sp);
                let queue = snap.skill_queues.entry(char_id).or_default().clone();
                let mut remaining_queue = Vec::new();
                let skills_map = snap.skills.entry(char_id).or_default();
                let mut total_applied = 0.0;

                for mut q in queue {
                    let needed = (q.end_sp - q.start_sp).max(0.0);
                    if remaining_budget >= needed && needed > 0.0 {
                        remaining_budget -= needed;
                        total_applied += needed;
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: q.target_level,
                                skill_points: q.end_sp,
                            },
                        );
                    } else if remaining_budget > 0.0 && needed > 0.0 {
                        let applied = remaining_budget;
                        remaining_budget = 0.0;
                        total_applied += applied;
                        q.start_sp += applied;
                        q.duration_seconds *= ((q.end_sp - q.start_sp).max(0.0)) / needed;
                        let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
                        let rank = ctx
                            .sde
                            .types
                            .get(&q.type_id)
                            .and_then(|t| {
                                t.default_attributes
                                    .get(&attr_ids::SKILL_TIME_CONSTANT)
                                    .copied()
                            })
                            .unwrap_or(1.0);
                        let mut achieved_lvl = current_lvl;
                        for lvl in (current_lvl + 1)..=q.target_level {
                            if q.start_sp + 0.5 >= sp_for_skill_level(rank, lvl) {
                                achieved_lvl = lvl;
                            } else {
                                break;
                            }
                        }
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: achieved_lvl,
                                skill_points: q.start_sp,
                            },
                        );
                        remaining_queue.push(q);
                    } else {
                        remaining_queue.push(q);
                    }
                }
                snap.skill_queues.insert(char_id, remaining_queue);

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.free_skill_points = (ch.free_skill_points - total_applied).max(0.0);
                    ch.skill_points += total_applied;
                    return Ok(ch.skill_points);
                }
                Ok(0.0)
            });
            match res {
                Ok(sp) => {
                    if let Some(sid) = session.ship_id {
                        ctx.invalidate_ship_fitting(sid);
                    }
                    Some(Ok(PyRep::Real(sp)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "InjectSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Locate an owned, unlocked, reachable Large Skill Injector (type_id 40520) with quantity >= 1
                let injector_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.type_id == 40520
                            && i.quantity >= 1
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(item_id) = injector_item_id else {
                    return Err(BridgeRpcError {
                        code: "InjectorNotFound".into(),
                        message: "Character does not own a Skill Injector (typeID 40520)".into(),
                    });
                };

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    let y = skill_injector_sp_yield(ch.skill_points);
                    ch.free_skill_points += y;
                    Ok(y)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });
            match res {
                Ok(yielded) => Some(Ok(PyRep::Real(yielded))),
                Err(e) => Some(Err(e)),
            }
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
            let station_info = ctx
                .sde
                .solar_systems
                .values()
                .flat_map(|s| s.stations.iter())
                .find(|st| st.station_id == station_id);
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Read Infomorph Psychology (24242), Advanced Infomorph Psychology (33407), Elite Infomorph Psychology (73910),
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
                    .and_then(|m| m.get(&73910))
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
                Ok((jc_id, cooldown_sec)) => Some(Ok(PyRep::keyval([
                    ("jumpCloneID", PyRep::Integer(jc_id)),
                    ("locationID", PyRep::Integer(station_id)),
                    ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "InstallCloneInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(structure_id) = session.structure_id else {
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
                if !st.quantum_core_installed || st.fuel_blocks == 0 {
                    return Err(BridgeRpcError {
                        code: "StructureHasNoCloneBay".into(),
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
                    .and_then(|m| m.get(&73910))
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
                Ok((jc_id, cooldown_sec)) => Some(Ok(PyRep::keyval([
                    ("jumpCloneID", PyRep::Integer(jc_id)),
                    ("locationID", PyRep::Integer(structure_id)),
                    ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "GetCloneState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.jump_clones
                    .get(&char_id)
                    .map(|clones| {
                        clones
                            .iter()
                            .map(|c| {
                                vec![
                                    PyRep::Integer(c.jump_clone_id),
                                    PyRep::Integer(c.location_id),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["jumpCloneID", "locationID"], rows)))
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
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["jumpCloneID", "locationID"], rows)))
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
            let target_jc_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if target_jc_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidJumpCloneID".into(),
                    message: "CloneJump requires a positive jumpCloneID".into(),
                }));
            }
            let departure_has_clone_bay = if departure_loc_id >= 1_000_000_000_000 {
                ctx.db.with_snapshot(|snap| {
                    snap.structures
                        .get(&departure_loc_id)
                        .map(|st| st.quantum_core_installed && st.fuel_blocks > 0)
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
                    if !st.quantum_core_installed || st.fuel_blocks == 0 {
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
                        InventoryItemRecord {
                            item_id: cid,
                            type_id: 670, // Capsule
                            owner_id: char_id,
                            location_id: dest_loc_id,
                            flag: InventoryFlag::Hangar,
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

                let active_implants = {
                    let char_implants = snap.active_implants.entry(char_id).or_default();
                    if let Some(clones_mut) = snap.jump_clones.get_mut(&char_id) {
                        std::mem::swap(char_implants, &mut clones_mut[jc_idx].implants);
                    }
                    char_implants.clone()
                };
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
                Ok((
                    target_jc_id,
                    dest_loc_id,
                    dest_sys_id,
                    dest_const_id,
                    dest_reg_id,
                    capsule_id,
                    effective_cooldown,
                    active_implants,
                ))
            });
            match res {
                Ok((
                    jc_id,
                    dest_loc_id,
                    dest_sys_id,
                    dest_const_id,
                    dest_reg_id,
                    capsule_id,
                    cooldown_sec,
                    active_implants,
                )) => {
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
                    let implant_reps: Vec<PyRep> = active_implants
                        .into_iter()
                        .map(|t| PyRep::Integer(t as i64))
                        .collect();
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
            let removed = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let clones = snap.jump_clones.entry(char_id).or_default();
                    let before = clones.len();
                    clones.retain(|c| c.jump_clone_id != jc_id);
                    Ok::<_, std::convert::Infallible>(clones.len() < before)
                })
                .unwrap();
            Some(Ok(PyRep::Bool(removed)))
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

        ("crimewatch", "ApplyTagsForSecStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedInStation".into(),
                    message:
                        "Security status tag turn-in requires being docked at a CONCORD station"
                            .into(),
                }));
            }
            let raw_tags = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let Ok(tags_i32) = i32::try_from(raw_tags) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTagQuantity".into(),
                    message: format!("Invalid security tag quantity {raw_tags}"),
                }));
            };
            if tags_i32 <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTagQuantity".into(),
                    message: "Security tag quantity must be strictly positive".into(),
                }));
            }
            let tags_requested = tags_i32 as u32;
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Find owned, unlocked, reachable Clone Soldier Tag stack (35661..=35664) with sufficient quantity
                let tag_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        (35661..=35664).contains(&i.type_id)
                            && i.quantity >= tags_i32
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(item_id) = tag_item_id else {
                    return Err(BridgeRpcError {
                        code: "InsufficientSecurityTags".into(),
                        message: format!(
                            "Character does not own {tags_requested} Clone Soldier Tag(s) (35661..35664)"
                        ),
                    });
                };

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.quantity > tags_i32 {
                        item.quantity -= tags_i32;
                        item.version += 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.security_status =
                        apply_security_tag_repair(ch.security_status, tags_requested, 0.0);
                    Ok(ch.security_status)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });
            match res {
                Ok(new_sec) => Some(Ok(PyRep::Real(new_sec))),
                Err(e) => Some(Err(e)),
            }
        }

        ("home_station", "get_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            match ctx.db.get_character(char_id) {
                Ok(ch) => Some(Ok(PyRep::Integer(ch.home_station_id))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("home_station", "set_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(new_home_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtTargetStation".into(),
                    message: "set_home_station requires being docked at the target station".into(),
                }));
            };
            if session.station_id != Some(new_home_id) {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtTargetStation".into(),
                    message: format!(
                        "Setting home station {new_home_id} requires being docked at that station"
                    ),
                }));
            }
            let exists_in_sde = ctx
                .sde
                .solar_systems
                .values()
                .any(|s| s.stations.iter().any(|st| st.station_id == new_home_id));
            if !exists_in_sde {
                return Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {new_home_id} does not exist in SDE"),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                ch.home_station_id = new_home_id;
                Ok(ch.home_station_id)
            });
            match res {
                Ok(hid) => Some(Ok(PyRep::Integer(hid))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
