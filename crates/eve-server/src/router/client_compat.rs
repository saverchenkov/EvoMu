use std::collections::HashMap;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};

fn kv_int(entries: &HashMap<String, PyRep>, key: &str, default: i64) -> i64 {
    match entries.get(key) {
        Some(PyRep::Integer(v)) => *v,
        Some(PyRep::Real(v)) => *v as i64,
        Some(PyRep::Bool(v)) => i64::from(*v),
        _ => default,
    }
}

fn kv_str(entries: &HashMap<String, PyRep>, key: &str) -> String {
    match entries.get(key) {
        Some(PyRep::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn inv_keyval_to_dbrow_with_sde(rep: PyRep, sde: Option<&eve_sde::SdeRegistry>) -> PyRep {
    let PyRep::KeyVal(entries) = rep else {
        return rep;
    };
    let type_id = kv_int(&entries, "typeID", 0);
    let sde_type = u32::try_from(type_id)
        .ok()
        .and_then(|tid| sde.and_then(|s| s.types.get(&tid)));
    let raw_group = kv_int(&entries, "groupID", 0);
    let raw_cat = kv_int(&entries, "categoryID", 0);
    let group_id = if raw_group > 0 {
        raw_group
    } else {
        sde_type.map(|t| i64::from(t.group_id)).unwrap_or(0)
    };
    let category_id = if raw_cat > 0 {
        raw_cat
    } else {
        sde_type.map(|t| i64::from(t.category_id)).unwrap_or(0)
    };
    let flag_id = if entries.contains_key("flagID") {
        kv_int(&entries, "flagID", 0)
    } else {
        kv_int(&entries, "flag", 0)
    };
    let singleton = matches!(entries.get("singleton"), Some(PyRep::Bool(true)))
        || matches!(entries.get("singleton"), Some(PyRep::Integer(n)) if *n != 0);
    let raw_qty = if entries.contains_key("quantity") {
        kv_int(&entries, "quantity", 0)
    } else {
        kv_int(&entries, "stacksize", 0)
    };
    let quantity = if singleton && raw_qty >= 0 { -1 } else { raw_qty };
    let cells = vec![
        PyRep::Integer(kv_int(&entries, "itemID", 0)),
        PyRep::Integer(type_id),
        PyRep::Integer(kv_int(&entries, "ownerID", 0)),
        PyRep::Integer(kv_int(&entries, "locationID", 0)),
        PyRep::Integer(flag_id),
        PyRep::Integer(quantity),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        entries
            .get("customInfo")
            .cloned()
            .unwrap_or_else(|| PyRep::String(kv_str(&entries, "customInfo"))),
    ];
    DBRowDescriptor::inv_item_header().build_dbrow(cells)
}

fn ensure_anomaly_dungeon_id(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    if matches!(entries.get("dungeonID"), None | Some(PyRep::None)) {
        entries.insert("dungeonID".to_string(), PyRep::Integer(1001));
    }
    PyRep::KeyVal(entries)
}

pub(crate) fn keyval_to_dict_recursive(rep: PyRep) -> PyRep {
    match rep {
        PyRep::KeyVal(entries) => PyRep::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (PyRep::String(k), keyval_to_dict_recursive(v)))
                .collect(),
        ),
        PyRep::List(items) => {
            PyRep::List(items.into_iter().map(keyval_to_dict_recursive).collect())
        }
        PyRep::Tuple(items) => {
            PyRep::Tuple(items.into_iter().map(keyval_to_dict_recursive).collect())
        }
        PyRep::Dict(entries) => PyRep::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k, keyval_to_dict_recursive(v)))
                .collect(),
        ),
        other => other,
    }
}

/// MachoNet wire-compatibility adjustments for live EVE V24.01 clients.
pub fn normalize_macho_rpc_reply(
    service: &str,
    method: &str,
    rpc_res: Result<PyRep, BridgeRpcError>,
) -> Result<PyRep, BridgeRpcError> {
    normalize_macho_rpc_reply_at(service, method, rpc_res, None)
}

/// MachoNet wire-compatibility adjustments with optional server Unix timestamp for
/// time-dependent normalization (e.g. promoting elapsed industry jobs to `STATUS_READY`).
pub fn normalize_macho_rpc_reply_at(
    service: &str,
    method: &str,
    rpc_res: Result<PyRep, BridgeRpcError>,
    _now_unix: Option<u64>,
) -> Result<PyRep, BridgeRpcError> {
    if service == "dungeonInstanceCacheMgr" && method == "GetSignatureInstances" {
        if let Ok(PyRep::Dict(entries)) = rpc_res {
            let out = entries
                .into_iter()
                .map(|(sys_key, val)| {
                    let count = match val {
                        PyRep::Integer(n) => n,
                        PyRep::List(items) => items.len() as i64,
                        PyRep::Dict(by_dungeon) => by_dungeon
                            .into_iter()
                            .map(|(_, v)| match v {
                                PyRep::List(iids) => iids.len() as i64,
                                PyRep::Integer(n) => n,
                                _ => 1,
                            })
                            .sum(),
                        _ => 0,
                    };
                    (sys_key, PyRep::Integer(count))
                })
                .collect();
            return Ok(PyRep::Dict(out));
        }
    }
    if service == "dungeonInstanceCacheMgr"
        && matches!(
            method,
            "GetCombatAnomalyInstances"
                | "GetOreAnomalyInstances"
                | "GetIceBeltInstances"
                | "GetFactionWarfareInstances"
        )
    {
        if let Ok(PyRep::Dict(entries)) = rpc_res {
            let out = entries
                .into_iter()
                .map(|(sys_key, val)| {
                    let mut list = Vec::new();
                    if let PyRep::Dict(by_dungeon) = val {
                        for (did_key, iids_val) in by_dungeon {
                            let faction_id = match did_key.as_i64().unwrap_or(0) {
                                2894 | 2900 | 2908 | 2911 | 9892 | 9909 => 500_002,
                                2774 | 2789 | 2799 | 2805 | 9578 | 9736 => 500_003,
                                2868 | 2879 | 2893 | 2903 | 9893 | 9911 => 500_004,
                                10485 => 500_011,
                                _ => 500_001,
                            };
                            if let PyRep::List(iids) = iids_val {
                                for iid in iids {
                                    list.push(PyRep::keyval([
                                        ("dungeonID", did_key.clone()),
                                        ("instanceID", iid),
                                        ("solarSystemID", sys_key.clone()),
                                        ("factionID", PyRep::Integer(faction_id)),
                                    ]));
                                }
                            }
                        }
                    }
                    (sys_key, PyRep::List(list))
                })
                .collect();
            return Ok(PyRep::Dict(out));
        }
    }
    if service == "dungeonInstanceCacheMgr" && method == "GetFactionWarfareBattlefieldInstances" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut by_sys: std::collections::BTreeMap<i64, Vec<PyRep>> =
                std::collections::BTreeMap::new();
            for mut item in items {
                let Some(sys_id) = item
                    .dict_get("solarSystemID")
                    .or_else(|| item.dict_get("solarsystemID"))
                    .and_then(|v| v.as_i64())
                    .filter(|&v| v > 0)
                else {
                    continue;
                };
                if let PyRep::KeyVal(ref mut map) = item {
                    map.entry("dungeonID".to_string())
                        .or_insert(PyRep::Integer(10_485));
                    map.entry("dungeonNameID".to_string())
                        .or_insert(PyRep::Integer(0));
                    map.entry("factionID".to_string())
                        .or_insert(PyRep::Integer(500_001));
                }
                by_sys.entry(sys_id).or_default().push(item);
            }
            let out = by_sys
                .into_iter()
                .map(|(sys_id, list)| (PyRep::Integer(sys_id), PyRep::List(list)))
                .collect();
            return Ok(PyRep::Dict(out));
        }
    }
    if service == "dogmaIM" && method == "AddTarget" {
        if let Ok(PyRep::KeyVal(map)) = rpc_res {
            let locked = map
                .get("locked")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let target_ids = if locked {
                match map.get("targetIDs") {
                    Some(PyRep::List(ids)) => ids.clone(),
                    _ => vec![map
                        .get("targetID")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0))],
                }
            } else {
                Vec::new()
            };
            // V24.01 `TargetMgr._LockTarget` (`targetMgr.pyj:1589-1592`):
            //   flag, targetList = self.GetDogmaLM().AddTarget(tid)
            //   if not flag:
            //       self.OnTargetAdded(tid)
            // `flag = 1` (`!locked`) means asynchronous target lock in progress (`targetList = []`),
            // whereas `flag = 0` (`locked`) means already/instantly locked (`targetList = [tid, ...]`).
            return Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(!locked)),
                PyRep::List(target_ids),
            ]));
        }
    } else if service == "agentMgr" && matches!(method, "DoAction" | "GetOffer") {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_agent_do_action_rep(rep));
        }
    }
    rpc_res
}

fn extract_string_field(rep: &PyRep, key: &str) -> Option<String> {
    match rep.dict_get(key) {
        Some(PyRep::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn normalize_agent_do_action_rep(rep: PyRep) -> PyRep {
    // V24.01 `AgentDialogueWindow._GetConversation` unpacks:
    // `((agentSaysMsg, availableActions), lastActionInfo) = agent.DoAction(actionID)`
    // where `agentSaysMsg` is `(text, contentID)`, `availableActions` is `[(actionID, buttonType), ...]`,
    // and `lastActionInfo` is a `dict` (supporting `.get(...)` and subscripting).
    if matches!(rep, PyRep::Tuple(ref items) if items.len() == 2) {
        return rep;
    }
    if !matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
        return rep;
    }
    let content_id = rep
        .dict_get("contentID")
        .or_else(|| rep.dict_get("missionID"))
        .cloned()
        .unwrap_or(PyRep::None);
    let state_str = extract_string_field(&rep, "state").unwrap_or_default();
    let mission_state = rep
        .dict_get("missionState")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let is_completed = state_str == "Completed" || mission_state == 4;
    let is_declined = state_str == "Declined" || mission_state == 5;
    let is_quit = state_str == "Quit" || mission_state == 6;

    let msg_text = extract_string_field(&rep, "agentSays")
        .or_else(|| extract_string_field(&rep, "briefing"))
        .or_else(|| extract_string_field(&rep, "text"))
        .unwrap_or_else(|| {
            if is_completed {
                "Well done! Your mission is complete and your rewards have been credited.".into()
            } else if state_str == "Accepted" || mission_state == 2 {
                "Excellent. Your mission is now active in your journal. Return here once your objectives are complete.".into()
            } else if is_declined || is_quit {
                "Very well. Let me know when you are ready for another assignment.".into()
            } else {
                "Very well, let us proceed.".into()
            }
        });
    let agent_says_msg = PyRep::Tuple(vec![PyRep::String(msg_text), content_id]);

    let available_actions = match rep.dict_get("dialogue") {
        Some(PyRep::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                PyRep::Tuple(pair) if pair.len() >= 2 => {
                    let act_id = pair[0].as_i64()?;
                    let btn_type = pair[1].as_i64().unwrap_or(act_id);
                    Some(PyRep::Tuple(vec![
                        PyRep::Integer(act_id),
                        PyRep::Integer(btn_type),
                    ]))
                }
                PyRep::Integer(act_id) => Some(PyRep::Tuple(vec![
                    PyRep::Integer(*act_id),
                    PyRep::Integer(*act_id),
                ])),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    let lp_val = rep
        .dict_get("loyaltyPoints")
        .or_else(|| rep.dict_get("lpReward"))
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let mut info_entries = vec![
        (PyRep::String("missionCompleted".into()), PyRep::Bool(is_completed)),
        (PyRep::String("missionDeclined".into()), PyRep::Bool(is_declined)),
        (PyRep::String("missionQuit".into()), PyRep::Bool(is_quit)),
        (PyRep::String("missionCantReplay".into()), PyRep::None),
        (PyRep::String("loyaltyPoints".into()), lp_val),
    ];
    let existing_dict = keyval_to_dict_recursive(rep);
    if let PyRep::Dict(kvs) = existing_dict {
        for (k, v) in kvs {
            let dup = matches!(&k, PyRep::String(s) if matches!(
                s.as_str(),
                "missionCompleted" | "missionDeclined" | "missionQuit" | "missionCantReplay" | "loyaltyPoints"
            ));
            if !dup {
                info_entries.push((k, v));
            }
        }
    }

    PyRep::Tuple(vec![
        PyRep::Tuple(vec![agent_says_msg, PyRep::List(available_actions)]),
        PyRep::Dict(info_entries),
    ])
}

fn normalize_damage_message_dict(rep: PyRep) -> PyRep {
    let mut entries: Vec<(PyRep, PyRep)> = match rep {
        PyRep::Dict(e) => e,
        PyRep::KeyVal(m) => m
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), v))
            .collect(),
        other => return other,
    };
    let get_val = |entries: &[(PyRep, PyRep)], key: &str| -> Option<PyRep> {
        entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
    };
    let set_val = |entries: &mut Vec<(PyRep, PyRep)>, key: &str, val: PyRep| {
        if let Some((_, existing)) = entries.iter_mut().find(|(k, _)| k.as_str() == Some(key)) {
            *existing = val;
        } else {
            entries.push((PyRep::String(key.into()), val));
        }
    };
    let raw_attack_type = get_val(&entries, "attackType")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| {
            if get_val(&entries, "target").is_some() {
                "me".to_string()
            } else {
                "otherPlayerWeapons".to_string()
            }
        });
    let attack_type = match raw_attack_type.as_str() {
        "other" | "otherPlayerWeapons" => "otherPlayerWeapons",
        "you" | "me" => "me",
        "otherPlayer" => "otherPlayer",
        _ => "me",
    };
    set_val(&mut entries, "attackType", PyRep::String(attack_type.into()));

    let hit_quality = match get_val(&entries, "hitQuality") {
        Some(PyRep::Integer(n)) => n.clamp(1, 6),
        Some(PyRep::String(s)) => match s.as_str() {
            "Wrecks" => 6,
            "Smashes" => 5,
            "Penetrates" => 4,
            "Hits" => 3,
            "GlancesOff" => 2,
            "Grazes" => 1,
            _ => 3,
        },
        _ => 3,
    };
    set_val(&mut entries, "hitQuality", PyRep::Integer(hit_quality));

    if get_val(&entries, "isBanked").is_none() {
        set_val(&mut entries, "isBanked", PyRep::Bool(false));
    }
    let dmg = get_val(&entries, "damage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    set_val(&mut entries, "damage", PyRep::Real(dmg));

    if matches!(attack_type, "me" | "otherPlayerWeapons") && get_val(&entries, "weapon").is_none() {
        let w = get_val(&entries, "typeID").unwrap_or(PyRep::Integer(0));
        set_val(&mut entries, "weapon", w);
    }
    PyRep::Dict(entries)
}

/// Returns `true` when a server notification uses an internal test-only tuple shape that
/// has no bound MachoNet moniker on the live TCP client wire.
pub fn should_suppress_macho_notification(event: &str, args: &[PyRep]) -> bool {
    if matches!(event, "OnDamageStateChange" | "OnSessionChanged") {
        return true;
    }
    // `TargetMgr._LockTarget` starts the client lock timer from `AddTarget` returning `(1, [])`;
    // suppress internal `"locking"` notices on `:26000` so only lock completion (`OnMultiEvent` `"add"`) fires (`#749`).
    if event == "OnTarget"
        && matches!(
            args,
            [PyRep::Integer(_), PyRep::String(w)]
                | [PyRep::String(w), PyRep::Integer(_), ..]
                if w == "locking"
        )
    {
        return true;
    }
    if event == "OnProgressionRoomJoined"
        && args
            .get(1)
            .and_then(PyRep::as_i64)
            .is_some_and(|id| id >= 1_000_000)
    {
        return true;
    }
    if event == "DoDestinyUpdate" {
        if let Some(first) = args.first() {
            if matches!(first, PyRep::List(items) if items.is_empty()) {
                return true;
            }
        }
    }
    // `combat/mod.rs` emits both `OnDamageMessage` and legacy `OnCombatMessage` with synthetic
    // keys `"AttackHit"`, `"AttackMiss"`, `"AttackedBy"`, `"AttackMissBy"` that do not exist in
    // V24.01 `eveMessages` (`#960`), causing raw dict text on the HUD. Suppress those duplicate
    // synthetic `OnCombatMessage` notices on `:26000` while keeping valid V24 `OnCombatMessage` keys.
    if event == "OnCombatMessage"
        && matches!(
            args.first().and_then(PyRep::as_str),
            Some("AttackHit" | "AttackMiss" | "AttackedBy" | "AttackMissBy")
        )
    {
        return true;
    }
    false
}

/// Normalizes MachoNet broadcast notifications for live EVE V24.01 clients.
pub fn normalize_macho_notification(event: &str, args: Vec<PyRep>) -> Vec<PyRep> {
    normalize_macho_notification_with_sde(event, args, None)
}

/// Normalizes MachoNet broadcast notifications for live EVE V24.01 clients using SDE lookups.
pub fn normalize_macho_notification_with_sde(
    event: &str,
    mut args: Vec<PyRep>,
    sde: Option<&eve_sde::SdeRegistry>,
) -> Vec<PyRep> {
    match event {
        "OnSharedBookmarksFolderUpdated" => {
            if let Some(PyRep::List(entries)) = args.first_mut() {
                for entry in entries.iter_mut() {
                    if let PyRep::Tuple(ref mut parts) = entry {
                        if parts.len() == 3 && !matches!(parts[2], PyRep::Tuple(_)) {
                            let bms: Vec<PyRep> = match &parts[2] {
                                PyRep::List(items) => items
                                    .iter()
                                    .map(|it| {
                                        if it.dict_get("bookmarkID").is_some() {
                                            it.clone()
                                        } else {
                                            PyRep::keyval([("bookmarkID", it.clone())])
                                        }
                                    })
                                    .collect(),
                                _ => Vec::new(),
                            };
                            parts[2] =
                                PyRep::Tuple(vec![PyRep::List(bms), PyRep::List(Vec::new())]);
                        }
                    }
                }
            }
            args
        }
        "OnTargetClear" => {
            if args.is_empty() {
                vec![PyRep::List(Vec::new())]
            } else {
                args
            }
        }
        "OnRemoteMessage" => {
            if args.len() == 2 {
                args.push(PyRep::None);
            }
            args
        }
        "ProcessInventoryChange" => {
            if args.len() == 3 && !matches!(args[0], PyRep::List(_)) {
                let item_id = args[0].clone();
                let loc_id = args[1].clone();
                let flag_id = args[2].clone();
                let item_kv = PyRep::keyval([
                    ("itemID", item_id),
                    ("typeID", PyRep::Integer(0)),
                    ("ownerID", PyRep::None),
                    ("locationID", loc_id.clone()),
                    ("flagID", flag_id.clone()),
                    ("categoryID", PyRep::Integer(0)),
                    ("groupID", PyRep::Integer(0)),
                    ("singleton", PyRep::Integer(1)),
                    ("quantity", PyRep::Integer(1)),
                ]);
                let change_dict = PyRep::Dict(vec![
                    (
                        PyRep::Integer(3),
                        PyRep::Tuple(vec![loc_id.clone(), loc_id]),
                    ),
                    (
                        PyRep::Integer(4),
                        PyRep::Tuple(vec![flag_id.clone(), flag_id]),
                    ),
                ]);
                vec![
                    PyRep::List(vec![item_kv]),
                    change_dict,
                    PyRep::Bool(true),
                    PyRep::None,
                ]
            } else {
                args
            }
        }
        "OnTarget" => {
            if args.len() == 2 {
                let (what, tid) = match (&args[0], &args[1]) {
                    (PyRep::Integer(id), PyRep::String(w)) => {
                        // Never rewrite `"locking"` to `"add"`: doing so triggers `TargetMgr.OnTargetAdded(tid)`
                        // immediately before `remaining_lock_ms` elapses (`#749`).
                        let mapped = match w.as_str() {
                            "locked" | "add" | "added" => "add",
                            "targetedBy" | "otheradd" => "otheradd",
                            "unlock" | "unlocked" | "lost" => "lost",
                            "clear" => "clear",
                            other => other,
                        };
                        (PyRep::String(mapped.to_string()), PyRep::Integer(*id))
                    }
                    (w, id) => (w.clone(), id.clone()),
                };
                vec![what, tid, PyRep::None]
            } else {
                args
            }
        }
        "OnWarpFinished" => {
            if args.len() > 2 {
                let first = args.into_iter().next().unwrap_or(PyRep::Integer(0));
                vec![first, PyRep::Integer(0)]
            } else {
                args
            }
        }
        "OnServerSkillsChanged" => {
            if args
                .get(1)
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.starts_with("On"))
            {
                args[1] = PyRep::None;
            }
            args
        }
        "DoDestinyUpdates" => args
            .into_iter()
            .map(|arg| match arg {
                PyRep::List(entries) => PyRep::List(
                    entries
                        .into_iter()
                        .map(|entry| match &entry {
                            PyRep::Tuple(pair)
                                if pair.len() == 2
                                    && matches!(pair.first(), Some(PyRep::List(_)))
                                    && matches!(pair.get(1), Some(PyRep::Bool(_))) =>
                            {
                                entry
                            }
                            _ => PyRep::Tuple(vec![PyRep::List(vec![entry]), PyRep::Bool(false)]),
                        })
                        .collect(),
                ),
                other => other,
            })
            .collect(),
        "OnSpecialFX" => normalize_on_special_fx(args),
        "OnGodmaShipEffect" => normalize_on_godma_ship_effect(args, sde),
        "OnDamageMessage" => {
            // V24.01 `Logger.OnDamageMessage(self, damageMessagesArgs)` (`loggerSvc.pyj:322`)
            // takes 1 argument (`damageMessagesArgs: dict`) and passes it to
            // `AddCombatMessageFromDict(damageMessagesArgs)` (`#952`).
            let raw_dict = if args.len() >= 2 {
                args.swap_remove(1)
            } else if !args.is_empty() {
                args.swap_remove(0)
            } else {
                PyRep::Dict(Vec::new())
            };
            vec![normalize_damage_message_dict(raw_dict)]
        }
        "OnDamageMessages" => {
            // V24.01 `Logger.OnDamageMessages(self, dmgmsgs)` (`loggerSvc.pyj:326`) iterates
            // `for msg in dmgmsgs: self.OnDamageMessage(*msg[1:])`, expecting each element in
            // `dmgmsgs` to be a 2-tuple `(msg_key, damage_dict)` (`#952`).
            if let Some(PyRep::List(items)) = args.first_mut() {
                for item in items.iter_mut() {
                    let old = std::mem::replace(item, PyRep::None);
                    *item = match old {
                        PyRep::Tuple(mut parts) if parts.len() >= 2 => {
                            let d = std::mem::replace(&mut parts[1], PyRep::None);
                            parts[1] = normalize_damage_message_dict(d);
                            PyRep::Tuple(parts)
                        }
                        other @ (PyRep::Dict(_) | PyRep::KeyVal(_)) => PyRep::Tuple(vec![
                            PyRep::String("AttackHit".into()),
                            normalize_damage_message_dict(other),
                        ]),
                        other => other,
                    };
                }
            }
            args
        }
        "OnGodmaPrimeItem" => {
            let first_arg = args.first().cloned().unwrap_or(PyRep::Integer(0));
            if let Some(PyRep::KeyVal(entries)) = args.get_mut(1) {
                if !entries.contains_key("wallclockTime") {
                    let wt = entries
                        .get("time")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    entries.insert("wallclockTime".to_string(), wt);
                }
                if matches!(entries.get("invItem"), Some(PyRep::KeyVal(_))) {
                    if let Some(old_inv) = entries.remove("invItem") {
                        entries.insert(
                            "invItem".to_string(),
                            inv_keyval_to_dbrow_with_sde(old_inv, sde),
                        );
                    }
                } else if matches!(entries.get("invItem"), None | Some(PyRep::None)) {
                    let mut fallback = HashMap::new();
                    match entries.get("itemID") {
                        Some(PyRep::Tuple(t)) if t.len() == 3 => {
                            fallback.insert("itemID".to_string(), t[0].clone());
                            fallback.insert("locationID".to_string(), t[0].clone());
                            fallback.insert("flagID".to_string(), t[1].clone());
                            fallback.insert("typeID".to_string(), t[2].clone());
                            fallback.insert("singleton".to_string(), PyRep::Bool(false));
                            fallback.insert("quantity".to_string(), PyRep::Integer(1));
                        }
                        Some(id_rep) => {
                            fallback.insert("itemID".to_string(), id_rep.clone());
                            fallback.insert(
                                "locationID".to_string(),
                                entries
                                    .get("locationID")
                                    .cloned()
                                    .unwrap_or_else(|| first_arg.clone()),
                            );
                            fallback.insert(
                                "typeID".to_string(),
                                entries.get("typeID").cloned().unwrap_or(PyRep::Integer(0)),
                            );
                            fallback.insert("singleton".to_string(), PyRep::Bool(true));
                            fallback.insert("quantity".to_string(), PyRep::Integer(-1));
                        }
                        None => {}
                    }
                    entries.insert(
                        "invItem".to_string(),
                        inv_keyval_to_dbrow_with_sde(PyRep::KeyVal(fallback), sde),
                    );
                }
                let is_subloc = matches!(
                    entries.get("itemID"),
                    Some(PyRep::Tuple(t)) if t.len() == 3
                );
                if is_subloc {
                    if let Some(PyRep::Dict(attrs)) = entries.get_mut("attributes") {
                        let has_805 = attrs.iter().any(|(k, _)| k.as_i64() == Some(805));
                        if !has_805 {
                            let qty_val = attrs
                                .iter()
                                .find_map(|(k, v)| (k.as_i64() == Some(80)).then(|| v.clone()))
                                .unwrap_or(PyRep::Real(1.0));
                            attrs.retain(|(k, _)| k.as_i64() != Some(80));
                            attrs.push((PyRep::Integer(805), qty_val));
                        }
                    }
                }
            }
            args
        }
        "OnItemChange" => {
            if let Some(first) = args.first_mut() {
                let old = std::mem::replace(first, PyRep::None);
                *first = inv_keyval_to_dbrow_with_sde(old, sde);
            }
            if args.len() == 2 {
                args.push(PyRep::None);
            }
            args
        }
        "OnMultipleItemChange" => {
            if let Some(PyRep::List(items)) = args.first_mut() {
                for entry in items.iter_mut() {
                    if let PyRep::Tuple(pair) = entry {
                        if let Some(first) = pair.first_mut() {
                            let old = std::mem::replace(first, PyRep::None);
                            *first = inv_keyval_to_dbrow_with_sde(old, sde);
                        }
                    } else {
                        let old = std::mem::replace(entry, PyRep::None);
                        *entry = inv_keyval_to_dbrow_with_sde(old, sde);
                    }
                }
            }
            args
        }
        "OnSystemScanStarted" => {
            if let Some(PyRep::List(probe_ids)) = args.get_mut(2) {
                let dict = probe_ids
                    .iter()
                    .cloned()
                    .map(|pid| {
                        (
                            pid,
                            PyRep::keyval([
                                (
                                    "pos",
                                    PyRep::Tuple(vec![
                                        PyRep::Real(0.0),
                                        PyRep::Real(0.0),
                                        PyRep::Real(0.0),
                                    ]),
                                ),
                                ("state", PyRep::Integer(4)),
                            ]),
                        )
                    })
                    .collect();
                args[2] = PyRep::Dict(dict);
            }
            args
        }
        "OnSignalTrackerAnomalyUpdate" => {
            if let Some(PyRep::Dict(entries)) = args.get_mut(1) {
                let updated = std::mem::take(entries)
                    .into_iter()
                    .map(|(k, v)| (k, ensure_anomaly_dungeon_id(v)))
                    .collect();
                *entries = updated;
            }
            args
        }
        "OnModuleAttributeChanges" => {
            let raw_items = if args.len() == 1 {
                match args.into_iter().next().unwrap() {
                    PyRep::List(items) => items,
                    PyRep::Tuple(items)
                        if items.is_empty()
                            || (items.len() != 7 && items.len() != 8)
                            || items
                                .iter()
                                .any(|el| matches!(el, PyRep::Tuple(_) | PyRep::List(_))) =>
                    {
                        items
                    }
                    other => vec![other],
                }
            } else {
                args
            };
            let norm_items: Vec<PyRep> = raw_items
                .into_iter()
                .map(|el| match el {
                    PyRep::Tuple(mut t) | PyRep::List(mut t) if t.len() == 8 => {
                        if t.get(3) == Some(&PyRep::Integer(0)) {
                            t[3] = PyRep::Integer(2);
                        }
                        if matches!(t.get(7), Some(PyRep::Bool(_))) {
                            t[7] = PyRep::None;
                        }
                        PyRep::Tuple(t)
                    }
                    PyRep::Tuple(mut t) | PyRep::List(mut t) if t.len() == 7 => {
                        if t.get(3) == Some(&PyRep::Integer(0)) {
                            t[3] = PyRep::Integer(2);
                        }
                        t.push(PyRep::None);
                        PyRep::Tuple(t)
                    }
                    other => PyRep::Tuple(vec![
                        PyRep::String("OnModuleAttributeChange".into()),
                        PyRep::None,
                        other,
                        PyRep::Integer(2),
                        PyRep::Integer(0),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                        PyRep::None,
                    ]),
                })
                .collect();
            vec![PyRep::List(norm_items)]
        }
        _ => args,
    }
}

/// Checks whether a session's `dogmaIM` moniker may also bind a station, Upwell structure,
/// or character home station in the current solar system.
pub fn is_dogma_station_or_structure_accessible(
    ctx: &crate::router::ServerContext,
    session: &eve_machonet::SessionState,
    char_id: i64,
    object_id: i64,
) -> bool {
    ctx.sde
        .stations
        .get(&object_id)
        .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
        || ctx.db.with_snapshot(|snap| {
            snap.structures
                .get(&object_id)
                .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
                || snap
                    .characters
                    .get(&char_id)
                    .is_some_and(|c| c.home_station_id == object_id)
        })
}

fn ensure_filetime_pyrep(rep: &mut PyRep, fallback_ft: i64) {
    let is_long_ft = rep
        .as_i64()
        .is_some_and(|v| v > 0 && v.abs() > i32::MAX as i64);
    if !is_long_ft {
        *rep = PyRep::Integer(fallback_ft);
    }
}

fn resolve_effect_id_pyrep(rep: PyRep, sde: Option<&eve_sde::SdeRegistry>) -> PyRep {
    match rep {
        PyRep::Integer(v) => PyRep::Integer(v),
        PyRep::String(name) | PyRep::Token(name) => {
            if let Ok(parsed) = name.parse::<i64>() {
                return PyRep::Integer(parsed);
            }
            if let Some(sde) = sde {
                if let Some(eff) = sde.effects.values().find(|e| e.name == name) {
                    return PyRep::Integer(eff.effect_id as i64);
                }
            }
            let fallback = match name.as_str() {
                "online" => 16,
                "miningLaser" => 67,
                "miningClouds" => 2726,
                "mining" => 17,
                "targetAttack" => 10,
                "projectileFired" => 34,
                "useMissiles" => 101,
                "loPower" => 11,
                "hiPower" => 12,
                "medPower" => 13,
                _ => super::binding_effects::fallback_superweapon_effect_id(0, Some(name.as_str()))
                    .unwrap_or(0),
            };
            PyRep::Integer(fallback)
        }
        other => other,
    }
}

fn normalize_godma_environment(
    env: PyRep,
    item_id: &PyRep,
    effect_id: &PyRep,
    sde: Option<&eve_sde::SdeRegistry>,
) -> PyRep {
    match env {
        PyRep::Tuple(mut elems) => {
            if elems.len() == 3 {
                let eff = resolve_effect_id_pyrep(elems[2].clone(), sde);
                return PyRep::Tuple(vec![
                    elems[0].clone(),
                    PyRep::None,
                    elems[1].clone(),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    eff,
                ]);
            }
            while elems.len() < 7 {
                match elems.len() {
                    5 => elems.push(PyRep::List(Vec::new())),
                    6 => elems.push(effect_id.clone()),
                    _ => elems.push(PyRep::None),
                }
            }
            if let Some(last) = elems.get_mut(6) {
                if matches!(last, PyRep::String(_) | PyRep::Token(_)) {
                    *last = effect_id.clone();
                }
            }
            PyRep::Tuple(elems)
        }
        _ => PyRep::Tuple(vec![
            item_id.clone(),
            PyRep::None,
            item_id.clone(),
            PyRep::None,
            PyRep::None,
            PyRep::List(Vec::new()),
            effect_id.clone(),
        ]),
    }
}

fn ensure_i4_pyrep(rep: &mut PyRep) {
    match rep {
        PyRep::Real(v) if v.is_finite() => {
            *rep = PyRep::Integer((v.round() as i64).clamp(i32::MIN as i64, i32::MAX as i64));
        }
        PyRep::Integer(v) => {
            *rep = PyRep::Integer((*v).clamp(i32::MIN as i64, i32::MAX as i64));
        }
        PyRep::None => {}
        _ => *rep = PyRep::Integer(0),
    }
}

fn ensure_real_duration_pyrep(rep: &mut PyRep) {
    match rep {
        PyRep::Real(v) if v.is_finite() => {
            *rep = PyRep::Real((*v).max(0.0));
        }
        PyRep::Integer(v) => {
            *rep = PyRep::Real((*v).max(0) as f64);
        }
        _ => *rep = PyRep::Real(0.0),
    }
}

/// Normalizes `OnSpecialFX` MachoNet broadcast notifications to the V24.01 14-positional-arg
/// signature (`shipID, moduleID, moduleTypeID, targetID, otherTypeID, guid, isOffensive,
/// start, active, duration, repeat, startTime, timeFromStart, graphicInfo`).
fn normalize_on_special_fx(mut args: Vec<PyRep>) -> Vec<PyRep> {
    if args.len() == 1 {
        match args.into_iter().next().unwrap() {
            PyRep::List(mut items) if items.len() == 1 => match items.remove(0) {
                PyRep::Tuple(inner) | PyRep::List(inner) => args = inner,
                other => args = vec![other],
            },
            PyRep::Tuple(inner) => args = inner,
            other => args = vec![other],
        }
    }
    if args.is_empty() {
        return Vec::new();
    }
    let (secs, nanos) = eve_db::clock::host_wall_unix_secs_and_nanos();
    let fallback_ft = (secs as i64 + 11_644_473_600) * 10_000_000 + (nanos as i64 / 100);
    while args.len() < 14 {
        match args.len() {
            5 => args.push(PyRep::String("effects.Laser".into())),
            6 => args.push(PyRep::Bool(false)),
            7 | 8 => args.push(PyRep::Integer(1)),
            9 => args.push(PyRep::Real(-1.0)),
            10 => args.push(PyRep::Integer(0)),
            11 => args.push(PyRep::Integer(fallback_ft)),
            12 => args.push(PyRep::Real(0.0)),
            _ => args.push(PyRep::None),
        }
    }
    args.truncate(14);
    ensure_filetime_pyrep(&mut args[11], fallback_ft);
    args
}

fn normalize_on_godma_ship_effect(
    mut args: Vec<PyRep>,
    sde: Option<&eve_sde::SdeRegistry>,
) -> Vec<PyRep> {
    if args.is_empty() {
        return Vec::new();
    }
    let (secs, nanos) = eve_db::clock::host_wall_unix_secs_and_nanos();
    let fallback_ft = (secs as i64 + 11_644_473_600) * 10_000_000 + (nanos as i64 / 100);
    if args.len() >= 10 {
        let item_id = args[0].clone();
        let effect_id = resolve_effect_id_pyrep(args[1].clone(), sde);
        args[1] = effect_id.clone();
        ensure_filetime_pyrep(&mut args[2], fallback_ft);
        let env = std::mem::replace(&mut args[5], PyRep::None);
        args[5] = normalize_godma_environment(env, &item_id, &effect_id, sde);
        ensure_filetime_pyrep(&mut args[6], fallback_ft);
        ensure_real_duration_pyrep(&mut args[7]);
        ensure_i4_pyrep(&mut args[8]);
        if args.len() == 10 {
            args.push(PyRep::None);
        }
        return args;
    }
    let item_id = args[0].clone();
    let effect_id = resolve_effect_id_pyrep(
        args.get(1).cloned().unwrap_or(PyRep::Integer(0)),
        sde,
    );
    let active_flag = PyRep::Integer(if args.get(2).and_then(PyRep::as_bool).unwrap_or(false) {
        1
    } else {
        0
    });
    let env = normalize_godma_environment(PyRep::None, &item_id, &effect_id, sde);
    vec![
        item_id,
        effect_id,
        PyRep::Integer(fallback_ft),
        active_flag.clone(),
        active_flag,
        env,
        PyRep::Integer(fallback_ft),
        PyRep::Real(0.0),
        PyRep::Integer(0),
        PyRep::None,
        PyRep::None,
    ]
}

#[cfg(test)]
#[path = "client_compat_cov_unit_tests.rs"]
mod client_compat_cov_unit_tests;

#[cfg(test)]
#[path = "client_compat_extra_tests.rs"]
mod client_compat_extra_tests;
