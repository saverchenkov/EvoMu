use std::collections::HashMap;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};

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

fn kv_float(entries: &HashMap<String, PyRep>, key: &str) -> f64 {
    match entries.get(key) {
        Some(PyRep::Real(v)) => *v,
        Some(PyRep::Integer(v)) => *v as f64,
        _ => 0.0,
    }
}

fn rowset_to_dict_by_key(rep: PyRep, key_col: &str) -> PyRep {
    let PyRep::Rowset { columns, rows } = rep else {
        return rep;
    };
    let key_idx = columns.iter().position(|c| c == key_col).unwrap_or(0);
    let mut dict = Vec::with_capacity(rows.len());
    for row in rows {
        let row_key = row
            .get(key_idx)
            .cloned()
            .unwrap_or(PyRep::Integer(dict.len() as i64 + 1));
        let entries: HashMap<String, PyRep> = columns.iter().cloned().zip(row).collect();
        dict.push((row_key, PyRep::KeyVal(entries)));
    }
    PyRep::Dict(dict)
}

fn inv_keyval_to_dbrow(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(entries) = rep else {
        return rep;
    };
    let group_id = kv_int(&entries, "groupID", 25);
    let category_id = kv_int(&entries, "categoryID", 7);
    let cells = vec![
        PyRep::Integer(kv_int(&entries, "itemID", 0)),
        PyRep::Integer(kv_int(&entries, "typeID", 0)),
        PyRep::Integer(kv_int(&entries, "ownerID", 0)),
        PyRep::Integer(kv_int(&entries, "locationID", 0)),
        PyRep::Integer(kv_int(&entries, "flagID", 0)),
        PyRep::Integer(kv_int(&entries, "quantity", 0)),
        PyRep::Integer(if group_id > 0 { group_id } else { 25 }),
        PyRep::Integer(if category_id > 0 { category_id } else { 7 }),
        entries
            .get("customInfo")
            .cloned()
            .unwrap_or_else(|| PyRep::String(kv_str(&entries, "customInfo"))),
    ];
    DBRowDescriptor::inv_item_header().build_dbrow(cells)
}

fn ensure_scan_result_data(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    if entries.get("scanGroupID").is_none() {
        let sg = match kv_str(&entries, "targetClass").as_str() {
            "CosmicAnomaly" | "Anomaly" => 2,
            "Ship" | "Structure" | "AbyssalTrace" => 8,
            "Drone" | "Probe" => 16,
            _ => 4,
        };
        entries.insert("scanGroupID".to_string(), PyRep::Integer(sg));
    }
    if entries.get("groupID").is_none() {
        entries.insert("groupID".to_string(), PyRep::Integer(502));
    }
    if entries.get("typeID").is_none() {
        entries.insert("typeID".to_string(), PyRep::Integer(502));
    }
    if !matches!(entries.get("data"), None | Some(PyRep::None)) {
        return PyRep::KeyVal(entries);
    }
    let pos = match entries.get("pos") {
        Some(PyRep::Tuple(coords)) | Some(PyRep::List(coords)) if coords.len() >= 3 => {
            coords[..3].to_vec()
        }
        _ => vec![
            PyRep::Real(kv_float(&entries, "x")),
            PyRep::Real(kv_float(&entries, "y")),
            PyRep::Real(kv_float(&entries, "z")),
        ],
    };
    entries.insert("data".to_string(), PyRep::Tuple(pos));
    PyRep::KeyVal(entries)
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

fn normalize_contract_reply(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    let mut c_map = entries.clone();
    let type_int = match kv_str(&entries, "type").as_str() {
        "ItemExchange" | "item_exchange" => 1,
        "Auction" | "auction" => 2,
        "Courier" | "courier" => 3,
        _ => kv_int(&entries, "type", 1),
    };
    let status_int = match kv_str(&entries, "status").as_str() {
        "Outstanding" | "outstanding" => 0,
        "InProgress" | "in_progress" => 1,
        "FinishedIssuer" | "finished_issuer" => 2,
        "FinishedContractor" | "finished_contractor" => 3,
        "Finished" | "finished" => 4,
        "Cancelled" | "cancelled" => 5,
        "Rejected" | "rejected" => 6,
        "Failed" | "failed" => 7,
        "Deleted" | "deleted" => 8,
        "Reversed" | "reversed" => 9,
        _ => kv_int(&entries, "status", 0),
    };
    let avail_int = match kv_str(&entries, "availability").as_str() {
        "Private" | "private" | "Personal" | "personal" | "Corporation" | "corporation" => 1,
        _ => kv_int(&entries, "availability", 0),
    };
    let unix_to_ft = |u: i64| -> i64 {
        if u > 100_000_000_000_000 {
            u
        } else if u > 0 {
            (u + 11_644_473_600) * 10_000_000
        } else {
            133_500_000_000_000_000
        }
    };
    let issued_ft = unix_to_ft(kv_int(&entries, "dateIssued", 1_700_000_000));
    let expired_ft = unix_to_ft(kv_int(&entries, "dateExpired", 1_700_086_400));
    let accepted_ft = unix_to_ft(kv_int(&entries, "dateAccepted", 1_700_000_000));
    c_map.insert("type".to_string(), PyRep::Integer(type_int));
    c_map.insert("status".to_string(), PyRep::Integer(status_int));
    c_map.insert("availability".to_string(), PyRep::Integer(avail_int));
    c_map.insert("startSolarSystemID".to_string(), PyRep::Integer(kv_int(&entries, "startSolarSystemID", 30_000_142)));
    c_map.insert("endSolarSystemID".to_string(), PyRep::Integer(kv_int(&entries, "endSolarSystemID", 30_000_142)));
    c_map.insert("startRegionID".to_string(), PyRep::Integer(kv_int(&entries, "startRegionID", 10_000_002)));
    c_map.insert("endRegionID".to_string(), PyRep::Integer(kv_int(&entries, "endRegionID", 10_000_002)));
    c_map.insert("dateIssued".to_string(), PyRep::Integer(issued_ft));
    c_map.insert("dateExpired".to_string(), PyRep::Integer(expired_ft));
    c_map.insert("dateAccepted".to_string(), PyRep::Integer(accepted_ft));
    c_map.insert("dateCompleted".to_string(), PyRep::Integer(accepted_ft));
    c_map.insert("numDays".to_string(), PyRep::Integer(kv_int(&entries, "numDays", 1)));
    c_map.insert("volume".to_string(), PyRep::Real(kv_float(&entries, "volume").max(1.0)));
    c_map.insert("crateID".to_string(), PyRep::Integer(0));
    entries.insert("contract".to_string(), PyRep::KeyVal(c_map));
    if let Some(PyRep::List(items)) = entries.get_mut("items") {
        for item in items.iter_mut() {
            if let PyRep::KeyVal(item_map) = item {
                let tid = kv_int(item_map, "itemTypeID", kv_int(item_map, "typeID", 34));
                let qty = kv_int(item_map, "quantity", 1).max(1);
                item_map.entry("inCrate".to_string()).or_insert(PyRep::Bool(true));
                item_map.insert("itemTypeID".to_string(), PyRep::Integer(tid));
                item_map.insert("quantity".to_string(), PyRep::Integer(qty));
            }
        }
    }
    entries.entry("bids".to_string()).or_insert_with(|| PyRep::List(Vec::new()));
    PyRep::KeyVal(entries)
}

fn normalize_skill_queue_rowset(rep: PyRep) -> PyRep {
    let PyRep::Rowset { mut columns, mut rows } = rep else {
        return rep;
    };
    if columns.iter().any(|c| c == "trainingEndTime") {
        return PyRep::Rowset { columns, rows };
    }
    let dur_idx = columns.iter().position(|c| c == "durationSeconds");
    columns.push("queuePosition".to_string());
    columns.push("trainingStartTime".to_string());
    columns.push("trainingEndTime".to_string());
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000);
    let mut cur_ft = (now_unix + 11_644_473_600) * 10_000_000;
    for (idx, row) in rows.iter_mut().enumerate() {
        let dur_secs = dur_idx
            .and_then(|i| row.get(i))
            .and_then(|v| v.as_f64())
            .unwrap_or(60.0)
            .max(1.0);
        let start_ft = cur_ft;
        let end_ft = start_ft + (dur_secs * 10_000_000.0) as i64;
        cur_ft = end_ft;
        row.push(PyRep::Integer(idx as i64));
        row.push(PyRep::Integer(start_ft));
        row.push(PyRep::Integer(end_ft));
    }
    PyRep::Rowset { columns, rows }
}

/// MachoNet wire-compatibility adjustments for live EVE V24.01 clients.
pub fn normalize_macho_rpc_reply(
    service: &str,
    method: &str,
    rpc_res: Result<PyRep, BridgeRpcError>,
) -> Result<PyRep, BridgeRpcError> {
    if service == "XmppChatMgr" && method == "ResyncSystemChannelAccess" {
        if let Ok(ref rep) = rpc_res {
            if let Some(PyRep::List(chans)) = rep.dict_get("channels") {
                return Ok(PyRep::List(
                    chans
                        .iter()
                        .filter_map(|c| c.as_str())
                        .map(|s| {
                            if let Some(rest) = s.strip_prefix("solarsystemid2:") {
                                format!("local_{rest}")
                            } else if let Some(rest) = s.strip_prefix("corpid:") {
                                format!("corp_{rest}")
                            } else if let Some(rest) = s.strip_prefix("allianceid:") {
                                format!("alliance_{rest}")
                            } else if let Some(rest) = s.strip_prefix("fleetid:") {
                                format!("fleet_{rest}")
                            } else {
                                s.replace(':', "_")
                            }
                        })
                        .map(PyRep::String)
                        .collect(),
                ));
            }
        }
    }
    if service == "fleetProxy" && method == "GetAvailableFleetAds" {
        if let Ok(rep) = rpc_res {
            return Ok(rowset_to_dict_by_key(rep, "fleetID"));
        }
    }
    if service == "contractProxy" && (method == "GetContract" || method == "GetContractInfo") {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_contract_reply(rep));
        }
    }
    if service == "skillMgr2" && method == "GetSkillQueue" {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_skill_queue_rowset(rep));
        }
    }
    if service == "skillMgr2" && method == "GetSkillQueueAndFreePoints" {
        if let Ok(PyRep::Tuple(mut parts)) = rpc_res {
            if let Some(first) = parts.first_mut() {
                let q = std::mem::replace(first, PyRep::None);
                *first = normalize_skill_queue_rowset(q);
            }
            return Ok(PyRep::Tuple(parts));
        }
    }
    if service == "config" && method == "GetMultiCorpTickerNamesEx" {
        if let Ok(PyRep::List(packed_rows)) = rpc_res {
            let header = DBRowDescriptor::corp_ticker_header().to_pyrep();
            return Ok(PyRep::ObjectEx {
                is_type_2: true,
                header: Box::new(PyRep::Tuple(vec![
                    PyRep::Tuple(vec![PyRep::Token(
                        "carbon.common.script.sys.crowset.CRowset".into(),
                    )]),
                    PyRep::Dict(vec![(PyRep::String("header".into()), header)]),
                ])),
                list: packed_rows,
                dict: Vec::new(),
            });
        }
    }
    if service == "sovMgr"
        && matches!(
            method,
            "GetSovStructuresInfoForLocalSolarSystem" | "GetSovStructuresInfoForSolarSystem"
        )
    {
        if let Ok(rep) = rpc_res {
            if let Some(structs) = rep.dict_get("structures").cloned() {
                return Ok(structs);
            }
            return Ok(PyRep::List(Vec::new()));
        }
    }
    if service == "marketProxy"
        && matches!(method, "CharGetTransactions" | "CorpGetTransactions")
    {
        if let Ok(PyRep::Rowset { columns, rows }) = rpc_res {
            let desc = DBRowDescriptor::new(
                columns
                    .into_iter()
                    .map(|col| {
                        let ctype = market_tx_column_type(&col);
                        (col, ctype)
                    })
                    .collect::<Vec<(String, DbColumnType)>>(),
            );
            return Ok(desc.build_crowset(rows));
        }
    }
    if service == "facilityManager" && matches!(method, "GetFacilities" | "GetFacilitiesByID") {
        if let Ok(PyRep::List(items)) = rpc_res {
            return Ok(PyRep::List(
                items.into_iter().map(normalize_facility_rep).collect(),
            ));
        }
    }
    if service == "facilityManager" && method == "GetFacility" {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_facility_rep(rep));
        }
    }
    if service == "map" && method == "GetIncursionGlobalReport" {
        if let Ok(PyRep::List(items)) = rpc_res {
            return Ok(PyRep::List(
                items.into_iter().map(normalize_incursion_report_rep).collect(),
            ));
        }
    }
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
    if (matches!(service, "corpRegistry" | "machoNet") && method == "GetCorporation")
        || (service == "corpmgr" && matches!(method, "GetCorporations" | "GetPublicInfo"))
    {
        if let Ok(PyRep::KeyVal(entries)) = rpc_res {
            let mut header = Vec::with_capacity(entries.len());
            let mut line = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                header.push(PyRep::String(k));
                line.push(v);
            }
            return Ok(PyRep::Object {
                type_name: "util.Row".into(),
                args: Box::new(PyRep::Tuple(vec![PyRep::List(header), PyRep::List(line)])),
            });
        }
    }
    crate::quasar_grpc::normalize_macho_rpc_reply(service, method, rpc_res)
}

fn market_tx_column_type(col: &str) -> DbColumnType {
    match col {
        "price" | "credit" => DbColumnType::R8,
        "transactionDate" | "date" => DbColumnType::FileTime,
        "typeID" | "quantity" | "buyerAccountID" | "sellerAccountID" | "accountKey" | "keyID" => {
            DbColumnType::I4
        }
        "transactionID"
        | "stationID"
        | "buyerID"
        | "sellerID"
        | "accountID"
        | "clientID"
        | "journalRefID"
        | "journalTransactionID"
        | "characterID"
        | "corporationID"
        | "regionID"
        | "solarSystemID"
        | "locationID" => DbColumnType::I8,
        _ => DbColumnType::WStr,
    }
}

fn normalize_facility_rep(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    const INDUSTRY_CATEGORIES: &[i64] = &[
        4, 5, 6, 7, 8, 9, 16, 17, 18, 20, 22, 23, 24, 25, 32, 34, 35, 39, 40, 41, 42, 43, 46, 65,
        66, 87, 91,
    ];
    if let Some(acts) = entries.get("activities").cloned() {
        if let PyRep::List(act_list) = acts {
            let cats: Vec<PyRep> = INDUSTRY_CATEGORIES
                .iter()
                .map(|&c| PyRep::Integer(c))
                .collect();
            let act_dict = act_list
                .into_iter()
                .filter_map(|v| v.as_i64())
                .map(|act_id| {
                    let tuple = PyRep::Tuple(vec![
                        PyRep::List(Vec::new()),
                        PyRep::List(Vec::new()),
                        PyRep::List(Vec::new()),
                        PyRep::List(cats.clone()),
                        PyRep::List(Vec::new()),
                        PyRep::List(Vec::new()),
                    ]);
                    (PyRep::Integer(act_id), tuple)
                })
                .collect();
            entries.insert("activities".into(), PyRep::Dict(act_dict));
        }
    }
    entries
        .entry("serviceAccess".into())
        .or_insert_with(|| PyRep::Dict(Vec::new()));
    entries
        .entry("sccTaxModifier".into())
        .or_insert(PyRep::Real(1.0));
    entries
        .entry("rigModifiers".into())
        .or_insert_with(|| PyRep::Dict(Vec::new()));
    entries
        .entry("globalModifiers".into())
        .or_insert_with(|| {
            PyRep::Dict(vec![
                (PyRep::String("faction".into()), PyRep::Real(1.0)),
                (PyRep::String("system".into()), PyRep::Dict(Vec::new())),
            ])
        });
    PyRep::KeyVal(entries)
}

fn normalize_incursion_report_rep(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    if let Some(state_val) = entries.get("state").cloned() {
        if let PyRep::String(s) = state_val {
            let code = match s.as_str() {
                "Withdrawing" | "withdrawing" => 0,
                "Mobilizing" | "mobilizing" => 1,
                _ => 2,
            };
            entries.insert("state".into(), PyRep::Integer(code));
        }
    }
    let faction_id = entries
        .get("factionID")
        .and_then(|v| v.as_i64())
        .unwrap_or(500_019);
    entries
        .entry("aggressorFactionID".into())
        .or_insert(PyRep::Integer(faction_id));
    let has_boss = entries
        .get("hasBoss")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    entries
        .entry("hasFinalEncounter".into())
        .or_insert(PyRep::Bool(has_boss));
    entries
        .entry("effects".into())
        .or_insert_with(|| PyRep::List(Vec::new()));
    PyRep::KeyVal(entries)
}

fn normalize_damage_state_tuple(rep: PyRep) -> PyRep {
    let PyRep::Tuple(mut state) = rep else {
        return rep;
    };
    if state.len() == 3 {
        if let Some(PyRep::Tuple(shield_tuple)) = state.first() {
            if let Some(shield_ratio) = shield_tuple.first().cloned() {
                state[0] = shield_ratio;
            }
        }
    }
    PyRep::Tuple(state)
}

fn destiny_entry_func_name(item: &PyRep) -> Option<&str> {
    let PyRep::Tuple(outer) = item else {
        return None;
    };
    if outer.len() == 2 {
        if let PyRep::Tuple(inner) = &outer[1] {
            return inner.first().and_then(|f| f.as_str());
        }
        return outer.first().and_then(|f| f.as_str());
    }
    None
}

fn normalize_destiny_payload_arg(rep: PyRep) -> PyRep {
    match rep {
        PyRep::List(items) => {
            let has_ball_add = items.iter().any(|it| {
                matches!(
                    destiny_entry_func_name(it),
                    Some("SetState" | "AddBalls" | "AddBalls2")
                )
            });
            PyRep::List(
                items
                    .into_iter()
                    .filter(|it| {
                        let fname = destiny_entry_func_name(it);
                        fname != Some("OnDockingAccepted")
                            && fname != Some("OnClientControllerEvent")
                            && !(has_ball_add && fname == Some("OnSlimItemChange"))
                    })
                    .map(normalize_destiny_payload_arg)
                    .collect(),
            )
        }
        PyRep::Tuple(items) => {
            PyRep::Tuple(items.into_iter().map(normalize_destiny_payload_arg).collect())
        }
        other => other,
    }
}

/// Returns `true` when a server notification uses an internal test-only tuple shape that
/// has no bound MachoNet moniker on the live TCP client wire.
pub fn should_suppress_macho_notification(event: &str, args: &[PyRep]) -> bool {
    if matches!(event, "OnDamageStateChange" | "OnSessionChanged") {
        return true;
    }
    if event == "OnTradeInitiate" && matches!(args.get(1), Some(PyRep::Integer(_))) {
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
            if matches!(normalize_destiny_payload_arg(first.clone()), PyRep::List(items) if items.is_empty()) {
                return true;
            }
        }
    }
    false
}

/// Normalizes MachoNet broadcast notifications for live EVE V24.01 clients.
pub fn normalize_macho_notification(event: &str, mut args: Vec<PyRep>) -> Vec<PyRep> {
    match event {
        "OnContractCreated" => Vec::new(),
        "OnWarpFinished" => {
            if args.len() > 2 {
                let first = args.into_iter().next().unwrap_or(PyRep::Integer(0));
                vec![first, PyRep::Integer(0)]
            } else {
                args
            }
        }
        "OnTradeOffer" | "OnTradeMoneyOffer" => {
            args.truncate(2);
            args
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
        "OnNewSkillQueueSaved" => {
            if let Some(first) = args.first_mut() {
                let q = std::mem::replace(first, PyRep::None);
                *first = normalize_skill_queue_rowset(q);
            }
            args
        }
        "OnDamageStateChange" => {
            if args.len() >= 2 {
                let dmg = std::mem::replace(&mut args[1], PyRep::None);
                args[1] = normalize_damage_state_tuple(dmg);
            }
            args
        }
        "OnSlimItemChange" => {
            if args.len() == 2 && matches!(args.first(), Some(PyRep::Integer(_))) {
                args[0] = args[1].clone();
            }
            args
        }
        "DoDestinyUpdate" => crate::quasar_grpc::normalize_macho_notification(event, args)
            .into_iter()
            .map(normalize_destiny_payload_arg)
            .collect(),
        "DoDestinyUpdates" => args
            .into_iter()
            .map(|arg| match arg {
                PyRep::List(entries) => normalize_destiny_payload_arg(PyRep::List(
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
                )),
                other => normalize_destiny_payload_arg(other),
            })
            .collect(),
        "OnSpecialFX" => {
            if args.first().is_some_and(|a| matches!(a, PyRep::List(_))) {
                args
            } else {
                vec![PyRep::List(vec![PyRep::Tuple(args)])]
            }
        }
        "OnGodmaShipEffect" => normalize_on_godma_ship_effect(args),
        "OnGodmaPrimeItem" => {
            if let Some(PyRep::KeyVal(entries)) = args.get_mut(1) {
                if !entries.contains_key("wallclockTime") {
                    let wt = entries
                        .get("time")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    entries.insert("wallclockTime".to_string(), wt);
                }
            }
            args
        }
        "OnItemChange" => {
            if let Some(first) = args.first_mut() {
                let old = std::mem::replace(first, PyRep::None);
                *first = inv_keyval_to_dbrow(old);
            }
            if args.len() == 2 {
                args.push(PyRep::None);
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
        "OnSystemScanStopped" => {
            if let Some(PyRep::List(rows)) = args.get_mut(1) {
                let updated = std::mem::take(rows)
                    .into_iter()
                    .map(ensure_scan_result_data)
                    .collect();
                *rows = updated;
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
            if let Some(PyRep::List(items)) = args.first_mut() {
                for item in items.iter_mut() {
                    if let PyRep::Tuple(elems) = item {
                        if elems.len() == 7 {
                            elems.push(PyRep::None);
                        }
                    }
                }
            } else {
                args = vec![PyRep::List(Vec::new())];
            }
            args
        }
        _ => crate::quasar_grpc::normalize_macho_notification(event, args),
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

fn normalize_godma_environment(env: PyRep, item_id: &PyRep, effect_id: &PyRep) -> PyRep {
    match env {
        PyRep::Tuple(mut elems) => {
            if elems.len() == 3 {
                return PyRep::Tuple(vec![
                    elems[0].clone(),
                    PyRep::None,
                    elems[1].clone(),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    elems[2].clone(),
                ]);
            }
            while elems.len() < 7 {
                match elems.len() {
                    5 => elems.push(PyRep::List(Vec::new())),
                    6 => elems.push(effect_id.clone()),
                    _ => elems.push(PyRep::None),
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

fn normalize_on_godma_ship_effect(mut args: Vec<PyRep>) -> Vec<PyRep> {
    if args.is_empty() {
        return Vec::new();
    }
    let (secs, nanos) = eve_db::clock::host_wall_unix_secs_and_nanos();
    let fallback_ft = (secs as i64 + 11_644_473_600) * 10_000_000 + (nanos as i64 / 100);
    if args.len() >= 10 {
        let item_id = args[0].clone();
        let effect_id = args[1].clone();
        ensure_filetime_pyrep(&mut args[2], fallback_ft);
        let env = std::mem::replace(&mut args[5], PyRep::None);
        args[5] = normalize_godma_environment(env, &item_id, &effect_id);
        ensure_filetime_pyrep(&mut args[6], fallback_ft);
        if args.len() == 10 {
            args.push(PyRep::None);
        }
        return args;
    }
    let item_id = args[0].clone();
    let effect_id = args.get(1).cloned().unwrap_or(PyRep::Integer(0));
    let active_flag = PyRep::Integer(if args.get(2).and_then(PyRep::as_bool).unwrap_or(false) {
        1
    } else {
        0
    });
    let env = normalize_godma_environment(PyRep::None, &item_id, &effect_id);
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
mod client_compat_tests {
    use super::*;

    #[test]
    fn normalize_on_godma_ship_effect_ensures_non_zero_filetime_and_seven_tuple_env() {
        let short = normalize_macho_notification(
            "OnGodmaShipEffect",
            vec![
                PyRep::Integer(200_018_015),
                PyRep::Integer(101),
                PyRep::Bool(true),
            ],
        );
        assert_eq!(short.len(), 11);
        assert!(short[2].as_i64().unwrap() > i32::MAX as i64);
        assert!(short[6].as_i64().unwrap() > i32::MAX as i64);
        assert!(matches!(&short[5], PyRep::Tuple(t) if t.len() == 7));

        let valid_ft: i64 = 134_384_360_643_585_500;
        let ten_zero_ft = normalize_macho_notification(
            "OnGodmaShipEffect",
            vec![
                PyRep::Integer(200_018_015),
                PyRep::Integer(101),
                PyRep::Integer(0),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::Integer(200_018_015),
                    PyRep::Integer(700_000_001),
                    PyRep::Integer(101),
                ]),
                PyRep::Integer(0),
                PyRep::Real(4000.0),
                PyRep::Integer(0),
                PyRep::None,
            ],
        );
        assert_eq!(ten_zero_ft.len(), 11);
        assert!(ten_zero_ft[2].as_i64().unwrap() > i32::MAX as i64);
        assert!(ten_zero_ft[6].as_i64().unwrap() > i32::MAX as i64);
        assert!(matches!(&ten_zero_ft[5], PyRep::Tuple(t) if t.len() == 7));

        let preserved = normalize_macho_notification(
            "OnGodmaShipEffect",
            vec![
                PyRep::Integer(200_018_015),
                PyRep::Integer(101),
                PyRep::Integer(valid_ft),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::Integer(200_018_015),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(700_000_001),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    PyRep::Integer(101),
                ]),
                PyRep::Integer(valid_ft),
                PyRep::Real(4000.0),
                PyRep::Integer(0),
                PyRep::None,
                PyRep::None,
            ],
        );
        assert_eq!(preserved[2].as_i64(), Some(valid_ft));
        assert_eq!(preserved[6].as_i64(), Some(valid_ft));
    }

    #[test]
    fn wrapped_named_user_error_body_emits_unmacho_destination_for_moniker_retry() {
        let body = eve_machonet::wrapped_named_user_error_body(
            6,
            "UnMachoDestination",
            "InvalidBoundObject: Bound object 10136 does not exist",
        );
        let PyRep::Tuple(parts) = body else {
            panic!("expected ErrorResponse tuple body");
        };
        assert_eq!(parts[0].as_i64(), Some(6));
        assert_eq!(parts[1].as_i64(), Some(eve_machonet::MACHONETERR_WRAPPEDEXCEPTION));
        let PyRep::Tuple(inner) = &parts[2] else {
            panic!("expected inner tuple");
        };
        let PyRep::Bytes(raw) = &inner[0] else {
            panic!("expected marshaled exception bytes");
        };
        let decoded = eve_proto::marshal::decode(raw).expect("valid marshal bytes");
        let PyRep::ObjectEx { header, .. } = decoded else {
            panic!("expected ObjectEx UserError");
        };
        let PyRep::Tuple(h_parts) = *header else {
            panic!("expected ObjectEx header tuple");
        };
        assert_eq!(h_parts[0], PyRep::Token("eveexceptions.UserError".into()));
        let PyRep::Tuple(u_args) = &h_parts[1] else {
            panic!("expected UserError args tuple");
        };
        assert_eq!(u_args[0].as_str(), Some("UnMachoDestination"));
    }

    #[test]
    fn corp_info_keyval_normalizes_to_util_row_for_live_client() {
        let kv = PyRep::KeyVal(
            [
                ("corporationID".to_string(), PyRep::Integer(98_100_001)),
                (
                    "corporationName".to_string(),
                    PyRep::String("Demo Corp".into()),
                ),
            ]
            .into_iter()
            .collect(),
        );
        for svc in ["corpRegistry", "machoNet"] {
            let res = normalize_macho_rpc_reply(svc, "GetCorporation", Ok(kv.clone())).unwrap();
            let PyRep::Object { type_name, args } = res else {
                panic!("expected util.Row PyRep::Object for {svc}");
            };
            assert_eq!(type_name, "util.Row");
            let PyRep::Tuple(state) = *args else {
                panic!("expected 2-tuple (header, line)");
            };
            let (PyRep::List(hdr), PyRep::List(row)) = (&state[0], &state[1]) else {
                panic!("expected header and line lists");
            };
            let pairs: HashMap<&str, &PyRep> = hdr
                .iter()
                .zip(row.iter())
                .map(|(k, v)| (k.as_str().unwrap(), v))
                .collect();
            assert_eq!(pairs.get("corporationID"), Some(&&PyRep::Integer(98_100_001)));
            assert_eq!(
                pairs.get("corporationName"),
                Some(&&PyRep::String("Demo Corp".into()))
            );
        }
    }

    #[test]
    fn market_transactions_normalize_to_crowset_with_float_prices_for_live_client() {
        let rowset = PyRep::rowset(
            vec![
                "transactionID",
                "transactionDate",
                "typeID",
                "quantity",
                "price",
                "stationID",
                "buyerID",
                "sellerID",
                "accountID",
                "buyerAccountID",
                "sellerAccountID",
                "clientID",
                "credit",
                "journalRefID",
                "journalTransactionID",
            ],
            vec![vec![
                PyRep::Integer(9001),
                PyRep::Integer(134_384_360_643_585_500),
                PyRep::Integer(34),
                PyRep::Integer(100),
                PyRep::Real(5.25),
                PyRep::Integer(60_003_760),
                PyRep::Integer(90_000_001),
                PyRep::Integer(90_000_002),
                PyRep::Integer(90_000_001),
                PyRep::Integer(1000),
                PyRep::Integer(1000),
                PyRep::Integer(90_000_002),
                PyRep::Real(-525.0),
                PyRep::Integer(777),
                PyRep::Integer(777),
            ]],
        );
        for method in ["CharGetTransactions", "CorpGetTransactions"] {
            let res = normalize_macho_rpc_reply("marketProxy", method, Ok(rowset.clone())).unwrap();
            let PyRep::ObjectEx {
                header,
                list,
                ..
            } = res
            else {
                panic!("expected CRowset ObjectEx for {method}");
            };
            let PyRep::Tuple(h_parts) = *header else {
                panic!("expected header tuple");
            };
            let PyRep::Tuple(class_parts) = &h_parts[0] else {
                panic!("expected class tuple");
            };
            assert_eq!(
                class_parts[0],
                PyRep::Token("carbon.common.script.sys.crowset.CRowset".into())
            );
            assert_eq!(list.len(), 1);
            let PyRep::DBRow { columns, .. } = &list[0] else {
                panic!("expected packed blue.DBRow inside CRowset");
            };
            let by_col: HashMap<&str, &PyRep> =
                columns.iter().map(|(k, v)| (k.as_str(), v)).collect();
            assert_eq!(by_col.get("transactionID"), Some(&&PyRep::Integer(9001)));
            assert_eq!(by_col.get("typeID"), Some(&&PyRep::Integer(34)));
            assert_eq!(by_col.get("quantity"), Some(&&PyRep::Integer(100)));
            assert_eq!(by_col.get("price"), Some(&&PyRep::Real(5.25)));
            assert_eq!(by_col.get("credit"), Some(&&PyRep::Real(-525.0)));
        }
        // Negative / non-rowset passthrough
        assert_eq!(
            normalize_macho_rpc_reply("marketProxy", "CharGetTransactions", Ok(PyRep::None))
                .unwrap(),
            PyRep::None
        );
    }

    #[test]
    fn facility_replies_normalize_activities_to_dict_of_six_tuples_for_live_client() {
        let facility_kv = PyRep::keyval([
            ("facilityID", PyRep::Integer(60_003_760)),
            ("typeID", PyRep::Integer(35835)),
            ("ownerID", PyRep::Integer(98_000_001)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("online", PyRep::Bool(true)),
            ("tax", PyRep::Real(0.01)),
            (
                "activities",
                PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(3), PyRep::Integer(9)]),
            ),
        ]);
        let list_res = normalize_macho_rpc_reply(
            "facilityManager",
            "GetFacilities",
            Ok(PyRep::List(vec![facility_kv.clone()])),
        )
        .unwrap();
        let PyRep::List(items) = list_res else {
            panic!("expected List");
        };
        assert_eq!(items.len(), 1);
        let single_res =
            normalize_macho_rpc_reply("facilityManager", "GetFacility", Ok(facility_kv)).unwrap();
        for fac in [&items[0], &single_res] {
            assert!(matches!(fac.dict_get("rigModifiers"), Some(PyRep::Dict(_))));
            assert!(matches!(fac.dict_get("serviceAccess"), Some(PyRep::Dict(_))));
            assert_eq!(fac.dict_get("sccTaxModifier"), Some(&PyRep::Real(1.0)));
            assert!(matches!(fac.dict_get("globalModifiers"), Some(PyRep::Dict(_))));
            let Some(PyRep::Dict(acts)) = fac.dict_get("activities") else {
                panic!("expected activities dict on live client");
            };
            assert_eq!(acts.len(), 3);
            for (k, v) in acts {
                assert!(k.as_i64().is_some());
                let PyRep::Tuple(six) = v else {
                    panic!("expected 6-tuple per activity");
                };
                assert_eq!(six.len(), 6);
                let PyRep::List(cats) = &six[3] else {
                    panic!("expected category list");
                };
                assert!(!cats.is_empty());
            }
        }
        // Negative / non-KeyVal passthrough
        assert_eq!(
            normalize_macho_rpc_reply("facilityManager", "GetFacility", Ok(PyRep::None)).unwrap(),
            PyRep::None
        );
    }

    #[test]
    fn incursion_global_report_normalizes_state_to_integer_and_required_fields() {
        let input = PyRep::List(vec![
            PyRep::keyval([
                ("taleID", PyRep::Integer(1)),
                ("state", PyRep::String("Withdrawing".into())),
                ("hasBoss", PyRep::Bool(false)),
            ]),
            PyRep::keyval([
                ("taleID", PyRep::Integer(2)),
                ("state", PyRep::String("Mobilizing".into())),
                ("hasBoss", PyRep::Bool(false)),
            ]),
            PyRep::keyval([
                ("taleID", PyRep::Integer(3)),
                ("state", PyRep::String("Establishing".into())),
                ("hasBoss", PyRep::Bool(true)),
            ]),
        ]);
        let res = normalize_macho_rpc_reply("map", "GetIncursionGlobalReport", Ok(input)).unwrap();
        let PyRep::List(rows) = res else {
            panic!("expected List");
        };
        assert_eq!(rows[0].dict_get("state"), Some(&PyRep::Integer(0)));
        assert_eq!(rows[1].dict_get("state"), Some(&PyRep::Integer(1)));
        assert_eq!(rows[2].dict_get("state"), Some(&PyRep::Integer(2)));
        assert_eq!(rows[2].dict_get("aggressorFactionID"), Some(&PyRep::Integer(500_019)));
        assert_eq!(rows[2].dict_get("hasFinalEncounter"), Some(&PyRep::Bool(true)));
        assert_eq!(rows[2].dict_get("effects"), Some(&PyRep::List(Vec::new())));
    }

    #[test]
    fn signature_instances_normalize_nested_dungeon_map_to_system_counts() {
        let input = PyRep::Dict(vec![
            (
                PyRep::Integer(30_000_142),
                PyRep::Dict(vec![
                    (
                        PyRep::Integer(100),
                        PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                    ),
                    (PyRep::Integer(101), PyRep::List(vec![PyRep::Integer(3)])),
                ]),
            ),
            (PyRep::Integer(30_002_187), PyRep::Dict(Vec::new())),
        ]);
        let res = normalize_macho_rpc_reply(
            "dungeonInstanceCacheMgr",
            "GetSignatureInstances",
            Ok(input),
        )
        .unwrap();
        let PyRep::Dict(entries) = res else {
            panic!("expected Dict");
        };
        let by_sys: HashMap<i64, i64> = entries
            .into_iter()
            .filter_map(|(k, v)| Some((k.as_i64()?, v.as_i64()?)))
            .collect();
        assert_eq!(by_sys.get(&30_000_142), Some(&3));
        assert_eq!(by_sys.get(&30_002_187), Some(&0));
    }
}
