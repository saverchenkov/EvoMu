//! `character_contacts` — Character Sheet, Biography, Owner Notes, Contacts, Labels, Blocklist, & Home Station (`CH-4`).

#![forbid(unsafe_code)]

use crate::esi::runtime_state::EsiContactEntry;
use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub fn parse_id_list(arg: Option<&PyRep>) -> Vec<i64> {
    match arg {
        Some(PyRep::List(items) | PyRep::Tuple(items)) => {
            items.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    }
}

pub fn label_mask_from_ids(label_ids: &[i64]) -> i64 {
    label_ids.iter().copied().fold(0i64, |acc, id| acc | id)
}

pub fn is_blocked_by(ctx: &ServerContext, target_char_id: i64, sender_char_id: i64) -> bool {
    ctx.esi_state
        .char_contacts
        .read()
        .ok()
        .and_then(|g| {
            g.get(&target_char_id).and_then(|list| {
                list.iter()
                    .find(|e| e.contact_id == sender_char_id)
                    .map(|e| e.is_blocked)
            })
        })
        .unwrap_or(false)
}

pub fn is_character_online(ctx: &ServerContext, target_char_id: i64) -> bool {
    let uid_opt = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&target_char_id).map(|c| c.user_id)
    });
    // Online means the account's active session has this character selected (chat-channel
    // membership is not presence).
    uid_opt.is_some_and(|uid| {
        ctx.db
            .get_online_session(uid)
            .is_some_and(|os| os.character_id == Some(target_char_id))
    })
}

/// Emits `OnClientContactChange(charID, online)` to all online characters who have `logged_on_char_id`
/// on their watchlist and are not blocked by it (`feat:social.contacts-buddy-online`, `feat:social.contacts-block`).
/// Client: `eve/client/script/ui/shared/comtool/onlineStatus.pyj` `OnlineStatus.OnClientContactChange`
/// calls its own `OnContactLoggedOn`/`OnContactLoggedOff` from the `online` flag.
pub fn notify_watchlist_logon(ctx: &ServerContext, logged_on_char_id: i64) {
    notify_watchlist_presence(ctx, logged_on_char_id, true);
}

/// See [`notify_watchlist_logon`]; `online == false` is sent when the watched character logs out.
pub fn notify_watchlist_presence(ctx: &ServerContext, logged_on_char_id: i64, online: bool) {
    let watchers: Vec<i64> = ctx
        .esi_state
        .char_contacts
        .read()
        .ok()
        .map(|g| {
            let blocked_by_target = |watcher_id: i64| -> bool {
                g.get(&logged_on_char_id)
                    .and_then(|list| {
                        list.iter()
                            .find(|e| e.contact_id == watcher_id)
                            .map(|e| e.is_blocked)
                    })
                    .unwrap_or(false)
            };
            g.iter()
                .filter_map(|(&watcher_id, contacts)| {
                    if watcher_id != logged_on_char_id
                        && contacts
                            .iter()
                            .any(|c| c.contact_id == logged_on_char_id && c.is_watched)
                        && !blocked_by_target(watcher_id)
                    {
                        Some(watcher_id)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    for watcher_id in watchers {
        ctx.notify(
            NotificationTarget::Character(watcher_id),
            "OnClientContactChange",
            "charid",
            vec![PyRep::Integer(logged_on_char_id), PyRep::Bool(online)],
        );
    }
}


pub fn handle_contacts(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charMgr", "GetPublicInfo3") => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetPublicInfo3 requires an explicit charID or selected character".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&cid).cloned().map(|mut c| {
                    if c.alliance_id.is_none() {
                        c.alliance_id = snap
                            .corporations
                            .get(&c.corporation_id)
                            .and_then(|co| co.alliance_id);
                    }
                    let (_, portrait) = snap
                        .character_doll_info
                        .get(&cid)
                        .cloned()
                        .unwrap_or_else(|| (String::new(), format!("portrait:bloodline:{}", c.bloodline_id)));
                    (c, portrait)
                })
            });
            let Some((c, portrait_info)) = res else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {cid} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("characterName", PyRep::String(c.name)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("allianceID", c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("raceID", PyRep::Integer(c.race_id as i64)),
                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                ("schoolID", PyRep::Integer(c.school_id as i64)),
                ("securityStatus", PyRep::Real(c.security_status)),
                ("description", PyRep::String(c.description)),
                ("title", PyRep::String(c.title)),
                ("gender", PyRep::Integer(c.gender as i64)),
                ("createDateTime", PyRep::Integer(c.create_date_unix as i64)),
                ("portraitInfo", PyRep::String(portrait_info)),
            ])))
        }

        ("charMgr", "GetPrivateInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_cid = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            if target_cid != char_id {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipMismatch".into(),
                    message: format!("GetPrivateInfo can only be queried for the active character {char_id}"),
                }));
            }
            let Some(c) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).cloned()) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                }));
            };
            let (skills_list, attrs_kv, implants_list, jump_clones_list, standings_list, kill_rights_list) =
                ctx.db.with_snapshot(|snap| {
                    let mut skills_list = Vec::new();
                    if let Some(smap) = snap.skills.get(&char_id) {
                        let mut sorted: Vec<_> = smap.values().collect();
                        sorted.sort_by_key(|s| s.type_id);
                        for s in sorted {
                            skills_list.push(PyRep::keyval([
                                ("typeID", PyRep::Integer(s.type_id as i64)),
                                ("skillLevel", PyRep::Integer(s.level as i64)),
                                ("skillPoints", PyRep::Real(s.skill_points)),
                            ]));
                        }
                    }
                    let (eff_cha, eff_int, eff_mem, eff_per, eff_wil) =
                        crate::services::character_skills::effective_character_attributes(&ctx.sde, snap, char_id);
                    let attrs_kv = PyRep::keyval([
                        ("charisma", PyRep::Integer(eff_cha.round() as i64)),
                        ("intelligence", PyRep::Integer(eff_int.round() as i64)),
                        ("memory", PyRep::Integer(eff_mem.round() as i64)),
                        ("perception", PyRep::Integer(eff_per.round() as i64)),
                        ("willpower", PyRep::Integer(eff_wil.round() as i64)),
                    ]);
                    let implants_list: Vec<PyRep> = snap
                        .active_implants
                        .get(&char_id)
                        .map(|v| v.iter().map(|&tid| PyRep::Integer(tid as i64)).collect())
                        .unwrap_or_default();
                    let jump_clones_list: Vec<PyRep> = snap
                        .jump_clones
                        .get(&char_id)
                        .map(|v| {
                            v.iter()
                                .map(|jc| {
                                    PyRep::keyval([
                                        ("jumpCloneID", PyRep::Integer(jc.jump_clone_id)),
                                        ("locationID", PyRep::Integer(jc.location_id)),
                                        ("implants", PyRep::List(jc.implants.iter().map(|&t| PyRep::Integer(t as i64)).collect())),
                                    ])
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let standings_list: Vec<PyRep> = c
                        .standings
                        .iter()
                        .map(|(&fid, &val)| PyRep::keyval([("fromID", PyRep::Integer(fid)), ("standing", PyRep::Real(val))]))
                        .collect();
                    let kill_rights_list: Vec<PyRep> = snap
                        .kill_rights
                        .values()
                        .filter(|kr| kr.owner_id == char_id || kr.target_id == char_id)
                        .map(|kr| PyRep::keyval([("killRightID", PyRep::Integer(kr.kill_right_id)), ("fromID", PyRep::Integer(kr.owner_id)), ("toID", PyRep::Integer(kr.target_id))]))
                        .collect();
                    (skills_list, attrs_kv, implants_list, jump_clones_list, standings_list, kill_rights_list)
                });
            let (medals_list, decorations_list) =
                crate::services::fleet_social::fleet_social_corps::get_character_medals_and_decorations(char_id);
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("charName", PyRep::String(c.name.clone())),
                ("characterName", PyRep::String(c.name)),
                ("gender", PyRep::Integer(c.gender as i64)),
                ("createDateTime", PyRep::Integer(c.create_date_unix as i64)),
                ("raceID", PyRep::Integer(c.race_id as i64)),
                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                ("schoolID", PyRep::Integer(c.school_id as i64)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("allianceID", c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("balance", PyRep::Real(c.balance.to_wire_f64())),
                ("skillPoints", PyRep::Real(c.skill_points)),
                ("freeSkillPoints", PyRep::Real(c.free_skill_points)),
                ("securityStatus", PyRep::Real(c.security_status)),
                ("cloneStationID", PyRep::Integer(c.home_station_id)),
                ("skills", PyRep::List(skills_list)),
                ("attributes", attrs_kv),
                ("implants", PyRep::List(implants_list)),
                ("jumpClones", PyRep::List(jump_clones_list)),
                ("standings", PyRep::List(standings_list)),
                ("killRights", PyRep::List(kill_rights_list)),
                ("medals", PyRep::List(medals_list)),
                ("decorations", PyRep::List(decorations_list)),
            ])))
        }

        ("charMgr", "GetCharacterDescription") => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetCharacterDescription requires charID or selected character".into(),
                }));
            };
            let desc = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&cid).map(|c| c.description.clone())
            }).unwrap_or_default();
            Some(Ok(PyRep::String(desc)))
        }

        ("charMgr", "SetCharacterDescription") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let desc = args.first().and_then(|a| a.as_str()).unwrap_or("");
            if desc.len() > 5000 {
                return Some(Err(BridgeRpcError {
                    code: "DescriptionTooLong".into(),
                    message: "Character description exceeds 5000 characters limit".into(),
                }));
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.description = desc.to_string();
                }
            });
            Some(Ok(PyRep::None))
        }

        ("corporationSvc", "GetEmployementRecordAndCharacterTransfers")
        | ("charMgr", "GetEmployementRecordAndCharacterTransfers") => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetEmployementRecordAndCharacterTransfers requires charID or selected character".into(),
                }));
            };
            let mut rows = Vec::new();
            if let Some(c) = ctx.db.with_snapshot(|snap| snap.characters.get(&cid).cloned()) {
                rows.push(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(c.corporation_id),
                    PyRep::Integer(c.create_date_unix as i64),
                ]);
            }
            Some(Ok(PyRep::rowset(
                vec!["recordID", "corporationID", "startDate"],
                rows,
            )))
        }

        ("charMgr", "AddOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let label = args.get(1).and_then(|a| a.as_str()).unwrap_or("Default").to_string();
            let note = args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let now_unix = ctx.now_unix();
            let note_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.owner_notes.insert(id, eve_db::OwnerNoteRecord {
                    note_id: id,
                    character_id: char_id,
                    item_id: target_id,
                    label,
                    note,
                    timestamp_unix: now_unix,
                });
                id
            });
            Some(Ok(PyRep::Integer(note_id)))
        }

        ("charMgr", "GetOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.owner_notes
                    .values()
                    .filter(|n| n.character_id == char_id && n.item_id == target_id)
                    .map(|n| {
                        vec![
                            PyRep::Integer(n.note_id),
                            PyRep::Integer(n.item_id),
                            PyRep::String(n.label.clone()),
                            PyRep::String(n.note.clone()),
                            PyRep::Integer(n.timestamp_unix as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["noteID", "itemID", "label", "note", "timestamp"],
                rows,
            )))
        }

        ("charMgr", "EditOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let note_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let label = args.get(1).and_then(|a| a.as_str());
            let note = args.get(2).and_then(|a| a.as_str());
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(n) = snap.owner_notes.get_mut(&note_id) {
                    if n.character_id == char_id {
                        if let Some(l) = label { n.label = l.to_string(); }
                        if let Some(nt) = note { n.note = nt.to_string(); }
                        n.timestamp_unix = ctx.now_unix();
                        return true;
                    }
                }
                false
            });
            Some(Ok(PyRep::Bool(updated)))
        }

        ("charMgr", "RemoveOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let note_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let removed = ctx.db.with_mut_snapshot(|snap| {
                if let Some(n) = snap.owner_notes.get(&note_id) {
                    if n.character_id == char_id {
                        snap.owner_notes.remove(&note_id);
                        return true;
                    }
                }
                false
            });
            Some(Ok(PyRep::Bool(removed)))
        }

        ("charMgr", "GetOwnerNoteLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let labels = ctx.db.with_snapshot(|snap| {
                let mut set = std::collections::BTreeSet::new();
                for n in snap.owner_notes.values() {
                    if n.character_id == char_id {
                        set.insert(n.label.clone());
                    }
                }
                set.into_iter().map(PyRep::String).collect()
            });
            Some(Ok(PyRep::List(labels)))
        }

        ("charMgr", "RespecCharacter")
        | ("skillMgr2", "RespecCharacter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (cha_opt, int_opt, mem_opt, per_opt, wil_opt) = match args.first() {
                Some(PyRep::Dict(map)) => {
                    let get_u8 = |k: &str| {
                        map.iter()
                            .find(|(key, _)| key.as_str() == Some(k))
                            .and_then(|(_, v)| v.as_i64())
                            .map(|v| v as u8)
                    };
                    (
                        get_u8("charisma"),
                        get_u8("intelligence"),
                        get_u8("memory"),
                        get_u8("perception"),
                        get_u8("willpower"),
                    )
                }
                Some(PyRep::KeyVal(kv)) => (
                    kv.get("charisma").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("intelligence").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("memory").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("perception").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("willpower").and_then(|v| v.as_i64()).map(|v| v as u8),
                ),
                _ => (
                    args.first()
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("charisma").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(1)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("intelligence").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(2)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("memory").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(3)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("perception").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(4)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("willpower").and_then(|v| v.as_i64()).map(|v| v as u8)),
                ),
            };
            let (Some(cha), Some(int), Some(mem), Some(per), Some(wil)) =
                (cha_opt, int_opt, mem_opt, per_opt, wil_opt)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRemapArguments".into(),
                    message: "Remap requires charisma, intelligence, memory, perception, and willpower values".into(),
                }));
            };

            let now_unix = ctx.now_unix();
            match ctx.db.remap_neural_attributes(char_id, cha, int, mem, per, wil, now_unix) {
                Ok(ch) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnRespecInfoChanged",
                        "charid",
                        vec![],
                    );
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(char_id)),
                        ("charisma", PyRep::Integer(ch.neural.charisma as i64)),
                        ("intelligence", PyRep::Integer(ch.neural.intelligence as i64)),
                        ("memory", PyRep::Integer(ch.neural.memory as i64)),
                        ("perception", PyRep::Integer(ch.neural.perception as i64)),
                        ("willpower", PyRep::Integer(ch.neural.willpower as i64)),
                        ("bonusRemaps", PyRep::Integer(ch.neural.bonus_remaps as i64)),
                        (
                            "lastRemapUnix",
                            PyRep::Integer(ch.neural.last_remap_unix.unwrap_or(0) as i64),
                        ),
                    (
                        "remapCooldownUntilUnix",
                        PyRep::Integer(ch.neural.remap_cooldown_until_unix.unwrap_or(0) as i64),
                    ),
                ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "RemapFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("charMgr", "GetContactList") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !char_exists {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                }));
            }
            let mut address_rows = Vec::new();
            let mut blocked_rows = Vec::new();
            if let Ok(guard) = ctx.esi_state.char_contacts.read() {
                if let Some(list) = guard.get(&char_id) {
                    let mut entries: Vec<_> = list.iter().collect();
                    entries.sort_by_key(|e| e.contact_id);
                    for entry in entries {
                        let mask = label_mask_from_ids(&entry.label_ids);
                        address_rows.push(vec![
                            PyRep::Integer(entry.contact_id),
                            PyRep::Bool(entry.is_watched),
                            PyRep::Real(entry.standing),
                            PyRep::Integer(mask),
                        ]);
                        if entry.is_blocked {
                            blocked_rows.push(vec![PyRep::Integer(entry.contact_id)]);
                        }
                    }
                }
            }
            let mut label_rows = Vec::new();
            if let Ok(lguard) = ctx.esi_state.char_contact_labels.read() {
                if let Some(labels) = lguard.get(&char_id) {
                    for (lid, lname) in labels {
                        label_rows.push(vec![
                            PyRep::Integer(*lid),
                            PyRep::String(lname.clone()),
                        ]);
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                (
                    "addresses",
                    PyRep::rowset(
                        vec!["contactID", "inWatchlist", "relationshipID", "labelMask"],
                        address_rows,
                    ),
                ),
                ("blocked", PyRep::rowset(vec!["contactID"], blocked_rows)),
                (
                    "labels",
                    PyRep::rowset(vec!["labelID", "name"], label_rows),
                ),
            ])))
        }

        ("charMgr", "AddContact") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(contact_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AddContact requires contactID".into(),
                }));
            };
            if contact_id <= 0 || contact_id == char_id {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContactID".into(),
                    message: "contactID must be a positive entity ID distinct from self".into(),
                }));
            }
            let valid_entity = ctx.db.with_snapshot(|snap| {
                snap.characters.contains_key(&contact_id)
                    || snap.corporations.contains_key(&contact_id)
                    || snap.alliances.contains_key(&contact_id)
                    || contact_id >= 1_000
            });
            if !valid_entity {
                return Some(Err(BridgeRpcError {
                    code: "ContactNotFound".into(),
                    message: format!("Contact entity {contact_id} not found"),
                }));
            }
            let standing = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(0.0)
                .clamp(-10.0, 10.0);
            let in_watchlist = args
                .get(2)
                .and_then(|a| match a {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(i) => Some(*i != 0),
                    _ => None,
                })
                .unwrap_or(false);
            let contact_type = if (99_000_000..100_000_000).contains(&contact_id) {
                "alliance"
            } else if (1_000_000..2_000_000).contains(&contact_id)
                || (98_000_000..99_000_000).contains(&contact_id)
            {
                "corporation"
            } else {
                "character"
            };

            let mut guard = match ctx.esi_state.char_contacts.write() {
                Ok(g) => g,
                Err(_) => {
                    return Some(Err(BridgeRpcError {
                        code: "LockPoisoned".into(),
                        message: "Contact state lock poisoned".into(),
                    }))
                }
            };
            let list = guard.entry(char_id).or_default();
            let existing_pos = list.iter().position(|e| e.contact_id == contact_id);
            if existing_pos.is_none() && list.len() >= 1_024 {
                return Some(Err(BridgeRpcError {
                    code: "MaxContactsExceeded".into(),
                    message: "Character has reached the maximum of 1,024 contacts".into(),
                }));
            }
            let (is_blocked, label_mask) = if let Some(idx) = existing_pos {
                list[idx].standing = standing;
                list[idx].is_watched = in_watchlist;
                (list[idx].is_blocked, label_mask_from_ids(&list[idx].label_ids))
            } else {
                list.push(EsiContactEntry {
                    contact_id,
                    contact_type: contact_type.to_string(),
                    standing,
                    is_watched: in_watchlist,
                    is_blocked: false,
                    label_ids: Vec::new(),
                });
                (false, 0)
            };
            drop(guard);

            // AddressBookSvc.OnPersonalContactsAdded(contacts, relationshipID, watchlist, labelMask):
            // each contact is read as `contact.contactID` / `contact.isOnline`. (`OnContactChange` is
            // scattered client-side by that handler, so the server does not send it.)
            let contact = PyRep::keyval([
                ("contactID", PyRep::Integer(contact_id)),
                // A contact that has blocked us never reveals presence (as notify_watchlist_presence).
                ("isOnline", PyRep::Bool(!is_blocked_by(ctx, contact_id, char_id) && is_character_online(ctx, contact_id))),
            ]);
            if ctx.sde.agents.contains_key(&contact_id) {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnAgentAdded",
                    "charid",
                    vec![PyRep::Integer(contact_id)],
                );
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnPersonalContactsAdded",
                "charid",
                vec![
                    PyRep::List(vec![contact]),
                    PyRep::Real(standing),
                    PyRep::Bool(in_watchlist),
                    PyRep::Integer(label_mask),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(contact_id)),
                ("standing", PyRep::Real(standing)),
                ("inWatchlist", PyRep::Bool(in_watchlist)),
                ("blocked", PyRep::Bool(is_blocked)),
                ("labelMask", PyRep::Integer(label_mask)),
            ])))
        }

        ("charMgr", "EditContact") | ("charMgr", "EditContactsRelationshipID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let contact_ids = parse_id_list(args.first());
            if contact_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("{method} requires at least one contactID"),
                }));
            }
            let standing = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(0.0)
                .clamp(-10.0, 10.0);
            let watchlist_opt = if method == "EditContact" {
                args.get(2).and_then(|a| match a {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(i) => Some(*i != 0),
                    _ => None,
                })
            } else {
                None
            };

            let mut updated_count = 0usize;
            if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
                let list = guard.entry(char_id).or_default();
                for &cid in &contact_ids {
                    if let Some(entry) = list.iter_mut().find(|e| e.contact_id == cid) {
                        entry.standing = standing;
                        if let Some(w) = watchlist_opt {
                            entry.is_watched = w;
                        }
                    } else {
                        list.push(EsiContactEntry {
                            contact_id: cid,
                            contact_type: "character".to_string(),
                            standing,
                            is_watched: watchlist_opt.unwrap_or(false),
                            is_blocked: false,
                            label_ids: Vec::new(),
                        });
                    }
                    updated_count += 1;
                }
            }
            let id_reps: Vec<PyRep> = contact_ids.iter().map(|&id| PyRep::Integer(id)).collect();
            // AddressBookSvc.OnPersonalContactsUpdated(contactIDs, standing, inWatchlist): `None` keeps
            // the client's current value (EditContactsRelationshipID leaves the watchlist untouched).
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnPersonalContactsUpdated",
                "charid",
                vec![
                    PyRep::List(id_reps.clone()),
                    PyRep::Real(standing),
                    watchlist_opt.map(PyRep::Bool).unwrap_or(PyRep::None),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("updatedCount", PyRep::Integer(updated_count as i64)),
                ("standing", PyRep::Real(standing)),
                ("contactIDs", PyRep::List(id_reps)),
            ])))
        }

        ("charMgr", "DeleteContacts") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let contact_ids = parse_id_list(args.first());
            if contact_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteContacts requires contactIDs".into(),
                }));
            }
            let mut removed = 0usize;
            if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
                if let Some(list) = guard.get_mut(&char_id) {
                    let before = list.len();
                    list.retain(|e| !contact_ids.contains(&e.contact_id));
                    removed = before.saturating_sub(list.len());
                }
            }
            let id_reps: Vec<PyRep> = contact_ids.iter().map(|&id| PyRep::Integer(id)).collect();
            // AddressBookSvc.OnPersonalContactsDeleted(contacts): each entry is read as `contact.contactID`.
            let deleted: Vec<PyRep> = contact_ids
                .iter()
                .map(|&id| PyRep::keyval([("contactID", PyRep::Integer(id))]))
                .collect();
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnPersonalContactsDeleted",
                "charid",
                vec![PyRep::List(deleted)],
            );
            Some(Ok(PyRep::keyval([
                ("deletedCount", PyRep::Integer(removed as i64)),
                ("contactIDs", PyRep::List(id_reps)),
            ])))
        }

        ("charMgr", "BlockOwners") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let owner_ids = parse_id_list(args.first());
            if owner_ids.is_empty() || owner_ids.iter().any(|&id| id <= 0 || id == char_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "BlockOwners requires valid positive ownerIDs distinct from self"
                        .into(),
                }));
            }
            let mut blocked_count = 0usize;
            if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
                let list = guard.entry(char_id).or_default();
                for &oid in &owner_ids {
                    if let Some(entry) = list.iter_mut().find(|e| e.contact_id == oid) {
                        entry.is_blocked = true;
                        blocked_count += 1;
                    } else {
                        if list.len() >= 1_024 {
                            return Some(Err(BridgeRpcError {
                                code: "MaxContactsExceeded".into(),
                                message: "Character has reached the maximum of 1,024 contacts/blocked owners".into(),
                            }));
                        }
                        list.push(EsiContactEntry {
                            contact_id: oid,
                            contact_type: "character".to_string(),
                            standing: 0.0,
                            is_watched: false,
                            is_blocked: true,
                            label_ids: Vec::new(),
                        });
                        blocked_count += 1;
                    }
                }
            }
            let id_reps: Vec<PyRep> = owner_ids.iter().map(|&id| PyRep::Integer(id)).collect();
            // `OnBlockContacts` is scattered by the client itself (AddressBookSvc.BlockOwner).
            Some(Ok(PyRep::keyval([
                ("blockedCount", PyRep::Integer(blocked_count as i64)),
                ("blockedIDs", PyRep::List(id_reps)),
            ])))
        }

        ("charMgr", "UnblockOwners") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let owner_ids = parse_id_list(args.first());
            if owner_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "UnblockOwners requires ownerIDs".into(),
                }));
            }
            let mut unblocked_count = 0usize;
            if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
                if let Some(list) = guard.get_mut(&char_id) {
                    for &oid in &owner_ids {
                        if let Some(entry) = list.iter_mut().find(|e| e.contact_id == oid) {
                            if entry.is_blocked {
                                entry.is_blocked = false;
                                unblocked_count += 1;
                            }
                        }
                    }
                }
            }
            let id_reps: Vec<PyRep> = owner_ids.iter().map(|&id| PyRep::Integer(id)).collect();
            // `OnUnblockContacts` is scattered by the client itself (AddressBookSvc.UnblockOwner);
            // ContactsForm.OnUnblockContact(contactID) only reloads the contacts tab.
            for &oid in &owner_ids {
                ctx.notify(NotificationTarget::Character(char_id), "OnUnblockContact", "charid", vec![PyRep::Integer(oid)]);
            }
            Some(Ok(PyRep::keyval([
                ("unblockedCount", PyRep::Integer(unblocked_count as i64)),
                ("unblockedIDs", PyRep::List(id_reps)),
            ])))
        }

        ("charMgr", "CreateLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let Some(name) = args
                .first()
                .and_then(|a| a.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLabelName".into(),
                    message: "CreateLabel requires a non-empty label name".into(),
                }));
            };
            let mut guard = match ctx.esi_state.char_contact_labels.write() {
                Ok(g) => g,
                Err(_) => {
                    return Some(Err(BridgeRpcError {
                        code: "LockPoisoned".into(),
                        message: "Contact labels lock poisoned".into(),
                    }))
                }
            };
            let labels = guard.entry(char_id).or_default();
            let used_mask = labels.iter().fold(0i64, |acc, (lid, _)| acc | *lid);
            let mut label_id = 1i64;
            while (used_mask & label_id) != 0 && label_id < (1i64 << 60) {
                label_id <<= 1;
            }
            labels.push((label_id, name.to_string()));
            Some(Ok(PyRep::Integer(label_id)))
        }

        ("charMgr", "GetLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let mut dict_entries = Vec::new();
            if let Ok(guard) = ctx.esi_state.char_contact_labels.read() {
                if let Some(labels) = guard.get(&char_id) {
                    for (lid, lname) in labels {
                        dict_entries.push((
                            PyRep::Integer(*lid),
                            PyRep::keyval([
                                ("labelID", PyRep::Integer(*lid)),
                                ("name", PyRep::String(lname.clone())),
                            ]),
                        ));
                    }
                }
            }
            Some(Ok(PyRep::Dict(dict_entries)))
        }

        ("charMgr", "EditLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "EditLabel requires labelID".into(),
                }));
            };
            let Some(new_name) = args
                .get(1)
                .and_then(|a| a.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLabelName".into(),
                    message: "EditLabel requires a non-empty label name".into(),
                }));
            };
            let mut found = false;
            if let Ok(mut guard) = ctx.esi_state.char_contact_labels.write() {
                if let Some(labels) = guard.get_mut(&char_id) {
                    if let Some((_, lname)) = labels.iter_mut().find(|(lid, _)| *lid == label_id) {
                        *lname = new_name.to_string();
                        found = true;
                    }
                }
            }
            if !found {
                return Some(Err(BridgeRpcError {
                    code: "LabelNotFound".into(),
                    message: format!("Contact label {label_id} not found"),
                }));
            }
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(label_id)),
                ("name", PyRep::String(new_name.to_string())),
            ])))
        }

        ("charMgr", "DeleteLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let Some(label_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteLabel requires labelID".into(),
                }));
            };
            let mut removed = false;
            if let Ok(mut guard) = ctx.esi_state.char_contact_labels.write() {
                if let Some(labels) = guard.get_mut(&char_id) {
                    let before = labels.len();
                    labels.retain(|(lid, _)| *lid != label_id);
                    removed = labels.len() < before;
                }
            }
            if let Ok(mut cguard) = ctx.esi_state.char_contacts.write() {
                if let Some(list) = cguard.get_mut(&char_id) {
                    for entry in list.iter_mut() {
                        entry.label_ids.retain(|&id| id != label_id);
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(label_id)),
                ("deleted", PyRep::Bool(removed)),
            ])))
        }

        ("charMgr", "AssignLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let contact_ids = parse_id_list(args.first());
            let label_mask = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            if contact_ids.is_empty() || label_mask <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AssignLabels requires contactIDs and positive labelMask".into(),
                }));
            }
            let mut updated = 0usize;
            if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
                if let Some(list) = guard.get_mut(&char_id) {
                    for &cid in &contact_ids {
                        if let Some(entry) = list.iter_mut().find(|e| e.contact_id == cid) {
                            for bit in 0..62 {
                                let bit_val = 1i64 << bit;
                                if (label_mask & bit_val) != 0
                                    && !entry.label_ids.contains(&bit_val)
                                {
                                    entry.label_ids.push(bit_val);
                                }
                            }
                            updated += 1;
                        }
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("updatedCount", PyRep::Integer(updated as i64)),
                ("labelMask", PyRep::Integer(label_mask)),
            ])))
        }

        ("charMgr", "RemoveLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            let contact_ids = parse_id_list(args.first());
            let label_mask = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            if contact_ids.is_empty() || label_mask <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveLabels requires contactIDs and positive labelMask".into(),
                }));
            }
            let mut updated = 0usize;
            if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
                if let Some(list) = guard.get_mut(&char_id) {
                    for &cid in &contact_ids {
                        if let Some(entry) = list.iter_mut().find(|e| e.contact_id == cid) {
                            entry.label_ids.retain(|&id| (label_mask & id) == 0);
                            updated += 1;
                        }
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("updatedCount", PyRep::Integer(updated as i64)),
                ("labelMask", PyRep::Integer(label_mask)),
            ])))
        }

        ("onlineStatus", "GetOnlineStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_cid) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetOnlineStatus requires target characterID".into(),
                }));
            };
            if is_blocked_by(ctx, target_cid, char_id) {
                return Some(Ok(PyRep::Bool(false)));
            }
            let online = is_character_online(ctx, target_cid);
            Some(Ok(PyRep::Bool(online)))
        }

        ("onlineStatus", "GetInitialState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let watched_ids: Vec<i64> = ctx
                .esi_state
                .char_contacts
                .read()
                .ok()
                .and_then(|g| {
                    g.get(&char_id).map(|list| {
                        let mut ids: Vec<i64> = list
                            .iter()
                            .filter(|e| e.is_watched)
                            .map(|e| e.contact_id)
                            .collect();
                        ids.sort_unstable();
                        ids
                    })
                })
                .unwrap_or_default();
            let rows: Vec<Vec<PyRep>> = watched_ids
                .into_iter()
                .map(|cid| {
                    let online =
                        !is_blocked_by(ctx, cid, char_id) && is_character_online(ctx, cid);
                    vec![PyRep::Integer(cid), PyRep::Bool(online)]
                })
                .collect();
            Some(Ok(PyRep::rowset(vec!["contactID", "online"], rows)))
        }

        ("home_station", "get_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            match ctx.db.get_character(char_id) {
                Ok(ch) => Some(Ok(PyRep::Integer(ch.home_station_id))),
                Err(e) => Some(Err(BridgeRpcError { code: "CharacterNotFound".into(), message: e.to_string() })),
            }
        }

        ("home_station", "set_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(new_home_id) = args.first().and_then(|a| a.as_i64()).or(session.station_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtTargetStation".into(),
                    message: "set_home_station requires a target station ID".into(),
                }));
            };
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
            let now_unix = ctx.now_unix_secs();
            let is_local_docked = session.station_id == Some(new_home_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let (school_id, corp_id) = (ch.school_id, ch.corporation_id);
                if !is_local_docked {
                    let school_st = ctx.sde.schools.get(&school_id).map(|s| s.station_id).filter(|&id| id > 0);
                    let corp_hq_st = snap.corporations.get(&corp_id).and_then(|c| c.hq_station_id).filter(|&id| id > 0);
                    if Some(new_home_id) != school_st && Some(new_home_id) != corp_hq_st {
                        return Err(BridgeRpcError {
                            code: "MustBeDockedAtTargetStation".into(),
                            message: format!("Station {new_home_id} is not a remote candidate"),
                        });
                    }
                    let cd_key = -10_000_099 - char_id;
                    let next_allowed = snap.owner_notes.get(&cd_key).and_then(|n| n.note.parse::<u64>().ok()).unwrap_or(0);
                    if now_unix < next_allowed {
                        return Err(BridgeRpcError {
                            code: "HomeStationRemoteCooldownActive".into(),
                            message: format!("Remote home station change on cooldown until {next_allowed}"),
                        });
                    }
                    let next_time = now_unix + 365 * 86_400;
                    snap.owner_notes.insert(cd_key, eve_db::OwnerNoteRecord {
                        note_id: cd_key,
                        character_id: -char_id,
                        item_id: new_home_id,
                        label: "home_station_remote_cooldown".into(),
                        note: next_time.to_string(),
                        timestamp_unix: next_time,
                    });
                }
                let ch = snap.characters.get_mut(&char_id).unwrap();
                ch.home_station_id = new_home_id;
                Ok(ch.home_station_id)
            });
            match res {
                Ok(hid) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnHomeStationChanged",
                        "charid",
                        vec![PyRep::Integer(hid)],
                    );
                    Some(Ok(PyRep::Integer(hid)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // parity: feat:character.home-station
        ("home_station", "get_home_station_candidates") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let list = match ctx.db.get_character(char_id) {
                Ok(ch) => {
                    let mut cands = vec![ch.home_station_id];
                    let school_st = ctx.sde.schools.get(&ch.school_id).map(|s| s.station_id).filter(|&id| id > 0);
                    let hq_st = ctx.db.with_snapshot(|snap| {
                        snap.corporations.get(&ch.corporation_id).and_then(|c| c.hq_station_id).filter(|&id| id > 0)
                    });
                    for st in [school_st, hq_st, session.station_id].into_iter().flatten() {
                        if !cands.contains(&st) {
                            cands.push(st);
                        }
                    }
                    PyRep::List(cands.into_iter().map(PyRep::Integer).collect())
                }
                Err(e) => return Some(Err(BridgeRpcError { code: "CharacterNotFound".into(), message: e.to_string() })),
            };
            Some(Ok(list))
        }

        ("home_station", "get_next_remote_change_time") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let next_time = ctx.db.with_snapshot(|snap| {
                snap.owner_notes.get(&(-10_000_099 - char_id)).and_then(|n| n.note.parse::<i64>().ok()).unwrap_or(0)
            });
            Some(Ok(PyRep::Integer(next_time)))
        }

        ("home_station", "reset_remote_change_time") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_mut_snapshot(|snap| {
                snap.owner_notes.remove(&(-10_000_099 - char_id));
            });
            Some(Ok(PyRep::Integer(0)))
        }

        // parity: feat:character.self-destruct-clone
        ("home_station", "self_destruct_clone") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = session.ship_id.unwrap_or(0);
            match ctx.db.respawn_character_at_home_station(char_id, ship_id, &ctx.sde) {
                Ok((ch, capsule)) => {
                    session.station_id = Some(ch.home_station_id);
                    session.structure_id = None;
                    session.solar_system_id = Some(ch.solar_system_id);
                    session.constellation_id = Some(ch.constellation_id);
                    session.region_id = Some(ch.region_id);
                    session.ship_id = Some(capsule.item_id);
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnCloneDestruction",
                        "charid",
                        vec![],
                    );
                    Some(Ok(PyRep::Integer(ch.home_station_id)))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "RespawnFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        _ => {
            if service == "mailMgr" && method == "SendMail" {
                if let Some(sender_id) = session.character_id {
                    let mut rec_ids = Vec::new();
                    if let Some(first) = args.first() {
                        match first {
                            PyRep::Integer(id) => rec_ids.push(*id),
                            PyRep::List(items) | PyRep::Tuple(items) => {
                                for it in items {
                                    if let Some(id) = it.as_i64() {
                                        rec_ids.push(id);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    for rec_id in rec_ids {
                        if is_blocked_by(ctx, rec_id, sender_id) {
                            return Some(Err(BridgeRpcError {
                                code: "ContactBlocked".into(),
                                message: format!("Recipient {rec_id} has blocked sender {sender_id}"),
                            }));
                        }
                    }
                }
            }
            super::character_gm_tools::handle_char_unbound(ctx, session, service, method, args, _kwargs)
        }
    }
}
