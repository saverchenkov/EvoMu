//! `quasar_grpc` — Native V24.01 Quasar Public Gateway HTTP/2 (`h2c`) gRPC transport.
//!
//! Serves `eve_public/gateway.proto` directly over plaintext HTTP/2 prior-knowledge (`h2c`),
//! matching `publicGateway/grpc/connectionConfig.pyj` when the EVE client is launched with
//! `EVE_PUBLIC_GATEWAY_ADDRESS=<host>:26005` (`secure=False` in `_grpc_public_gateway.pyd` /
//! `eve_public_grpc_client.so`):
//! - `/eve_public.gateway.Events/Ping` (`Empty -> Empty`)
//! - `/eve_public.gateway.Events/Publish` (`stream eve_public.Event -> stream Empty`)
//! - `/eve_public.gateway.Notices/Ping` (`Empty -> Empty`)
//! - `/eve_public.gateway.Notices/Consume` (`stream Empty -> stream eve_public.Notice`)
//! - `/eve_public.gateway.Requests/Ping` (`Empty -> Empty`)
//! - `/eve_public.gateway.Requests/Send` (`stream eve_public.Request -> stream eve_public.Response`)
//!
//! Authentication accepts both `eve-db` OAuth2 SSO HS256 JWTs (`authorization: Bearer <jwt>`) and
//! the stock V24.01 client's local-dev token (`jwt.fabricate_local_dev_user_token(tenant, userid)`
//! in `publicGateway/publicGatewaySvc.pyj:284`, signed with HS256 key `"dev"` and `sub =
//! "USER:EVE:<userid>"`), enforcing account ban/suspension checks and strict character ownership.

#![forbid(unsafe_code)]

use eve_proto::{BridgeRpcError, PyRep};

fn row_field(item: &PyRep, key: &str) -> Option<PyRep> {
    match item {
        PyRep::DBRow { columns, .. } => columns.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()),
        _ => item.dict_get(key).cloned(),
    }
}

fn extract_table_rows(rep: &PyRep) -> Vec<PyRep> {
    match rep {
        PyRep::List(items) => items.clone(),
        PyRep::Rowset { columns, rows } => rows
            .iter()
            .map(|r| {
                let mut pairs = Vec::with_capacity(columns.len());
                for (i, c) in columns.iter().enumerate() {
                    if let Some(v) = r.get(i) {
                        pairs.push((c.as_str(), v.clone()));
                    }
                }
                PyRep::keyval(pairs)
            })
            .collect(),
        PyRep::ObjectEx { list, .. } => list.clone(),
        _ => Vec::new(),
    }
}

/// Normalizes `machoNet` RPC replies where the live V24.01 client unpacks a tuple shape.
pub fn normalize_macho_rpc_reply(
    service: &str,
    method: &str,
    rpc_res: Result<PyRep, BridgeRpcError>,
) -> Result<PyRep, BridgeRpcError> {
    if service == "structureDirectory" && method == "GetJumpBridgesWithMyAccess" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut pairs = Vec::with_capacity(items.len());
            let mut accessible_ids = Vec::with_capacity(items.len());
            for item in items {
                let sid = item
                    .dict_get("structureID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let sys_a = item
                    .dict_get("solarSystemID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let linked_id = item
                    .dict_get("linkedStructureID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let sys_b = item
                    .dict_get("destinationSolarSystemID")
                    .or_else(|| item.dict_get("linkedSolarSystemID"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                accessible_ids.push(sid.clone());
                pairs.push(PyRep::Tuple(vec![
                    PyRep::keyval([("structureID", sid), ("solarSystemID", sys_a)]),
                    PyRep::keyval([("structureID", linked_id), ("solarSystemID", sys_b)]),
                ]));
            }
            return Ok(PyRep::Tuple(vec![
                PyRep::List(pairs),
                PyRep::List(accessible_ids),
                PyRep::Dict(Vec::new()),
            ]));
        }
    } else if service == "jumpTimers" && method == "GetTimers" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let jump_activation = rep
                    .dict_get("jumpActivation")
                    .cloned()
                    .unwrap_or(PyRep::None);
                let jump_fatigue = rep
                    .dict_get("jumpFatigue")
                    .cloned()
                    .unwrap_or(PyRep::None);
                let last_updated = rep
                    .dict_get("lastUpdated")
                    .cloned()
                    .unwrap_or(PyRep::None);
                return Ok(PyRep::Tuple(vec![
                    jump_activation,
                    jump_fatigue,
                    last_updated,
                ]));
            }
        }
    } else if service == "XmppChatMgr" && method == "ResyncSystemChannelAccess" {
        if let Ok(ref rep) = rpc_res {
            if let Some(chans) = rep.dict_get("channels") {
                return Ok(chans.clone());
            }
        }
    } else if service == "beyonce" && method == "GetCapacityOfSiphon" {
        if let Ok(PyRep::Real(frac)) = rpc_res {
            return Ok(PyRep::keyval([
                ("capacity", PyRep::Real(1_000.0)),
                ("used", PyRep::Real(frac * 1_000.0)),
            ]));
        }
    } else if service == "blueprintManager" && method == "GetBlueprintDataByOwner" {
        if let Ok(PyRep::List(bps)) = rpc_res {
            let mut seen_locs = std::collections::HashSet::new();
            let mut locs = Vec::new();
            for bp in &bps {
                if let Some(loc_id) = bp.dict_get("locationID").and_then(PyRep::as_i64) {
                    if seen_locs.insert(loc_id) {
                        locs.push((PyRep::Integer(loc_id), PyRep::Integer(loc_id)));
                    }
                }
            }
            return Ok(PyRep::Tuple(vec![PyRep::List(bps), PyRep::Dict(locs)]));
        }
    } else if service == "charMgr" && method == "GetOwnerNoteLabels" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for (idx, it) in items.into_iter().enumerate() {
                match it {
                    PyRep::String(s) => {
                        let lbl = if s.chars().count() < 2 {
                            "N:General".to_string()
                        } else {
                            s
                        };
                        out.push(PyRep::keyval([
                            ("noteID", PyRep::Integer((idx + 1) as i64)),
                            ("label", PyRep::String(lbl)),
                        ]));
                    }
                    other => out.push(other),
                }
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "charMgr"
        && (method == "GetRecentShipKillsAndLosses" || method == "GetRecentShipKills")
    {
        if let Ok(ref rep) = rpc_res {
            let items = extract_table_rows(rep);
            let rows = items
                .into_iter()
                .map(|it| {
                    let kid = row_field(&it, "killID").unwrap_or(PyRep::Integer(0));
                    let vid = row_field(&it, "victimID")
                        .or_else(|| row_field(&it, "victimCharacterID"))
                        .unwrap_or(PyRep::None);
                    let sys = row_field(&it, "solarSystemID").unwrap_or(PyRep::None);
                    let kt = row_field(&it, "killTime").unwrap_or(PyRep::Integer(0));
                    let vc = row_field(&it, "victimCharacterID").unwrap_or_else(|| vid.clone());
                    let fc = row_field(&it, "finalCharacterID")
                        .or_else(|| row_field(&it, "attackerID"))
                        .unwrap_or(PyRep::None);
                    vec![kid, vid, sys, kt, vc, fc]
                })
                .collect();
            return Ok(PyRep::rowset(
                vec![
                    "killID",
                    "victimID",
                    "solarSystemID",
                    "killTime",
                    "victimCharacterID",
                    "finalCharacterID",
                ],
                rows,
            ));
        }
    } else if service == "charUnboundMgr" && method == "GetCharacterInfo" {
        if let Ok(rep) = rpc_res {
            if let PyRep::KeyVal(mut map) = rep {
                let cid = map
                    .get("characterID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let cname = map
                    .get("characterName")
                    .or_else(|| map.get("name"))
                    .cloned()
                    .unwrap_or_else(|| PyRep::String(String::new()));
                map.insert("characterID".into(), cid);
                map.insert("characterName".into(), cname);
                return Ok(PyRep::List(vec![PyRep::KeyVal(map)]));
            }
            return Ok(rep);
        }
    } else if service == "contractProxy" && method == "GetContractListForOwner" {
        if let Ok(PyRep::List(contracts)) = rpc_res {
            return Ok(PyRep::keyval([
                ("contracts", PyRep::List(contracts)),
                ("items", PyRep::Dict(Vec::new())),
            ]));
        }
    } else if service == "contractProxy" && method == "NumOutstandingContracts" {
        if let Ok(PyRep::Integer(cnt)) = rpc_res {
            return Ok(PyRep::keyval([
                ("myCharTotal", PyRep::Integer(cnt)),
                ("myCorpTotal", PyRep::Integer(0)),
                ("nonCorpForMyChar", PyRep::Integer(cnt)),
                ("nonCorpForMyCorp", PyRep::Integer(0)),
            ]));
        }
    } else if service == "contractProxy" && method == "SearchContracts" {
        if let Ok(PyRep::List(raw)) = rpc_res {
            let mut wrapped = Vec::new();
            for item in raw {
                if item.dict_get("contract").is_some() && item.dict_get("bids").is_some() {
                    wrapped.push(item);
                } else {
                    let bids = item
                        .dict_get("bids")
                        .cloned()
                        .unwrap_or_else(|| PyRep::List(Vec::new()));
                    wrapped.push(PyRep::keyval([("contract", item), ("bids", bids)]));
                }
            }
            let n = wrapped.len() as i64;
            return Ok(PyRep::keyval([
                ("contracts", PyRep::List(wrapped)),
                ("numFound", PyRep::Integer(n)),
                ("maxResults", PyRep::Integer(100)),
                ("searchTime", PyRep::Integer(1)),
            ]));
        }
    } else if service == "corpRegistry" && method == "CanLeaveCurrentCorporation" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let can_leave = rep
                    .dict_get("canLeave")
                    .cloned()
                    .unwrap_or(PyRep::Bool(false));
                let err_kv = PyRep::keyval([
                    ("msg", PyRep::String("CrpCantQuitNotInStasis".into())),
                    ("dict", PyRep::Dict(Vec::new())),
                    ("__class__", PyRep::String("UserError".into())),
                ]);
                return Ok(PyRep::Tuple(vec![
                    can_leave,
                    err_kv,
                    PyRep::Dict(Vec::new()),
                ]));
            }
        }
    } else if service == "corpRegistry" && method == "GetStructureReinforceDefault" {
        if let Ok(PyRep::Integer(h)) = rpc_res {
            return Ok(PyRep::Tuple(vec![PyRep::Integer(h), PyRep::Integer(0)]));
        }
    } else if service == "corpmgr" && method == "AuditMember" {
        if let Ok(rep) = rpc_res {
            return Ok(PyRep::Tuple(vec![rep.clone(), rep]));
        }
    } else if service == "corporationSvc"
        && method == "GetEmployementRecordAndCharacterTransfers"
    {
        if let Ok(rep) = rpc_res {
            if matches!(rep, PyRep::Tuple(_)) {
                return Ok(rep);
            }
            return Ok(PyRep::Tuple(vec![rep, PyRep::List(Vec::new())]));
        }
    } else if service == "devIndexManager" && method == "GetAllDevelopmentIndices" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for it in items {
                let sys_id = it
                    .dict_get("solarSystemID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let mil = it
                    .dict_get("militaryPoints")
                    .or_else(|| it.dict_get("military"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let ind = it
                    .dict_get("industrialPoints")
                    .or_else(|| it.dict_get("industrial"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let claimed = it
                    .dict_get("claimedFor")
                    .or_else(|| it.dict_get("strategic"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let adm = it.dict_get("adm").cloned().unwrap_or(PyRep::Real(0.0));
                out.push(PyRep::keyval([
                    ("solarSystemID", sys_id),
                    ("militaryPoints", mil),
                    ("industrialPoints", ind),
                    ("claimedFor", claimed),
                    ("adm", adm),
                ]));
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "dogmaIM" && method == "OverloadRack" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let rack = rep.dict_get("rackIndex").cloned().unwrap_or(PyRep::Integer(0));
                let mids = match rep.dict_get("moduleIDs") {
                    Some(PyRep::List(list)) => list.clone(),
                    _ => Vec::new(),
                };
                return Ok(PyRep::List(
                    mids.into_iter()
                        .map(|mid| PyRep::Tuple(vec![mid, rack.clone()]))
                        .collect(),
                ));
            }
        }
    } else if service == "fwVictoryPointMgr" && method == "GetLocalVictoryPointState" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::None) {
                return Ok(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)]));
            }
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let sys_id = rep.dict_get("solarSystemID").cloned().unwrap_or(PyRep::None);
                let state = rep.dict_get("state").cloned().unwrap_or(PyRep::Integer(0));
                return Ok(PyRep::Tuple(vec![sys_id, state]));
            }
        }
    } else if service == "home_station" && method == "get_home_station_candidates" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for it in items {
                match it {
                    PyRep::Integer(sid) => {
                        out.push(PyRep::keyval([
                            ("id", PyRep::Integer(sid)),
                            ("station_id", PyRep::Integer(sid)),
                            ("type_id", PyRep::Integer(1529)),
                            ("solar_system_id", PyRep::Integer(0)),
                            ("is_current_station", PyRep::Bool(true)),
                            ("is_school_hq", PyRep::Bool(true)),
                            ("errors", PyRep::List(Vec::new())),
                        ]));
                    }
                    other => out.push(other),
                }
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "invbroker" && method == "ALSCLogGet" {
        if let Ok(ref rep) = rpc_res {
            if let Some(entries) = rep.dict_get("entries") {
                return Ok(entries.clone());
            }
        }
    } else if service == "invbroker" && method == "TrashItems" {
        if let Ok(rep) = rpc_res {
            return match rep {
                PyRep::None => Ok(PyRep::List(Vec::new())),
                PyRep::KeyVal(_) | PyRep::Dict(_) => Ok(PyRep::List(vec![rep])),
                other => Ok(other),
            };
        }
    } else if service == "jumpCloneSvc" && method == "GetCloneState" {
        if let Ok(rep) = rpc_res {
            let clone_items = rep
                .dict_get("clones")
                .map(extract_table_rows)
                .unwrap_or_default();
            let clone_rows = clone_items
                .into_iter()
                .map(|c| {
                    vec![
                        row_field(&c, "jumpCloneID").unwrap_or(PyRep::Integer(0)),
                        row_field(&c, "locationID").unwrap_or(PyRep::None),
                        row_field(&c, "cloneName").unwrap_or_else(|| PyRep::String(String::new())),
                    ]
                })
                .collect();
            let imp_items = rep
                .dict_get("implants")
                .map(extract_table_rows)
                .unwrap_or_default();
            let imp_rows = imp_items
                .into_iter()
                .map(|im| {
                    vec![
                        row_field(&im, "jumpCloneID").unwrap_or(PyRep::Integer(0)),
                        row_field(&im, "implantID").unwrap_or(PyRep::Integer(0)),
                        row_field(&im, "typeID").unwrap_or(PyRep::Integer(0)),
                    ]
                })
                .collect();
            let time_last = rep.dict_get("timeLastJump").cloned().unwrap_or(PyRep::None);
            return Ok(PyRep::keyval([
                (
                    "clones",
                    PyRep::rowset(vec!["jumpCloneID", "locationID", "cloneName"], clone_rows),
                ),
                (
                    "implants",
                    PyRep::rowset(vec!["jumpCloneID", "implantID", "typeID"], imp_rows),
                ),
                ("timeLastJump", time_last),
            ]));
        }
    } else if service == "map" && method == "GetConstellationLPData" {
        if let Ok(ref rep) = rpc_res {
            let rows = extract_table_rows(rep)
                .into_iter()
                .map(|r| {
                    let sys = row_field(&r, "solarSystemID").unwrap_or(PyRep::Integer(0));
                    let factor = row_field(&r, "lpFactor")
                        .or_else(|| row_field(&r, "points").map(|p| PyRep::Real(p.as_f64().unwrap_or(0.0))))
                        .unwrap_or(PyRep::Real(0.0));
                    vec![sys, factor]
                })
                .collect();
            return Ok(PyRep::rowset(vec!["solarSystemID", "lpFactor"], rows));
        }
    } else if service == "map" && method == "GetCurrentSovData" {
        if let Ok(ref rep) = rpc_res {
            let rows = extract_table_rows(rep)
                .into_iter()
                .map(|r| {
                    let loc = row_field(&r, "locationID")
                        .or_else(|| row_field(&r, "solarSystemID"))
                        .unwrap_or(PyRep::Integer(0));
                    vec![
                        loc.clone(),
                        row_field(&r, "solarSystemID").unwrap_or(loc),
                        row_field(&r, "allianceID").unwrap_or(PyRep::None),
                        row_field(&r, "corporationID").unwrap_or(PyRep::None),
                        row_field(&r, "claimStructureID").unwrap_or(PyRep::None),
                    ]
                })
                .collect();
            return Ok(PyRep::rowset(
                vec![
                    "locationID",
                    "solarSystemID",
                    "allianceID",
                    "corporationID",
                    "claimStructureID",
                ],
                rows,
            ));
        }
    } else if service == "map" && method == "GetMyExtraMapInfoAgents" {
        if let Ok(ref rep) = rpc_res {
            let rows = extract_table_rows(rep)
                .into_iter()
                .map(|r| {
                    let aid = row_field(&r, "agentID")
                        .or_else(|| row_field(&r, "fromID"))
                        .unwrap_or(PyRep::Integer(0));
                    let from_id = row_field(&r, "fromID").unwrap_or_else(|| aid.clone());
                    let rank = row_field(&r, "rank").unwrap_or(PyRep::Real(1.0));
                    let standing = row_field(&r, "standing").unwrap_or(PyRep::Real(0.0));
                    vec![from_id, aid, rank, standing]
                })
                .collect();
            return Ok(PyRep::rowset(
                vec!["fromID", "agentID", "rank", "standing"],
                rows,
            ));
        }
    } else if service == "map" && method == "GetSecurityModifiedSystems" {
        if let Ok(ref rep) = rpc_res {
            let rows = extract_table_rows(rep)
                .into_iter()
                .map(|r| {
                    let sys = row_field(&r, "solarSystemID").unwrap_or(PyRep::Integer(0));
                    let sec = row_field(&r, "security").unwrap_or(PyRep::Real(0.0));
                    let mod_sec = row_field(&r, "modifiedSecurity").unwrap_or_else(|| sec.clone());
                    vec![sys, sec, mod_sec]
                })
                .collect();
            return Ok(PyRep::rowset(
                vec!["solarSystemID", "security", "modifiedSecurity"],
                rows,
            ));
        }
    } else if service == "paperDollServer" && method == "GetPaperDollPortraitDataFor" {
        if let Ok(rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return Ok(PyRep::Tuple(vec![rep.clone(), rep]));
            }
            return Ok(rep);
        }
    } else if service == "dogmaIM" && method == "AddTarget" {
        if let Ok(PyRep::KeyVal(map)) = rpc_res {
            let locked = map
                .get("locked")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let target_id = map
                .get("targetID")
                .cloned()
                .unwrap_or(PyRep::Integer(0));
            return Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(locked)),
                PyRep::List(vec![target_id]),
            ]));
        }
    } else if service == "peaceTreatyMgr" && method == "GetPeaceTreatiesForSession" {
        if let Ok(rep) = rpc_res {
            return Ok(PyRep::Tuple(vec![rep.clone(), rep]));
        }
    } else if service == "petitioner" && method == "GetCategoryHierarchicalInfo" {
        if let Ok(PyRep::Tuple(mut parts)) = rpc_res {
            if parts.len() == 4 {
                match &parts[3] {
                    PyRep::None => {
                        parts[3] = PyRep::Dict(Vec::new());
                    }
                    PyRep::Dict(entries) => {
                        let norm_entries = entries
                            .iter()
                            .map(|(k, v)| {
                                if matches!(v, PyRep::Tuple(t) if t.len() == 2) {
                                    (k.clone(), v.clone())
                                } else {
                                    (
                                        k.clone(),
                                        PyRep::Tuple(vec![
                                            PyRep::String(String::new()),
                                            v.clone(),
                                        ]),
                                    )
                                }
                            })
                            .collect();
                        parts[3] = PyRep::Dict(norm_entries);
                    }
                    PyRep::List(items) => {
                        let norm_entries = items
                            .iter()
                            .map(|it| {
                                let label = format!("Category/{}", it.as_i64().unwrap_or(0));
                                (
                                    it.clone(),
                                    PyRep::Tuple(vec![
                                        PyRep::String(String::new()),
                                        PyRep::String(label),
                                    ]),
                                )
                            })
                            .collect();
                        parts[3] = PyRep::Dict(norm_entries);
                    }
                    _ => {}
                }
            }
            return Ok(PyRep::Tuple(parts));
        }
    } else if service == "planetMgr" && method == "GMGetSynchedServerState" {
        if let Ok(mut rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let sim_time = rep
                    .dict_get("currentSimTime")
                    .or_else(|| rep.dict_get("lastTick"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let raw_pins = extract_table_rows(
                    rep.dict_get("pins").unwrap_or(&PyRep::List(Vec::new())),
                );
                let norm_pins: Vec<PyRep> = raw_pins
                    .into_iter()
                    .map(|mut p| {
                        let pid = p
                            .dict_get("id")
                            .or_else(|| p.dict_get("pinID"))
                            .cloned()
                            .unwrap_or(PyRep::Integer(0));
                        match p {
                            PyRep::KeyVal(ref mut m) => {
                                m.entry("id".into()).or_insert(pid);
                                p
                            }
                            PyRep::Dict(entries) => {
                                let mut out_kv = std::collections::HashMap::new();
                                for (k, v) in entries {
                                    if let Some(ks) = k.as_str() {
                                        out_kv.insert(ks.to_string(), v);
                                    }
                                }
                                out_kv.entry("id".into()).or_insert(pid);
                                PyRep::KeyVal(out_kv)
                            }
                            other => other,
                        }
                    })
                    .collect();
                match rep {
                    PyRep::KeyVal(ref mut map) => {
                        map.entry("currentSimTime".into()).or_insert(sim_time);
                        map.insert("pins".into(), PyRep::List(norm_pins));
                    }
                    PyRep::Dict(ref mut entries) => {
                        if !entries
                            .iter()
                            .any(|(k, _)| k.as_str() == Some("currentSimTime"))
                        {
                            entries.push((PyRep::String("currentSimTime".into()), sim_time));
                        }
                        if let Some((_, v)) =
                            entries.iter_mut().find(|(k, _)| k.as_str() == Some("pins"))
                        {
                            *v = PyRep::List(norm_pins);
                        } else {
                            entries.push((PyRep::String("pins".into()), PyRep::List(norm_pins)));
                        }
                    }
                    _ => {}
                }
                return Ok(PyRep::Tuple(vec![PyRep::Integer(0), rep]));
            }
            return Ok(rep);
        }
    } else if service == "planetMgr" && method == "GetPlanetsForChar" {
        if let Ok(rep) = rpc_res {
            if matches!(
                rep,
                PyRep::List(_) | PyRep::Rowset { .. } | PyRep::ObjectEx { .. }
            ) {
                let items = extract_table_rows(&rep);
                let out = items
                    .into_iter()
                    .map(|mut it| {
                        let pins_cnt = it
                            .dict_get("numberOfPins")
                            .or_else(|| it.dict_get("pinsCount"))
                            .cloned()
                            .unwrap_or(PyRep::Integer(0));
                        let has_colony = it.dict_get("colonyData").is_some();
                        match it {
                            PyRep::KeyVal(ref mut map) => {
                                map.entry("numberOfPins".into()).or_insert(pins_cnt);
                                if !has_colony {
                                    map.insert(
                                        "colonyData".into(),
                                        PyRep::keyval([("pins", PyRep::List(Vec::new()))]),
                                    );
                                }
                            }
                            PyRep::Dict(ref mut entries) => {
                                if !entries
                                    .iter()
                                    .any(|(k, _)| k.as_str() == Some("numberOfPins"))
                                {
                                    entries.push((PyRep::String("numberOfPins".into()), pins_cnt));
                                }
                                if !has_colony {
                                    entries.push((
                                        PyRep::String("colonyData".into()),
                                        PyRep::keyval([("pins", PyRep::List(Vec::new()))]),
                                    ));
                                }
                            }
                            _ => {}
                        }
                        it
                    })
                    .collect();
                return Ok(PyRep::List(out));
            }
            return Ok(rep);
        }
    } else if service == "posMgr" && method == "GetControlTowers" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let out = items
                .into_iter()
                .map(|it| {
                    if matches!(it, PyRep::Tuple(ref t) if t.len() == 3) {
                        it
                    } else {
                        let sid = it
                            .dict_get("structureID")
                            .or_else(|| it.dict_get("itemID"))
                            .cloned()
                            .unwrap_or(PyRep::Integer(0));
                        let tid = it.dict_get("typeID").cloned().unwrap_or(PyRep::Integer(0));
                        PyRep::Tuple(vec![sid, tid, it])
                    }
                })
                .collect();
            return Ok(PyRep::List(out));
        }
    } else if service == "raffleProxy" && method == "GetMyRaffleHistory" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for (idx, it) in items.into_iter().enumerate() {
                if it.dict_get("running_id").is_some() {
                    out.push(it);
                } else {
                    out.push(PyRep::keyval([
                        ("running_id", PyRep::Integer((idx + 1) as i64)),
                        ("raffle_id", PyRep::Integer((idx + 1) as i64)),
                        ("type_id", PyRep::Integer(0)),
                    ]));
                }
            }
            let total = out.len() as i64;
            return Ok(PyRep::Tuple(vec![PyRep::List(out), PyRep::Integer(total)]));
        }
    } else if service == "reprocessingSvc" && method == "GetQuotes" {
        if let Ok(PyRep::Tuple(parts)) = rpc_res {
            if parts.len() >= 3 && matches!(parts[1], PyRep::Real(_)) && matches!(parts[2], PyRep::List(_)) {
                let tax = parts[0].clone();
                let base_yield = parts[1].clone();
                let PyRep::List(quotes) = parts[2].clone() else {
                    unreachable!()
                };
                let eff_dict = PyRep::Dict(vec![
                    (PyRep::None, base_yield.clone()),
                    (PyRep::Integer(25), base_yield),
                ]);
                let mut quote_entries = Vec::with_capacity(quotes.len());
                for q in quotes {
                    let item_id = q.dict_get("itemID").cloned().unwrap_or(PyRep::Integer(0));
                    let recoverables = match q.dict_get("outputs") {
                        Some(PyRep::List(outs)) => outs
                            .iter()
                            .map(|o| {
                                let tid = o.dict_get("typeID").cloned().unwrap_or(PyRep::Integer(0));
                                let qty = o.dict_get("quantity").cloned().unwrap_or(PyRep::Integer(0));
                                PyRep::keyval([
                                    ("typeID", tid),
                                    ("client", qty),
                                    ("station", PyRep::Integer(0)),
                                    ("unrecoverable", PyRep::Integer(0)),
                                ])
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    let entry = PyRep::keyval([
                        ("typeID", PyRep::Integer(0)),
                        ("totalISKCost", PyRep::Real(0.0)),
                        ("recoverables", PyRep::List(recoverables)),
                    ]);
                    quote_entries.push((item_id, entry));
                }
                return Ok(PyRep::Tuple(vec![tax, eff_dict, PyRep::Dict(quote_entries)]));
            }
            return Ok(PyRep::Tuple(parts));
        }
    } else if service == "scanMgr" && method == "RecoverProbes" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let recovered = rep
                    .dict_get("recoveredProbeIDs")
                    .or_else(|| rep.dict_get("probeIDs"))
                    .cloned()
                    .unwrap_or_else(|| PyRep::List(Vec::new()));
                return Ok(recovered);
            }
        }
    } else if service == "ship" && method == "GetShipFittingInfo" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let cpu_load = rep.dict_get("cpuLoad").cloned().unwrap_or(PyRep::Real(0.0));
                let cpu_out = rep.dict_get("cpuOutput").cloned().unwrap_or(PyRep::Real(0.0));
                let pwg_load = rep.dict_get("powerLoad").cloned().unwrap_or(PyRep::Real(0.0));
                let pwg_out = rep.dict_get("powerOutput").cloned().unwrap_or(PyRep::Real(0.0));
                return Ok(PyRep::Tuple(vec![cpu_load, cpu_out, pwg_load, pwg_out]));
            }
        }
    } else if service == "skillMgr2" && method == "GetSkillQueueAndFreePoints" {
        if let Ok(PyRep::Tuple(mut parts)) = rpc_res {
            let mut norm_q = Vec::new();
            match parts.first() {
                Some(PyRep::Rowset { columns, rows }) => {
                    for (idx, row) in rows.iter().enumerate() {
                        let get_col = |name: &str| -> Option<PyRep> {
                            columns
                                .iter()
                                .position(|c| c == name)
                                .and_then(|i| row.get(i).cloned())
                        };
                        norm_q.push(PyRep::keyval([
                            ("queuePosition", PyRep::Integer(idx as i64)),
                            ("trainingTypeID", get_col("trainingTypeID").unwrap_or(PyRep::Integer(0))),
                            ("trainingToLevel", get_col("trainingToLevel").unwrap_or(PyRep::Integer(0))),
                            ("trainingStartTime", get_col("trainingStartTime").unwrap_or(PyRep::Integer(0))),
                            ("trainingEndTime", get_col("trainingEndTime").unwrap_or(PyRep::Integer(0))),
                            ("trainingStartSP", get_col("trainingStartSP").unwrap_or(PyRep::Real(0.0))),
                            ("trainingDestinationSP", get_col("trainingDestinationSP").unwrap_or(PyRep::Real(0.0))),
                        ]));
                    }
                }
                Some(PyRep::List(queue)) => {
                    for (idx, entry) in queue.iter().enumerate() {
                        norm_q.push(PyRep::keyval([
                            ("queuePosition", PyRep::Integer(idx as i64)),
                            ("trainingTypeID", entry.dict_get("trainingTypeID").cloned().unwrap_or(PyRep::Integer(0))),
                            ("trainingToLevel", entry.dict_get("trainingToLevel").cloned().unwrap_or(PyRep::Integer(0))),
                            ("trainingStartTime", entry.dict_get("trainingStartTime").cloned().unwrap_or(PyRep::Integer(0))),
                            ("trainingEndTime", entry.dict_get("trainingEndTime").cloned().unwrap_or(PyRep::Integer(0))),
                            ("trainingStartSP", entry.dict_get("trainingStartSP").cloned().unwrap_or(PyRep::Real(0.0))),
                            ("trainingDestinationSP", entry.dict_get("trainingDestinationSP").cloned().unwrap_or(PyRep::Real(0.0))),
                        ]));
                    }
                }
                _ => {}
            }
            if !parts.is_empty() {
                parts[0] = PyRep::List(norm_q);
            }
            return Ok(PyRep::Tuple(parts));
        }
    } else if service == "charMgr" && method == "GetContactList" {
        if let Ok(PyRep::KeyVal(mut map)) = rpc_res {
            map.entry("blocked".into()).or_insert_with(|| {
                PyRep::rowset(vec!["contactID", "senderID"], Vec::new())
            });
            return Ok(PyRep::KeyVal(map));
        }
    } else if service == "stationSvc" && method == "GetAllianceSystems" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for it in items {
                match it {
                    PyRep::Integer(sys_id) => {
                        out.push(PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(sys_id)),
                            ("allianceID", PyRep::Integer(0)),
                        ]));
                    }
                    other => out.push(other),
                }
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "stationSvc" && method == "GetSolarSystem" {
        if let Ok(rep) = rpc_res {
            return match rep {
                PyRep::Integer(sys_id) => Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id)),
                    ("factionID", PyRep::None),
                ])),
                mut other => {
                    if other.dict_get("factionID").is_none() {
                        match other {
                            PyRep::KeyVal(ref mut map) => {
                                map.insert("factionID".into(), PyRep::None);
                            }
                            PyRep::Dict(ref mut entries) => {
                                entries.push((PyRep::String("factionID".into()), PyRep::None));
                            }
                            _ => {}
                        }
                    }
                    Ok(other)
                }
            };
        }
    } else if service == "structureAssetSafety" && method == "GetStructuresICanDeliverTo" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let first = items.first().cloned().unwrap_or(PyRep::None);
            return Ok(PyRep::Tuple(vec![PyRep::List(items), first]));
        }
    } else if service == "structureCompressionMgr"
        && method == "GetMyGasDecompressionEfficiency"
    {
        if let Ok(PyRep::Real(eff)) = rpc_res {
            return Ok(PyRep::Tuple(vec![PyRep::Real(eff), PyRep::Real(eff)]));
        }
    } else if service == "warStatisticMgr" && method == "GetBaseInfo" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let decl_id = rep.dict_get("declaredByID").cloned().unwrap_or(PyRep::Integer(0));
                let agst_id = rep.dict_get("againstID").cloned().unwrap_or(PyRep::Integer(0));
                let k_agg = rep.dict_get("killsForAggressor").cloned().unwrap_or(PyRep::Integer(0));
                let k_def = rep.dict_get("killsForDefender").cloned().unwrap_or(PyRep::Integer(0));
                let isk_agg = rep.dict_get("iskDestroyedByAggressor").cloned().unwrap_or(PyRep::Real(0.0));
                let isk_def = rep.dict_get("iskDestroyedByDefender").cloned().unwrap_or(PyRep::Real(0.0));
                let allies = rep.dict_get("allies").cloned().unwrap_or_else(|| PyRep::Dict(Vec::new()));
                let ships_killed = PyRep::Dict(vec![(decl_id.clone(), k_agg), (agst_id.clone(), k_def)]);
                let isk_killed = PyRep::Dict(vec![(decl_id, isk_agg), (agst_id, isk_def)]);
                return Ok(PyRep::Tuple(vec![rep.clone(), ships_killed, isk_killed, allies]));
            }
        }
    } else if service == "warsInfoMgr" && method == "GetPublicWarInfo" {
        if let Ok(rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return Ok(PyRep::List(vec![rep]));
            }
            return Ok(rep);
        }
    } else if service == "storeManager"
        && matches!(method, "get_categories" | "get_products" | "get_offers")
    {
        if let Ok(rep) = rpc_res {
            return Ok(keyval_to_dict_recursive(rep));
        }
    }
    rpc_res
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

#[cfg(test)]
#[path = "normalize_replies_cov_unit_tests.rs"]
mod normalize_replies_cov_unit_tests;

