//! `jump_clone_names` — persisted labels for jump clones (`jumpCloneSvc.SetJumpCloneName`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Private JSON side-store key in `item_custom_names` (jumpCloneID -> label).
pub(crate) const JUMP_CLONE_NAMES_STORE_KEY: i64 = -10_000_006;

/// DERIVED: maximum label length accepted. The client UI limit is not available in the SDE;
/// 100 characters is an assumption.
pub(crate) const MAX_JUMP_CLONE_NAME_LEN: usize = 100;

fn load(snap: &eve_db::WorldStateSnapshot) -> HashMap<i64, String> {
    snap.item_custom_names
        .get(&JUMP_CLONE_NAMES_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// Label of a jump clone (empty string when none was set).
pub fn name_of(snap: &eve_db::WorldStateSnapshot, jump_clone_id: i64) -> String {
    load(snap).get(&jump_clone_id).cloned().unwrap_or_default()
}

/// `SetJumpCloneName(cloneID, newName)`: only the clone's owner may rename it; an empty name clears it.
pub fn set_jump_clone_name(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let clone_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidJumpCloneID".into(),
        message: "SetJumpCloneName requires cloneID".into(),
    })?;
    let name = args.get(1).and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
    if name.chars().count() > MAX_JUMP_CLONE_NAME_LEN {
        return Err(BridgeRpcError {
            code: "JumpCloneNameTooLong".into(),
            message: format!("Jump clone names are limited to {MAX_JUMP_CLONE_NAME_LEN} characters"),
        });
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let owns = snap.jump_clones.get(&char_id).is_some_and(|l| l.iter().any(|c| c.jump_clone_id == clone_id));
        if !owns {
            return Err(BridgeRpcError {
                code: "JumpCloneNotFound".into(),
                message: format!("Jump clone {clone_id} not found"),
            });
        }
        let mut names = load(snap);
        if name.is_empty() {
            names.remove(&clone_id);
        } else {
            names.insert(clone_id, name.clone());
        }
        if let Ok(json) = serde_json::to_string(&names) {
            snap.item_custom_names.insert(JUMP_CLONE_NAMES_STORE_KEY, json);
        }
        Ok(())
    })?;
    Ok(PyRep::None)
}

/// `ValidateInstallJumpClone`: validates prerequisites (docked, clone capacity, funds)
/// for installing a jump clone at the current station or structure.
pub fn validate_install_jump_clone(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_none() && session.structure_id.is_none() {
        return Err(BridgeRpcError {
            code: "MustBeDocked".into(),
            message: "ValidateInstallJumpClone requires being docked at an NPC station or structure".into(),
        });
    }
    let (clone_count, max_clones, balance) = ctx.db.with_snapshot(|snap| {
        let count = snap.jump_clones.get(&char_id).map(|v| v.len()).unwrap_or(0);
        let bal = snap.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0);
        let skills = snap.skills.get(&char_id);
        let info_lvl = skills.and_then(|m| m.get(&24242)).map(|s| s.level as usize).unwrap_or(0);
        let adv_lvl = skills.and_then(|m| m.get(&33407)).map(|s| s.level as usize).unwrap_or(0);
        let elite_lvl = skills.and_then(|m| m.get(&73910)).map(|s| s.level as usize).unwrap_or(0);
        (count, info_lvl + adv_lvl + elite_lvl, bal)
    });
    // Canonical jump clone installation fee: 900,000 ISK = 90,000,000 cents
    let clone_cost_cents = 90_000_000;
    if balance < clone_cost_cents {
        return Err(BridgeRpcError {
            code: "NotEnoughMoney".into(),
            message: "Insufficient funds to install jump clone (900,000 ISK required)".into(),
        });
    }
    if clone_count >= max_clones {
        return Err(BridgeRpcError {
            code: "JumpCloneLimitReached".into(),
            message: format!(
                "Maximum number of jump clones ({max_clones}) reached for current Infomorph Psychology skill levels"
            ),
        });
    }
    Ok(PyRep::Bool(true))
}

/// `GetShipCloneState`: returns clones installed in a ship's Clone Vat Bay (`jumpCloneID`, `ownerID`, `locationID`, `cloneName`).
pub fn get_ship_clone_state(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let host_ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.ship_id)
        .unwrap_or(0);
    let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
        snap.jump_clones
            .values()
            .flat_map(|v| v.iter())
            .filter(|jc| jc.location_id == host_ship_id)
            .map(|jc| {
                vec![
                    PyRep::Integer(jc.jump_clone_id),
                    PyRep::Integer(jc.character_id),
                    PyRep::Integer(jc.location_id),
                    PyRep::String(name_of(snap, jc.jump_clone_id)),
                ]
            })
            .collect()
    });
    Ok(PyRep::rowset(
        vec!["jumpCloneID", "ownerID", "locationID", "cloneName"],
        rows,
    ))
}

/// Emits `OnServerSkillsChanged(skillInfos, event, timeStamp)` matching `SkillsSvc.OnServerSkillsChanged`.
pub(crate) fn emit_server_skills_changed(
    ctx: &ServerContext,
    char_id: i64,
    skill_type_ids: &[u32],
    event: Option<&str>,
) {
    if skill_type_ids.is_empty() {
        return;
    }
    let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
        let Some(skills) = snap.skills.get(&char_id) else {
            return Vec::new();
        };
        skill_type_ids
            .iter()
            .filter_map(|&tid| {
                let rec = skills.get(&tid)?;
                Some((
                    PyRep::Integer(tid as i64),
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("trainedSkillPoints", PyRep::Real(rec.skill_points)),
                        ("trainedSkillLevel", PyRep::Integer(rec.level as i64)),
                        ("effectiveSkillLevel", PyRep::Integer(rec.level as i64)),
                        ("virtualSkillLevel", PyRep::None),
                    ]),
                ))
            })
            .collect()
    });
    if entries.is_empty() {
        return;
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerSkillsChanged",
        "charid",
        vec![
            PyRep::Dict(entries),
            event.map(|s| PyRep::String(s.into())).unwrap_or(PyRep::None),
            PyRep::Integer(now_ft),
        ],
    );
}

/// Emits `OnServerSkillsRemoved(skillInfos, timeStamp)` matching `SkillsSvc.OnServerSkillsRemoved`.
pub(crate) fn emit_server_skills_removed(
    ctx: &ServerContext,
    char_id: i64,
    removed: &[(u32, u8)],
) {
    if removed.is_empty() {
        return;
    }
    let entries: Vec<(PyRep, PyRep)> = removed
        .iter()
        .map(|&(tid, lvl)| {
            (
                PyRep::Integer(tid as i64),
                PyRep::keyval([
                    ("typeID", PyRep::Integer(tid as i64)),
                    ("trainedSkillPoints", PyRep::None),
                    ("trainedSkillLevel", PyRep::Integer(lvl as i64)),
                    ("effectiveSkillLevel", PyRep::Integer(lvl as i64)),
                    ("virtualSkillLevel", PyRep::None),
                ]),
            )
        })
        .collect();
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerSkillsRemoved",
        "charid",
        vec![PyRep::Dict(entries), PyRep::Integer(now_ft)],
    );
}

/// `skillMgr2.ExtractSkills(skills, itemID, token)`: extracts skill points/levels from `skills`
/// and emits `OnServerSkillsRemoved(skillInfos, timeStamp)`.
pub(crate) fn extract_skills(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    // 1. Validate extractor item ID argument first (R-SKX-02)
    let Some(extractor_item_id) = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
        return Err(BridgeRpcError {
            code: "MissingSkillExtractor".into(),
            message: "ExtractSkills requires a valid Skill Extractor itemID".into(),
        });
    };

    // 2. Parse requested skills dict
    let requested: Vec<(u32, f64)> = match args.first() {
        Some(PyRep::Dict(entries)) => entries
            .iter()
            .filter_map(|(k, v)| {
                let tid = k.as_i64().filter(|&id| id > 0 && id <= u32::MAX as i64)? as u32;
                let sp = v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))?;
                Some((tid, sp))
            })
            .collect(),
        Some(PyRep::Integer(tid)) if *tid > 0 && *tid <= u32::MAX as i64 => {
            vec![(*tid as u32, 500_000.0)]
        }
        _ => Vec::new(),
    };
    if requested.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "ExtractSkills requires non-empty skills dict".into(),
        });
    }

    // R-SKX-01: Reject duplicate skill typeIDs and non-positive/non-finite amounts
    // R-SKX-03 / R-SKX-04: Reject non-extractable skills.
    // Per EVE Wiki (Skill_trading.md:62-67), exactly 5 skills cannot be extracted:
    // - 24242: Infomorph Psychology
    // - 33407: Advanced Infomorph Psychology
    // - 3411:  Cybernetics
    // - 2505:  Command Center Upgrades
    // - 2495:  Interplanetary Consolidation
    let mut seen = std::collections::HashSet::new();
    for &(tid, take) in &requested {
        if matches!(tid, 24242 | 33407 | 3411 | 2505 | 2495) {
            return Err(BridgeRpcError {
                code: "SkillCannotBeExtracted".into(),
                message: format!("Skill {tid} cannot be extracted"),
            });
        }
        if !seen.insert(tid) {
            return Err(BridgeRpcError {
                code: "InvalidSkillPointAmount".into(),
                message: "Duplicate skill in extraction request".into(),
            });
        }
        if !take.is_finite() || take <= 0.0 {
            return Err(BridgeRpcError {
                code: "InvalidSkillPointAmount".into(),
                message: "Each requested extraction amount must be positive and finite".into(),
            });
        }
    }

    let (removed, injector_id) = ctx.db.try_with_mut_snapshot(|snap| {
        // 3. Extractor existence, ownership, type, and reachability validation FIRST in snapshot (R-SKX-02)
        let Some(loc) = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(session.station_id)
        else {
            return Err(BridgeRpcError {
                code: "NotDocked".into(),
                message: "Must be docked in a station or structure to extract skills".into(),
            });
        };
        let ship_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.active_ship_id)
            .or(session.ship_id);
        let extractor_record = snap.items.get(&extractor_item_id).cloned();
        let Some(extractor) = extractor_record else {
            return Err(BridgeRpcError {
                code: "MissingSkillExtractor".into(),
                message: "Skill Extractor item not found".into(),
            });
        };
        if extractor.owner_id != char_id || extractor.type_id != 40519 || extractor.quantity < 1 {
            return Err(BridgeRpcError {
                code: "MissingSkillExtractor".into(),
                message: "Valid Skill Extractor (type 40519) owned by character is required".into(),
            });
        }
        let in_hangar =
            extractor.location_id == loc && extractor.flag == eve_db::InventoryFlag::Hangar;
        let in_ship = ship_id.is_some_and(|sid| {
            extractor.location_id == sid && extractor.flag == eve_db::InventoryFlag::Cargo
        });
        if !in_hangar && !in_ship {
            return Err(BridgeRpcError {
                code: "MissingSkillExtractor".into(),
                message: "Skill Extractor must be located in character station hangar or active ship cargo".into(),
            });
        }
        if snap.is_item_locked_in_contract_or_job(extractor_item_id)
            || extractor.flag == eve_db::InventoryFlag::Locked
        {
            return Err(BridgeRpcError {
                code: "SkillExtractorLocked".into(),
                message: "Skill Extractor is locked in an active contract or industry job".into(),
            });
        }

        let ch_rec = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;

        // 4. Omega clone check
        if ch_rec.clone_grade <= 1 {
            return Err(BridgeRpcError {
                code: "OmegaCloneRequired".into(),
                message: "Skill extraction requires an active Omega clone account".into(),
            });
        }

        // 5. Total requested SP amount must equal exactly 500,000 SP (R-SKX-01)
        let total_req: f64 = requested.iter().map(|(_, sp)| *sp).sum();
        if !total_req.is_finite() || (total_req - 500_000.0).abs() > 0.01 {
            return Err(BridgeRpcError {
                code: "InvalidSkillPointAmount".into(),
                message: format!(
                    "Skill extraction requires extracting exactly 500,000 skill points (got {total_req})"
                ),
            });
        }

        // 6. Character allocated SP floor: must possess at least 5,500,000 allocated SP
        let allocated_sp = ch_rec.skill_points;
        if allocated_sp < 5_500_000.0 {
            return Err(BridgeRpcError {
                code: "InsufficientSkillPoints".into(),
                message: format!(
                    "Skill extraction requires at least 5,500,000 allocated SP (character has {allocated_sp:.0} SP)"
                ),
            });
        }
        if allocated_sp - 500_000.0 < 5_000_000.0 - 0.01 {
            return Err(BridgeRpcError {
                code: "InsufficientSkillPoints".into(),
                message: "ExtractSkills requires at least 5,000,000 allocated SP to remain after extraction".into(),
            });
        }

        // 7. Verify skill prerequisites and non-extractable dependencies (R-SKX-03)
        let skills_map = snap.skills.entry(char_id).or_default();
        let mut projected_levels: std::collections::HashMap<u32, u8> = skills_map
            .iter()
            .map(|(&tid, rec)| (tid, rec.level))
            .collect();

        for &(tid, take) in &requested {
            let Some(rec) = skills_map.get(&tid) else {
                return Err(BridgeRpcError {
                    code: "SkillNotTrained".into(),
                    message: format!("Skill {tid} is not trained by character {char_id}"),
                });
            };
            if take > rec.skill_points {
                return Err(BridgeRpcError {
                    code: "InsufficientSkillPoints".into(),
                    message: format!(
                        "Skill {tid} only has {:.0} SP (attempted to extract {:.0})",
                        rec.skill_points, take
                    ),
                });
            }
            let rem_sp = rec.skill_points - take;
            let rank = ctx
                .sde
                .types
                .get(&tid)
                .and_then(|t| {
                    t.default_attributes
                        .get(&eve_dogma::attr_ids::SKILL_TIME_CONSTANT)
                        .copied()
                })
                .unwrap_or(1.0);
            let mut new_lvl = 0u8;
            for lvl in (1..=5).rev() {
                if rem_sp + 0.5 >= crate::formulas::sp_for_skill_level(rank, lvl) {
                    new_lvl = lvl;
                    break;
                }
            }
            projected_levels.insert(tid, new_lvl);
        }

        // Validate that no remaining trained skill will have its prerequisites violated (R-SKX-05)
        // Check all 6 SDE required-skill dogma slots: [182/277, 183/278, 184/279, 1285/1286, 1289/1287, 1290/1288]
        for (&s_tid, &s_lvl) in &projected_levels {
            if s_lvl == 0 {
                continue;
            }
            if let Some(skill_type) = ctx.sde.types.get(&s_tid) {
                for (req_attr, req_lvl_attr) in eve_dogma::REQUIRED_SKILL_ATTR_IDS
                    .into_iter()
                    .zip(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS)
                {
                    if let Some(&req_tid_f) = skill_type.default_attributes.get(&req_attr) {
                        let req_tid = req_tid_f as u32;
                        if req_tid > 0 && requested.iter().any(|&(tid, _)| tid == req_tid) {
                            let req_lvl = skill_type
                                .default_attributes
                                .get(&req_lvl_attr)
                                .copied()
                                .unwrap_or(1.0)
                                .max(1.0) as u8;
                            let remaining_req_lvl = projected_levels.get(&req_tid).copied().unwrap_or(0);
                            if remaining_req_lvl < req_lvl {
                                return Err(BridgeRpcError {
                                    code: "SkillPrerequisiteRequired".into(),
                                    message: format!(
                                        "Cannot extract skill {req_tid}: required at level {req_lvl} for trained skill {s_tid}"
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 8. Apply extraction and level updates, and reconcile snap.skill_queues (#4)
        let mut removed_skills = Vec::new();
        let mut extracted_new_sp: std::collections::HashMap<u32, (u8, f64, f64)> =
            std::collections::HashMap::new();
        for &(tid, take) in &requested {
            let rec = skills_map.get_mut(&tid).unwrap();
            rec.skill_points = (rec.skill_points - take).max(0.0);
            let rem_sp = rec.skill_points;
            let new_lvl = projected_levels[&tid];
            rec.level = new_lvl;
            let rank = ctx
                .sde
                .types
                .get(&tid)
                .and_then(|t| {
                    t.default_attributes
                        .get(&eve_dogma::attr_ids::SKILL_TIME_CONSTANT)
                        .copied()
                })
                .unwrap_or(1.0);
            if rec.skill_points <= 0.5 && new_lvl == 0 {
                skills_map.remove(&tid);
            }
            extracted_new_sp.insert(tid, (new_lvl, rem_sp, rank));
            removed_skills.push((tid, new_lvl));
        }

        if let Some(queue) = snap.skill_queues.get_mut(&char_id) {
            let mut valid_next_lvl: std::collections::HashMap<u32, u8> = projected_levels.clone();
            queue.retain_mut(|q| {
                // Drop queued skills whose prerequisites are no longer met after extraction
                if let Some(skill_type) = ctx.sde.types.get(&q.type_id) {
                    for (req_attr, req_lvl_attr) in eve_dogma::REQUIRED_SKILL_ATTR_IDS
                        .into_iter()
                        .zip(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS)
                    {
                        if let Some(&req_tid_f) = skill_type.default_attributes.get(&req_attr) {
                            let req_tid = req_tid_f as u32;
                            if req_tid > 0 {
                                let req_lvl = skill_type
                                    .default_attributes
                                    .get(&req_lvl_attr)
                                    .copied()
                                    .unwrap_or(1.0)
                                    .max(1.0) as u8;
                                let have_lvl = valid_next_lvl.get(&req_tid).copied().unwrap_or(0);
                                if have_lvl < req_lvl {
                                    return false;
                                }
                            }
                        }
                    }
                }
                if let Some(&(new_lvl, rem_sp, rank)) = extracted_new_sp.get(&q.type_id) {
                    let expected_prev = valid_next_lvl.get(&q.type_id).copied().unwrap_or(new_lvl);
                    if q.target_level <= new_lvl || q.target_level != expected_prev + 1 {
                        return false;
                    }
                    let lvl_base_sp = if q.target_level <= 1 {
                        0.0
                    } else {
                        crate::formulas::sp_for_skill_level(rank, q.target_level - 1)
                    };
                    let new_start_sp = if q.target_level == new_lvl + 1 {
                        rem_sp.clamp(lvl_base_sp, q.end_sp)
                    } else {
                        lvl_base_sp
                    };
                    let old_needed = (q.end_sp - q.start_sp).max(1.0);
                    q.start_sp = new_start_sp;
                    q.duration_seconds *= ((q.end_sp - q.start_sp).max(0.0)) / old_needed;
                }
                valid_next_lvl.insert(q.type_id, q.target_level);
                true
            });
        }

        let now_ft = ctx.now_blueos_filetime_ticks();
        let hist = snap.skill_history.entry(char_id).or_default();
        for &(tid, _take) in &requested {
            let cur_sp = skills_map.get(&tid).map(|s| s.skill_points).unwrap_or(0.0);
            let cur_lvl = projected_levels.get(&tid).copied().unwrap_or(0);
            hist.push(eve_db::SkillHistoryEntry {
                log_date_ft: now_ft,
                event_type_id: 39,
                skill_type_id: tid,
                skill_level: cur_lvl,
                absolute_points: cur_sp,
            });
        }

        // 8. Consume extractor item
        if let Some(it) = snap.items.get_mut(&extractor_item_id) {
            if it.quantity > 1 {
                it.quantity -= 1;
                it.version += 1;
            } else {
                snap.items.remove(&extractor_item_id);
            }
        }

        // 9. Decrement character allocated SP by 500,000
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.skill_points = (ch.skill_points - 500_000.0).max(0.0);
        }

        // 10. Mint Large Skill Injector (type 40520) in station hangar
        snap.next_entity_id += 1;
        let new_id = snap.next_entity_id;
        snap.items.insert(
            new_id,
            eve_db::InventoryItemRecord {
                item_id: new_id,
                type_id: 40520,
                owner_id: char_id,
                location_id: loc,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                version: 1,
                ..Default::default()
            },
        );
        Ok((removed_skills, new_id))
    })?;

    emit_server_skills_removed(ctx, char_id, &removed);
    Ok(PyRep::keyval([
        ("injectorItemID", PyRep::Integer(injector_id)),
        ("extractedSkills", PyRep::Integer(removed.len() as i64)),
    ]))
}

/// Emits `OnStationJumpCloneCacheInvalidated(locationID, charID)`, `OnJumpCloneCacheInvalidated()`,
/// and (when `is_jump` is true) `OnJumpCloneTransitionCompleted()`.
pub(crate) fn emit_station_jump_clone_invalidated(
    ctx: &ServerContext,
    char_id: i64,
    location_id: i64,
    is_jump: bool,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnStationJumpCloneCacheInvalidated",
        "charid",
        vec![PyRep::Integer(location_id), PyRep::Integer(char_id)],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnJumpCloneCacheInvalidated",
        "charid",
        vec![],
    );
    if is_jump {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnJumpCloneTransitionCompleted",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnShipJumpCloneInstallationOffered(offeringCharID, targetCharID, shipID, b)`.
pub(crate) fn emit_ship_clone_offered(
    ctx: &ServerContext,
    offerer_id: i64,
    target_char_id: i64,
    ship_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(target_char_id),
        "OnShipJumpCloneInstallationOffered",
        "charid",
        vec![
            PyRep::Integer(offerer_id),
            PyRep::Integer(target_char_id),
            PyRep::Integer(ship_id),
            PyRep::Bool(true),
        ],
    );
}

/// Emits `OnShipJumpCloneInstallationCanceled()` to the cancelling party and offerer.
pub(crate) fn emit_ship_clone_canceled(
    ctx: &ServerContext,
    char_id: i64,
    offerer_opt: Option<i64>,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnShipJumpCloneInstallationCanceled",
        "charid",
        vec![],
    );
    if let Some(offerer_id) = offerer_opt.filter(|&id| id != char_id) {
        ctx.notify(
            NotificationTarget::Character(offerer_id),
            "OnShipJumpCloneInstallationCanceled",
            "charid",
            vec![],
        );
    }
}

/// Emits `OnShipJumpCloneInstallationDone`, `OnShipJumpCloneCacheInvalidated`, and `OnJumpCloneCacheInvalidated`.
pub(crate) fn emit_ship_clone_accepted(
    ctx: &ServerContext,
    offerer_id: i64,
    target_char_id: i64,
    ship_id: i64,
) {
    let done_args = vec![
        PyRep::Integer(offerer_id),
        PyRep::Integer(target_char_id),
        PyRep::Integer(ship_id),
        PyRep::Bool(true),
    ];
    let inv_args = vec![PyRep::Integer(ship_id), PyRep::Integer(target_char_id)];
    for rcpt in [target_char_id, offerer_id] {
        ctx.notify(
            NotificationTarget::Character(rcpt),
            "OnShipJumpCloneInstallationDone",
            "charid",
            done_args.clone(),
        );
        ctx.notify(
            NotificationTarget::Character(rcpt),
            "OnShipJumpCloneCacheInvalidated",
            "charid",
            inv_args.clone(),
        );
        if rcpt == offerer_id && offerer_id == target_char_id {
            break;
        }
    }
    ctx.notify(
        NotificationTarget::Character(target_char_id),
        "OnJumpCloneCacheInvalidated",
        "charid",
        vec![],
    );
}

