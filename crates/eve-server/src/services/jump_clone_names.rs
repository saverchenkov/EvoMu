//! `jump_clone_names` — persisted labels for jump clones (`jumpCloneSvc.SetJumpCloneName`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
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
    let clone_id = args
        .first()
        .and_then(|a| a.as_i64())
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidJumpCloneID".into(),
            message: "SetJumpCloneName requires cloneID".into(),
        })?;
    let name = args
        .get(1)
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.chars().count() > MAX_JUMP_CLONE_NAME_LEN {
        return Err(BridgeRpcError {
            code: "JumpCloneNameTooLong".into(),
            message: format!(
                "Jump clone names are limited to {MAX_JUMP_CLONE_NAME_LEN} characters"
            ),
        });
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let owns = snap
            .jump_clones
            .get(&char_id)
            .is_some_and(|l| l.iter().any(|c| c.jump_clone_id == clone_id));
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
            snap.item_custom_names
                .insert(JUMP_CLONE_NAMES_STORE_KEY, json);
        }
        Ok(())
    })?;
    Ok(PyRep::None)
}

/// `ValidateInstallJumpClone`: returns a `PyRep::List` of V24.01 localization labels
/// (or `(label, kwargs)` tuples) describing why installing a jump clone is blocked,
/// or an empty list `[]` when all prerequisites are satisfied (#637).
pub fn validate_install_jump_clone(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let mut errors: Vec<PyRep> = Vec::new();
    if session.station_id.is_none() && session.structure_id.is_none() {
        errors.push(PyRep::String(
            "UI/Medical/Errors/JumpCloneValidationNotDocked".into(),
        ));
    }
    let (clone_count, max_clones, balance) = ctx.db.with_snapshot(|snap| {
        let count = snap.jump_clones.get(&char_id).map(|v| v.len()).unwrap_or(0);
        let bal = snap
            .characters
            .get(&char_id)
            .map(|c| c.balance.0)
            .unwrap_or(0);
        let skills = snap.skills.get(&char_id);
        let info_lvl = skills
            .and_then(|m| m.get(&24242))
            .map(|s| s.level as usize)
            .unwrap_or(0);
        let adv_lvl = skills
            .and_then(|m| m.get(&33407))
            .map(|s| s.level as usize)
            .unwrap_or(0);
        let elite_lvl = skills
            .and_then(|m| m.get(&73910))
            .map(|s| s.level as usize)
            .unwrap_or(0);
        (count, info_lvl + adv_lvl + elite_lvl, bal)
    });
    if max_clones == 0 {
        errors.push(PyRep::String(
            "UI/Medical/Errors/JumpCloneValidationSkillReqNotFulfilled".into(),
        ));
    } else if clone_count >= max_clones {
        errors.push(PyRep::String(
            "UI/Medical/Errors/JumpCloneValidationCloneCapacityReached".into(),
        ));
    }
    // Canonical jump clone installation fee: 900,000 ISK = 90,000,000 cents
    let clone_cost_cents = 90_000_000;
    if balance < clone_cost_cents {
        errors.push(PyRep::Tuple(vec![
            PyRep::String("UI/Medical/Errors/JumpCloneValidationInsufficientFunds".into()),
            PyRep::Dict(vec![(
                PyRep::String("cloneCost".into()),
                PyRep::Integer(900_000),
            )]),
        ]));
    }
    Ok(PyRep::List(errors))
}

/// `GetShipCloneState`: returns clones installed in a ship's Clone Vat Bay (`jumpCloneID`, `ownerID`, `locationID`, `cloneName`).
pub fn get_ship_clone_state(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (host_ship_id, caller_controls_ship) = ctx.db.with_snapshot(|snap| {
        let active_ship_id = session
            .ship_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id))
            .unwrap_or(0);
        let sid = args
            .first()
            .and_then(|a| a.as_i64())
            .or(session.ship_id)
            .unwrap_or(active_ship_id);
        let controls = sid > 0
            && (sid == active_ship_id
                || snap.items.get(&sid).is_some_and(|i| i.owner_id == char_id));
        (sid, controls)
    });
    if !caller_controls_ship {
        return Err(BridgeRpcError {
            code: "ShipAccessDenied".into(),
            message: "Caller must own or pilot the target ship to query its clone state".into(),
        });
    }
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
                let rank = ctx
                    .sde
                    .types
                    .get(&tid)
                    .and_then(|t| {
                        t.default_attributes
                            .get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT)
                            .copied()
                    })
                    .unwrap_or(1.0);
                let lvl = rec.level as i64;
                Some((
                    PyRep::Integer(tid as i64),
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("skillLevel", PyRep::Integer(lvl)),
                        ("trainedSkillLevel", PyRep::Integer(lvl)),
                        ("effectiveSkillLevel", PyRep::Integer(lvl)),
                        ("activeSkillLevel", PyRep::Integer(lvl)),
                        ("virtualSkillLevel", PyRep::None),
                        ("skillPoints", PyRep::Real(rec.skill_points)),
                        ("trainedSkillPoints", PyRep::Real(rec.skill_points)),
                        ("skillRank", PyRep::Real(rank)),
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
            event
                .map(|s| PyRep::String(s.into()))
                .unwrap_or(PyRep::None),
            PyRep::Integer(now_ft),
        ],
    );
}

/// Emits `OnServerSkillsRemoved(skillInfos, timeStamp)` matching `SkillsSvc.OnServerSkillsRemoved`.
pub(crate) fn emit_server_skills_removed(ctx: &ServerContext, char_id: i64, removed: &[(u32, u8)]) {
    if removed.is_empty() {
        return;
    }
    let entries: Vec<(PyRep, PyRep)> = removed
        .iter()
        .map(|&(tid, lvl)| {
            let rank = ctx
                .sde
                .types
                .get(&tid)
                .and_then(|t| {
                    t.default_attributes
                        .get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT)
                        .copied()
                })
                .unwrap_or(1.0);
            let sp = crate::formulas::sp_for_skill_level(rank, lvl);
            let lvl_i64 = lvl as i64;
            (
                PyRep::Integer(tid as i64),
                PyRep::keyval([
                    ("typeID", PyRep::Integer(tid as i64)),
                    ("skillLevel", PyRep::Integer(lvl_i64)),
                    ("trainedSkillLevel", PyRep::Integer(lvl_i64)),
                    ("effectiveSkillLevel", PyRep::Integer(lvl_i64)),
                    ("activeSkillLevel", PyRep::Integer(lvl_i64)),
                    ("virtualSkillLevel", PyRep::None),
                    ("skillPoints", PyRep::Real(sp)),
                    ("trainedSkillPoints", PyRep::Real(sp)),
                    ("skillRank", PyRep::Real(rank)),
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
    crate::services::character_skills::catch_up_skill_queue(ctx, char_id);

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

    let (removed, injector_id, pre_extractor, post_extractor, new_inj) =
        ctx.db.try_with_mut_snapshot(|snap| {
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
                    if q.started_at_unix.is_some() {
                        q.started_at_unix = Some(ctx.now_unix());
                    }
                }
                valid_next_lvl.insert(q.type_id, q.target_level);
                true
            });
            if !snap.paused_skill_queues.contains(&char_id) {
                if let Some(head) = queue.first_mut() {
                    head.started_at_unix.get_or_insert(ctx.now_unix());
                }
            }
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
        let pre_extractor = snap.items.get(&extractor_item_id).cloned();
        snap.consume_exact_item_quantity(
            extractor_item_id,
            1,
            eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id),
        )
        .map_err(|e| BridgeRpcError {
            code: "SkillExtractorConsumeFailed".into(),
            message: e.to_string(),
        })?;
        let post_extractor = snap.items.get(&extractor_item_id).cloned();

        // 9. Decrement character allocated SP by 500,000
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.skill_points = (ch.skill_points - 500_000.0).max(0.0);
        }

        // 10. Mint Large Skill Injector (type 40520) in station hangar
        snap.next_entity_id += 1;
        let new_id = snap.next_entity_id;
        let new_inj = eve_db::InventoryItemRecord {
            item_id: new_id,
            type_id: 40520,
            owner_id: char_id,
            location_id: loc,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 1,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(new_id, new_inj.clone());
        Ok((removed_skills, new_id, pre_extractor, post_extractor, new_inj))
    })?;

    ctx.notify_item_mutation(char_id, pre_extractor.as_ref(), post_extractor.as_ref());
    ctx.notify_item_mutation(char_id, None, Some(&new_inj));

    if let Some(sid) = session.ship_id {
        ctx.invalidate_ship_fitting(sid);
    }
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

pub fn alpha_skill_cap_for_character(
    sde: &eve_sde::SdeRegistry,
    race_id: u32,
    skill_type_id: TypeId,
) -> u8 {
    sde.clone_grades
        .get(&race_id)
        .and_then(|cg| cg.alpha_skill_caps.get(&skill_type_id).copied())
        .or_else(|| {
            sde.clone_grades
                .values()
                .filter_map(|cg| cg.alpha_skill_caps.get(&skill_type_id).copied())
                .max()
        })
        .unwrap_or(0)
}

pub(crate) fn is_valid_doll_or_portrait_payload(rep: &PyRep) -> bool {
    match rep {
        PyRep::KeyVal(map) => !map.is_empty(),
        PyRep::Dict(entries) => !entries.is_empty(),
        PyRep::Object { .. } | PyRep::ObjectEx { .. } => true,
        PyRep::Tuple(items) | PyRep::List(items) => !items.is_empty(),
        PyRep::String(s) => {
            let trimmed = s.trim();
            !trimmed.is_empty()
                && (trimmed.starts_with("doll:")
                    || trimmed.starts_with("portrait:")
                    || trimmed.contains('='))
                && trimmed != "doll_info_placeholder"
                && trimmed != "portrait_info_placeholder"
        }
        _ => false,
    }
}

pub(crate) fn serialize_doll_pyrep(rep: &PyRep) -> String {
    match rep {
        PyRep::String(s) => s.clone(),
        PyRep::Integer(v) => v.to_string(),
        PyRep::Real(v) => v.to_string(),
        PyRep::Bool(v) => v.to_string(),
        PyRep::None => String::new(),
        PyRep::Tuple(items) | PyRep::List(items) => {
            let parts: Vec<String> = items.iter().map(serialize_doll_pyrep).collect();
            format!("[{}]", parts.join(","))
        }
        PyRep::KeyVal(map) => {
            let mut entries: Vec<(&String, &PyRep)> = map.iter().collect();
            entries.sort_by_key(|(k, _)| *k);
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("{k}={}", serialize_doll_pyrep(v)))
                .collect();
            parts.join("&")
        }
        PyRep::Dict(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}={}", serialize_doll_pyrep(k), serialize_doll_pyrep(v)))
                .collect();
            parts.join("&")
        }
        _ => String::from("paperdoll"),
    }
}

/// Computes effective character attributes `(charisma, intelligence, memory, perception, willpower)`
/// combining base/remapped neural attributes + plugged-in implants (`175..=179`) + active cerebral accelerators (`InventoryFlag::Booster`) (`feat:attributes.attributes-base`).
pub fn effective_character_attributes(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> (f64, f64, f64, f64, f64) {
    let now_unix = snap
        .service_state_blobs
        .get("__server_now_unix")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    effective_character_attributes_at(sde, snap, char_id, now_unix)
}

pub fn effective_character_attributes_at(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: f64,
) -> (f64, f64, f64, f64, f64) {
    let (mut cha, mut int, mut mem, mut per, mut wil) = snap
        .characters
        .get(&char_id)
        .map(|c| {
            (
                c.neural.charisma as f64,
                c.neural.intelligence as f64,
                c.neural.memory as f64,
                c.neural.perception as f64,
                c.neural.willpower as f64,
            )
        })
        .unwrap_or((19.0, 20.0, 20.0, 20.0, 20.0));

    let active_implant_types = snap
        .active_implants
        .get(&char_id)
        .cloned()
        .unwrap_or_default();
    for &tid in &active_implant_types {
        if let Some(t) = sde.types.get(&tid) {
            cha += t.default_attributes.get(&175).copied().unwrap_or(0.0);
            int += t.default_attributes.get(&176).copied().unwrap_or(0.0);
            mem += t.default_attributes.get(&177).copied().unwrap_or(0.0);
            per += t.default_attributes.get(&178).copied().unwrap_or(0.0);
            wil += t.default_attributes.get(&179).copied().unwrap_or(0.0);
        }
    }

    for item in snap.items.values() {
        if item.owner_id != char_id || item.location_id != char_id {
            continue;
        }
        if item.flag == InventoryFlag::Implant && !active_implant_types.contains(&item.type_id) {
            let get_bonus = |attr_id: u16| -> f64 {
                item.dynamic_attributes
                    .get(&attr_id)
                    .copied()
                    .or_else(|| {
                        sde.types
                            .get(&item.type_id)
                            .and_then(|t| t.default_attributes.get(&attr_id).copied())
                    })
                    .unwrap_or(0.0)
            };
            cha += get_bonus(175);
            int += get_bonus(176);
            mem += get_bonus(177);
            per += get_bonus(178);
            wil += get_bonus(179);
        } else if item.flag == InventoryFlag::Booster {
            if now_unix > 0.0
                && item
                    .dynamic_attributes
                    .get(&crate::services::implant_plugging::DYN_ATTR_BOOSTER_EXPIRY_UNIX)
                    .is_some_and(|&exp| exp > 0.0 && now_unix >= exp)
            {
                continue;
            }
            let get_bonus = |bonus_id: u16, direct_id: u16| -> f64 {
                item.dynamic_attributes
                    .get(&bonus_id)
                    .copied()
                    .or_else(|| item.dynamic_attributes.get(&direct_id).copied())
                    .or_else(|| {
                        sde.types.get(&item.type_id).and_then(|t| {
                            t.default_attributes
                                .get(&bonus_id)
                                .copied()
                                .or_else(|| t.default_attributes.get(&direct_id).copied())
                        })
                    })
                    .unwrap_or(0.0)
            };
            cha += get_bonus(175, 164);
            int += get_bonus(176, 165);
            mem += get_bonus(177, 166);
            per += get_bonus(178, 167);
            wil += get_bonus(179, 168);
        }
    }

    (cha, int, mem, per, wil)
}

pub fn lookup_effective_attr_from_tuple(attrs: (f64, f64, f64, f64, f64), attr_id: u32) -> f64 {
    match attr_id {
        164 | 175 => attrs.0, // charisma
        165 | 176 => attrs.1, // intelligence
        166 | 177 => attrs.2, // memory
        167 | 178 => attrs.3, // perception
        168 | 179 => attrs.4, // willpower
        _ => attrs.1,
    }
}

/// Skill queue list of `utillib.KeyVal` entries as returned by `skillMgr2.GetSkillQueue`,
/// `skillMgr2.GetSkillQueueAndFreePoints`, and `OnNewSkillQueueSaved` (`#658`).
pub(crate) fn skill_queue_rowset(ctx: &ServerContext, char_id: i64) -> PyRep {
    let now_unix = ctx.now_unix();
    let entries = ctx.db.with_snapshot(|snap| {
        let paused = snap.paused_skill_queues.contains(&char_id);
        snap.skill_queues
            .get(&char_id)
            .map(|q| {
                let active_base_unix = if paused {
                    None
                } else {
                    q.first()
                        .and_then(|head| head.started_at_unix.or(Some(now_unix)))
                };
                let mut cur_ft = active_base_unix
                    .map(|u| (u as i64 * 10_000_000) + 116_444_736_000_000_000);
                q.iter()
                    .enumerate()
                    .map(|(idx, e)| {
                        let (start_rep, end_rep) = match cur_ft {
                            Some(start_ft) => {
                                let dur_ft = (e.duration_seconds.max(1.0) * 10_000_000.0) as i64;
                                let end_ft = start_ft + dur_ft;
                                cur_ft = Some(end_ft);
                                (PyRep::Integer(start_ft), PyRep::Integer(end_ft))
                            }
                            None => (PyRep::None, PyRep::None),
                        };
                        PyRep::keyval([
                            ("trainingTypeID", PyRep::Integer(e.type_id as i64)),
                            ("trainingToLevel", PyRep::Integer(e.target_level as i64)),
                            ("trainingStartSP", PyRep::Real(e.start_sp)),
                            ("trainingDestinationSP", PyRep::Real(e.end_sp)),
                            ("durationSeconds", PyRep::Real(e.duration_seconds)),
                            ("queuePosition", PyRep::Integer(idx as i64)),
                            ("trainingStartTime", start_rep),
                            ("trainingEndTime", end_rep),
                        ])
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    PyRep::List(entries)
}

pub(crate) async fn handle_public_qa_tools(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "MoveMeTo" => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let has_gm_or_qa = session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_QA) != 0;
            if !has_gm_or_qa {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "publicQaToolsServer.MoveMeTo requires GM/QA role".into(),
                }));
            }
            Some(handle_qa_move_me_to(ctx, session, cid, args).await)
        }
        "CanGiveItemForMultifit" => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_exists = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&cid));
            let has_gm_or_qa = session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_QA) != 0;
            Some(Ok(PyRep::Bool(char_exists && has_gm_or_qa)))
        }
        _ => None,
    }
}

pub(crate) async fn handle_qa_move_me_to(
    ctx: &ServerContext,
    session: &mut SessionState,
    cid: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let dest_raw = args
        .first()
        .and_then(|a| {
            a.as_i64()
                .or_else(|| a.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        })
        .unwrap_or(0);
    let dest_sys_id = if ctx.sde.solar_systems.contains_key(&(dest_raw as u32)) {
        dest_raw as u32
    } else if let Some(st) = ctx.sde.stations.get(&dest_raw) {
        st.solar_system_id
    } else {
        return Err(BridgeRpcError {
            code: "DestinationNotFound".into(),
            message: format!("Destination {dest_raw} not found in SDE"),
        });
    };
    let (old_sys_id, ship_id) = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters.get(&cid).map(|c| {
                let sys = if c.solar_system_id != 0 {
                    c.solar_system_id
                } else {
                    c.station_id
                        .and_then(|sid| ctx.sde.stations.get(&sid))
                        .map(|st| st.solar_system_id)
                        .unwrap_or(0)
                };
                (sys, c.active_ship_id)
            })
        })
        .unwrap_or_else(|| (session.solar_system_id.unwrap_or(0), session.ship_id.unwrap_or(0)));
    let updated_ch = match ctx
        .db
        .set_character_solar_system(cid, dest_sys_id, &ctx.sde)
    {
        Ok(c) => c,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "TeleportFailed".into(),
                message: e.to_string(),
            })
        }
    };
    if old_sys_id > 0 && old_sys_id != dest_sys_id && ship_id > 0 {
        let old_handle = ctx.systems.get_or_spawn(old_sys_id);
        let existing_ball = old_handle.remove_ball(ship_id).await;
        let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
        let ship_tid = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id))
            .unwrap_or(587);
        let mut new_ball = existing_ball.unwrap_or(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_tid,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        });
        new_ball.position = glam::DVec3::new(10_000.0, 0.0, 0.0);
        new_ball.velocity = glam::DVec3::ZERO;
        let combat_state = ctx.build_ship_combat_state(cid, ship_id, new_ball.type_id);
        let slim_meta = ctx.build_ship_slim_meta(cid, new_ball.type_id);
        let _ = new_handle
            .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
            .await;
    }
    let _ = ctx.execute_session_transition_sync(
        Some(session),
        crate::router::SessionTransitionSpec::for_kind(
            cid,
            crate::router::TransitionKind::JumpSystem { solar_system_id: dest_sys_id },
            "slash_teleport",
        )
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_sync_local_chat(false)
        .with_emit_on_session_changed(false),
    );
    Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(cid)),
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        (
            "constellationID",
            PyRep::Integer(updated_ch.constellation_id as i64),
        ),
        ("regionID", PyRep::Integer(updated_ch.region_id as i64)),
        ("moved", PyRep::Bool(true)),
    ]))
}

#[cfg(test)] #[path = "jump_clone_names_tests.rs"] mod jump_clone_names_tests;

#[path = "jump_clone_names_cov_unit_tests.rs"]
#[cfg(test)]
mod jump_clone_names_cov_unit_tests;

