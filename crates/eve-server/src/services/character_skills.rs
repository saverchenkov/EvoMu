//! `character_skills` — Domain 2: Character Creation, Lineage, Clones, Implants, Skill Queue, & Standings (`§3.2`).

#![forbid(unsafe_code)]

use crate::formulas::{
    apply_security_tag_repair, clone_jump_cooldown_seconds, skill_injector_sp_yield,
    skill_training_time_seconds, sp_for_skill_level,
};
use crate::router::ServerContext;
use eve_db::{JumpCloneRecord, SkillQueueEntry, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charUnboundMgr", "GetCharactersToSelect") | ("charUnboundMgr", "GetCharacterInfo") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let chars = ctx.db.get_characters_for_user(user_id);
            let rows = chars
                .into_iter()
                .map(|c| {
                    vec![
                        PyRep::Integer(c.character_id),
                        PyRep::String(c.name),
                        PyRep::Integer(c.corporation_id),
                        PyRep::Integer(c.station_id.unwrap_or(0)),
                        PyRep::Integer(c.solar_system_id as i64),
                        PyRep::Real(c.balance.to_wire_f64()),
                        PyRep::Real(c.skill_points),
                    ]
                })
                .collect();

            Some(Ok(PyRep::rowset(
                vec![
                    "characterID",
                    "characterName",
                    "corporationID",
                    "stationID",
                    "solarSystemID",
                    "balance",
                    "skillPoints",
                ],
                rows,
            )))
        }

        ("charUnboundMgr", "ValidateNameEx") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let valid = name.trim().len() >= 3
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
            let race_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2) as u32;
            let bloodline_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(3) as u32;
            let ancestry_id = args.get(3).and_then(|a| a.as_i64()).unwrap_or(13) as u32;
            let school_id = args.get(4).and_then(|a| a.as_i64()).unwrap_or(20) as u32;

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
            session.character_id = Some(c.character_id);
            session.corporation_id = Some(c.corporation_id);
            session.alliance_id = c.alliance_id;
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
            let skill_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(3327) as TypeId;
            let target_level = args.get(1).and_then(|a| a.as_i64()).unwrap_or(5).clamp(1, 5) as u8;

            let rank = ctx
                .sde
                .types
                .get(&skill_type_id)
                .and_then(|t| t.default_attributes.get(&attr_ids::SKILL_TIME_CONSTANT))
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
                // Enforce 5,000,000 SP Alpha Clone cap (grade 1)
                if ch.clone_grade == 1 && ch.skill_points + end_sp > 5_000_000.0 {
                    return Err(BridgeRpcError {
                        code: "AlphaCloneSpCapExceeded".into(),
                        message: "Alpha clones cannot queue skills beyond 5,000,000 SP".into(),
                    });
                }
                let start_sp = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&skill_type_id))
                    .map(|s| s.skill_points)
                    .unwrap_or(0.0);
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

                let mut remaining_budget = available_free_sp;
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
                        let prev_lvl = skills_map
                            .get(&q.type_id)
                            .map(|s| s.level)
                            .unwrap_or_else(|| q.target_level.saturating_sub(1));
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: prev_lvl,
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
                Ok(sp) => Some(Ok(PyRep::Real(sp))),
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "InjectSkillPoints") | ("alphaInjectorMgr", "UseAlphaInjector") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Locate an owned Large Skill Injector (type_id 40520) with quantity >= 1
                let injector_item_id = snap
                    .items
                    .values()
                    .find(|i| i.owner_id == char_id && i.type_id == 40520 && i.quantity >= 1)
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
            let loc_id = session
                .station_id
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let jc_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.jump_clones
                    .entry(char_id)
                    .or_default()
                    .push(JumpCloneRecord {
                        jump_clone_id: id,
                        character_id: char_id,
                        location_id: loc_id,
                        implants: Vec::new(),
                    });
                id
            });
            Some(Ok(PyRep::keyval([
                ("jumpCloneID", PyRep::Integer(jc_id)),
                ("locationID", PyRep::Integer(loc_id)),
                (
                    "cooldownSeconds",
                    PyRep::Integer(clone_jump_cooldown_seconds(4) as i64),
                ),
            ])))
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

        ("jumpCloneSvc", "CloneJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_jc_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let clones = snap.jump_clones.entry(char_id).or_default();
                let Some(jc) = clones
                    .iter()
                    .find(|c| target_jc_id == 0 || c.jump_clone_id == target_jc_id)
                    .cloned()
                else {
                    return Err(BridgeRpcError {
                        code: "JumpCloneNotFound".into(),
                        message: format!("Jump clone {target_jc_id} not found"),
                    });
                };
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if jc.location_id >= 1_000_000_000_000 {
                        ch.structure_id = Some(jc.location_id);
                        ch.station_id = None;
                    } else {
                        ch.station_id = Some(jc.location_id);
                        ch.structure_id = None;
                    }
                }
                Ok(jc)
            });
            match res {
                Ok(jc) => {
                    if jc.location_id >= 1_000_000_000_000 {
                        session.structure_id = Some(jc.location_id);
                        session.station_id = None;
                    } else {
                        session.station_id = Some(jc.location_id);
                        session.structure_id = None;
                    }
                    Some(Ok(PyRep::keyval([
                        ("jumpCloneID", PyRep::Integer(jc.jump_clone_id)),
                        ("locationID", PyRep::Integer(jc.location_id)),
                        (
                            "cooldownSeconds",
                            PyRep::Integer(clone_jump_cooldown_seconds(4) as i64),
                        ),
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
            let removed = ctx.db.with_mut_snapshot(|snap| {
                let clones = snap.jump_clones.entry(char_id).or_default();
                let before = clones.len();
                clones.retain(|c| c.jump_clone_id != jc_id);
                clones.len() < before
            });
            Some(Ok(PyRep::Bool(removed)))
        }

        ("standingMgr", "GetCharStandings") | ("standingMgr", "GetStandingTransactions") => {
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
            let tags_requested = args.first().and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u32;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Find owned Clone Soldier Tag stack (35661..=35664) with sufficient quantity
                let tag_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && (35661..=35664).contains(&i.type_id)
                            && (i.quantity as u32) >= tags_requested
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
                    if (item.quantity as u32) > tags_requested {
                        item.quantity -= tags_requested as i32;
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

        ("LPSvc", "GetLPForCharacterCorp")
        | ("LPSvc", "GetAllMyCharacterWalletLPBalances") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| {
                        c.loyalty_points
                            .iter()
                            .map(|(&corp_id, &lp)| {
                                vec![PyRep::Integer(corp_id), PyRep::Integer(lp)]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["corporationID", "loyaltyPoints"],
                rows,
            )))
        }

        ("charMgr", "GetPublicInfo") | ("charMgr", "GetPublicInfo3") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            match ctx.db.get_character(target_id) {
                Ok(ch) => Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(ch.character_id)),
                    ("characterName", PyRep::String(ch.name)),
                    ("corporationID", PyRep::Integer(ch.corporation_id)),
                    ("homeStationID", PyRep::Integer(ch.home_station_id)),
                    ("raceID", PyRep::Integer(ch.race_id as i64)),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("home_station", "get_home_station") | ("charMgr", "GetHomeStation") => {
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
            let new_home_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);
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

        ("certificateMgr", "GetMyCertificates") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .map(|m| {
                        m.values()
                            .filter(|s| s.level >= 4)
                            .map(|s| {
                                vec![
                                    PyRep::Integer(s.type_id as i64),
                                    PyRep::Integer(s.level as i64),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["certificateID", "grade"],
                rows,
            )))
        }

        _ => None,
    }
}
