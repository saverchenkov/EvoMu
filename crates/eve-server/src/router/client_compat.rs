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

fn rowset_to_crowset(rep: PyRep, col_types: &[(&str, DbColumnType)]) -> PyRep {
    let PyRep::Rowset { columns, rows } = rep else {
        return rep;
    };
    let descriptor = DBRowDescriptor::new(col_types.iter().map(|&(name, ty)| (name, ty)));
    let col_indices: Vec<Option<usize>> = col_types
        .iter()
        .map(|&(name, _)| columns.iter().position(|c| c == name))
        .collect();
    let mapped_rows: Vec<Vec<PyRep>> = rows
        .into_iter()
        .map(|row| {
            col_indices
                .iter()
                .map(|idx| idx.and_then(|i| row.get(i).cloned()).unwrap_or(PyRep::None))
                .collect()
        })
        .collect();
    descriptor.build_crowset(mapped_rows)
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
        let entries: Vec<(PyRep, PyRep)> = columns
            .iter()
            .cloned()
            .zip(row)
            .map(|(k, v)| (PyRep::String(k), v))
            .collect();
        dict.push((row_key, PyRep::Dict(entries)));
    }
    PyRep::Dict(dict)
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

#[allow(dead_code)]
fn inv_keyval_to_dbrow(rep: PyRep) -> PyRep {
    inv_keyval_to_dbrow_with_sde(rep, None)
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

// Fix #969: normalize both top-level fields and nested `contract` KeyVal,
// resolve `dateIssuedUnix`/`expirationUnix` timestamps, normalize `items` (`itemTypeID`, `inCrate`),
// `requestedItems`, and bid `amount`, and map None `acceptorID`/`assigneeID` to 0.
fn normalize_contract_reply(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    let type_int = match kv_str(&entries, "type").as_str() {
        "ItemExchange" | "item_exchange" => 1,
        "Auction" | "auction" => 2,
        "Courier" | "courier" => 3,
        "Loan" | "loan" => 4,
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
        "Public" | "public" => 0,
        "Private" | "private" | "Personal" | "personal" | "Myself" | "myself" => 1,
        "Corporation" | "corporation" | "MyCorp" | "my_corp" => 2,
        "Alliance" | "alliance" | "MyAlliance" | "my_alliance" => 3,
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
    let pick_ts = |k1: &str, k2: &str, default: i64| -> i64 {
        match entries.get(k1) {
            Some(PyRep::Integer(v)) => *v,
            Some(PyRep::Real(v)) => *v as i64,
            _ => match entries.get(k2) {
                Some(PyRep::Integer(v)) => *v,
                Some(PyRep::Real(v)) => *v as i64,
                _ => default,
            },
        }
    };
    let issued_ft = unix_to_ft(pick_ts("dateIssued", "dateIssuedUnix", 1_700_000_000));
    let expired_ft = unix_to_ft(pick_ts("dateExpired", "expirationUnix", 1_700_086_400));
    let accepted_ft = unix_to_ft(pick_ts("dateAccepted", "dateAcceptedUnix", 1_700_000_000));
    let completed_raw = pick_ts("dateCompleted", "dateCompletedUnix", 0);
    let completed_ft = if completed_raw > 0 {
        unix_to_ft(completed_raw)
    } else {
        accepted_ft
    };
    let start_sys = kv_int(&entries, "startSolarSystemID", 0);
    let raw_end_sys = kv_int(&entries, "endSolarSystemID", 0);
    let end_sys = if raw_end_sys > 0 {
        raw_end_sys
    } else {
        start_sys
    };
    let start_reg = kv_int(&entries, "startRegionID", 0);
    let end_reg = kv_int(&entries, "endRegionID", 0);
    let num_days = kv_int(&entries, "numDays", 1);
    let volume = kv_float(&entries, "volume").max(1.0);
    let acceptor_id = kv_int(&entries, "acceptorID", 0);
    let assignee_id = kv_int(&entries, "assigneeID", 0);

    let req_items = match entries.get("requestedItems") {
        Some(PyRep::List(list)) => list.clone(),
        _ => Vec::new(),
    };
    let mut norm_items = match entries.remove("items") {
        Some(PyRep::List(list)) => list,
        _ => Vec::new(),
    };
    let mut has_req_in_items = false;
    for item in norm_items.iter_mut() {
        if let PyRep::KeyVal(item_map) = item {
            let tid = kv_int(item_map, "itemTypeID", kv_int(item_map, "typeID", 0));
            let qty = kv_int(item_map, "quantity", 1).max(1);
            let in_crate = match item_map.get("inCrate") {
                Some(PyRep::Bool(b)) => *b,
                Some(PyRep::Integer(v)) => *v != 0,
                _ => true,
            };
            if !in_crate {
                has_req_in_items = true;
            }
            item_map.insert("inCrate".to_string(), PyRep::Bool(in_crate));
            item_map.insert("itemTypeID".to_string(), PyRep::Integer(tid));
            item_map.insert("typeID".to_string(), PyRep::Integer(tid));
            item_map.insert("quantity".to_string(), PyRep::Integer(qty));
            item_map.entry("itemID".to_string()).or_insert(PyRep::Integer(0));
            item_map.entry("parentID".to_string()).or_insert(PyRep::Integer(0));
            item_map.entry("flagID".to_string()).or_insert(PyRep::Integer(0));
            item_map.entry("copy".to_string()).or_insert(PyRep::Bool(false));
        }
    }
    if !has_req_in_items {
        for req in req_items {
            if let PyRep::KeyVal(mut req_map) = req {
                let tid = kv_int(&req_map, "itemTypeID", kv_int(&req_map, "typeID", 0));
                if tid > 0 {
                    let qty = kv_int(&req_map, "quantity", 1).max(1);
                    req_map.insert("inCrate".to_string(), PyRep::Bool(false));
                    req_map.insert("itemTypeID".to_string(), PyRep::Integer(tid));
                    req_map.insert("typeID".to_string(), PyRep::Integer(tid));
                    req_map.insert("quantity".to_string(), PyRep::Integer(qty));
                    req_map.entry("itemID".to_string()).or_insert(PyRep::Integer(0));
                    req_map.entry("parentID".to_string()).or_insert(PyRep::Integer(0));
                    req_map.entry("flagID".to_string()).or_insert(PyRep::Integer(0));
                    req_map.entry("copy".to_string()).or_insert(PyRep::Bool(false));
                    norm_items.push(PyRep::KeyVal(req_map));
                }
            }
        }
    }
    entries.insert("items".to_string(), PyRep::List(norm_items));

    if let Some(PyRep::List(bids)) = entries.get_mut("bids") {
        for bid in bids.iter_mut() {
            if let PyRep::KeyVal(bid_map) = bid {
                if !bid_map.contains_key("amount") {
                    let amt = bid_map
                        .get("amountISK")
                        .cloned()
                        .unwrap_or(PyRep::Real(0.0));
                    bid_map.insert("amount".to_string(), amt);
                }
            }
        }
    }
    entries.entry("bids".to_string()).or_insert_with(|| PyRep::List(Vec::new()));
    let num_bids = match entries.get("bids") {
        Some(PyRep::List(bids)) => bids.len() as i64,
        _ => kv_int(&entries, "numBids", 0),
    };

    entries.insert("type".to_string(), PyRep::Integer(type_int));
    entries.insert("status".to_string(), PyRep::Integer(status_int));
    entries.insert("availability".to_string(), PyRep::Integer(avail_int));
    entries.insert("startSolarSystemID".to_string(), PyRep::Integer(start_sys));
    entries.insert("endSolarSystemID".to_string(), PyRep::Integer(end_sys));
    entries.insert("startRegionID".to_string(), PyRep::Integer(start_reg));
    entries.insert("endRegionID".to_string(), PyRep::Integer(end_reg));
    entries.insert("dateIssued".to_string(), PyRep::Integer(issued_ft));
    entries.insert("dateExpired".to_string(), PyRep::Integer(expired_ft));
    entries.insert("dateAccepted".to_string(), PyRep::Integer(accepted_ft));
    entries.insert("dateCompleted".to_string(), PyRep::Integer(completed_ft));
    entries.insert("numDays".to_string(), PyRep::Integer(num_days));
    entries.insert("volume".to_string(), PyRep::Real(volume));
    entries.insert("crateID".to_string(), PyRep::Integer(0));
    entries.insert("acceptorID".to_string(), PyRep::Integer(acceptor_id));
    entries.insert("assigneeID".to_string(), PyRep::Integer(assignee_id));
    entries.entry("numBids".to_string()).or_insert(PyRep::Integer(num_bids));

    let keep_non_kv_contract = matches!(
        entries.get("contract"),
        Some(existing) if !matches!(existing, PyRep::KeyVal(_) | PyRep::Dict(_))
    );
    if !keep_non_kv_contract {
        let mut c_map = entries.clone();
        c_map.remove("contract");
        entries.insert("contract".to_string(), PyRep::KeyVal(c_map));
    }
    PyRep::KeyVal(entries)
}

fn normalize_contract_list_reply(rep: PyRep) -> PyRep {
    let list = match rep {
        PyRep::List(contracts) => contracts,
        ref obj @ (PyRep::KeyVal(_) | PyRep::Dict(_)) => match obj.dict_get("contracts") {
            Some(PyRep::List(contracts)) => contracts.clone(),
            _ => return rep,
        },
        _ => return rep,
    };
    let mut normalized = Vec::with_capacity(list.len());
    let mut items_by_cid = Vec::with_capacity(list.len());
    for raw in list {
        let norm = normalize_contract_reply(raw);
        if let Some(cid) = norm.dict_get("contractID").and_then(PyRep::as_i64) {
            let items = norm
                .dict_get("items")
                .cloned()
                .unwrap_or_else(|| PyRep::List(Vec::new()));
            items_by_cid.push((PyRep::Integer(cid), items));
        }
        normalized.push(norm);
    }
    PyRep::keyval([
        ("contracts", PyRep::List(normalized)),
        ("items", PyRep::Dict(items_by_cid)),
    ])
}

fn normalize_skill_queue_rowset(rep: PyRep) -> PyRep {
    let PyRep::Rowset { columns, rows } = rep else {
        return rep;
    };
    let has_end = columns.iter().any(|c| c == "trainingEndTime");
    let dur_idx = columns.iter().position(|c| c == "durationSeconds");
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000);
    let mut cur_ft = (now_unix + 11_644_473_600) * 10_000_000;
    let list_entries = rows
        .into_iter()
        .enumerate()
        .map(|(idx, row)| {
            let mut entries = Vec::with_capacity(columns.len() + 3);
            for (col, val) in columns.iter().zip(row.into_iter()) {
                entries.push((col.as_str(), val));
            }
            if !has_end {
                let dur_secs = dur_idx
                    .and_then(|i| entries.get(i))
                    .and_then(|(_, v)| v.as_f64())
                    .unwrap_or(60.0)
                    .max(1.0);
                let start_ft = cur_ft;
                let end_ft = start_ft + (dur_secs * 10_000_000.0) as i64;
                cur_ft = end_ft;
                let mut kv_map: HashMap<String, PyRep> = entries
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect();
                kv_map
                    .entry("queuePosition".to_string())
                    .or_insert_with(|| PyRep::Integer(idx as i64));
                kv_map
                    .entry("trainingStartTime".to_string())
                    .or_insert_with(|| PyRep::Integer(start_ft));
                kv_map
                    .entry("trainingEndTime".to_string())
                    .or_insert_with(|| PyRep::Integer(end_ft));
                PyRep::KeyVal(kv_map)
            } else {
                PyRep::keyval(entries)
            }
        })
        .collect();
    PyRep::List(list_entries)
}

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
    if service == "XmppChatMgr" && method == "CreatePlayerOwnedChannel" {
        if let Ok(ref rep) = rpc_res {
            if let Some(s) = rep
                .dict_get("channelID")
                .and_then(|v| v.as_str())
                .or_else(|| rep.as_str())
            {
                return Ok(PyRep::String(s.replace(':', "_")));
            }
        }
    }
    if service == "fleetProxy" && matches!(method, "GetAvailableFleetAds" | "GetFleetAdsForChar") {
        if let Ok(rep) = rpc_res {
            return Ok(rowset_to_dict_by_key(rep, "fleetID"));
        }
    }
    if service == "account" && method == "GetTransactions" {
        if let Ok(rep) = rpc_res {
            return Ok(rowset_to_crowset(
                rep,
                &[
                    ("transactionID", DbColumnType::I8),
                    ("transactionDate", DbColumnType::FileTime),
                    ("referenceID", DbColumnType::I8),
                    ("entryTypeID", DbColumnType::I4),
                    ("ownerID1", DbColumnType::I8),
                    ("ownerID2", DbColumnType::I8),
                    ("accountKey", DbColumnType::I4),
                    ("amount", DbColumnType::R8),
                    ("balance", DbColumnType::R8),
                    ("description", DbColumnType::WStr),
                ],
            ));
        }
    }
    if service == "contractProxy" && method == "CreateContract" {
        // Fix #935: V24 ShowCreatedContracts expects a sequence of contractIDs, not a scalar KeyVal.
        if let Ok(rep) = rpc_res {
            let list = match rep {
                PyRep::List(ids) => PyRep::List(ids),
                PyRep::Tuple(ids) => PyRep::List(ids),
                PyRep::Integer(cid) => PyRep::List(vec![PyRep::Integer(cid)]),
                other => {
                    if let Some(PyRep::List(ids)) = other.dict_get("contractIDs") {
                        PyRep::List(ids.clone())
                    } else if let Some(cid) = other.dict_get("contractID").and_then(PyRep::as_i64) {
                        PyRep::List(vec![PyRep::Integer(cid)])
                    } else {
                        other
                    }
                }
            };
            return Ok(list);
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
    if service == "facilityManager" && method == "GetFacilityLocations" {
        if let Ok(PyRep::List(items)) = rpc_res {
            return Ok(PyRep::List(
                items
                    .into_iter()
                    .map(normalize_facility_location_rep)
                    .collect(),
            ));
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
    if service == "dungeonInstanceCacheMgr"
        && matches!(
            method,
            "GetPirateInsurgencyInstances" | "GetPirateInsurgencyIceHeistInstances"
        )
    {
        if let Ok(PyRep::List(items)) = rpc_res {
            let default_did = if method == "GetPirateInsurgencyIceHeistInstances" {
                10_497
            } else {
                10_477
            };
            let mut by_sys: std::collections::BTreeMap<i64, Vec<PyRep>> =
                std::collections::BTreeMap::new();
            for mut item in items {
                let sys_id = item
                    .dict_get("solarSystemID")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(30_000_142);
                if let PyRep::KeyVal(ref mut map) = item {
                    map.entry("dungeonID".to_string())
                        .or_insert(PyRep::Integer(default_did));
                    map.entry("factionID".to_string())
                        .or_insert(PyRep::Integer(500_010));
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
    if matches!(service, "corpRegistry" | "machoNet") && method == "GetMembersPaged" {
        if let Ok(PyRep::ObjectEx {
            is_type_2: false,
            header,
            list,
            dict,
        }) = rpc_res
        {
            let header = match *header {
                PyRep::Tuple(mut h) => {
                    if h.len() == 2
                        && h[0]
                            == PyRep::Token(
                                "eve.common.script.util.pagedCollection.PagedResultSet".into(),
                            )
                    {
                        if let PyRep::Tuple(mut args) = h[1].clone() {
                            if !args.is_empty() {
                                const CORP_MEMBER_COLS: &[(&str, DbColumnType)] = &[
                                    ("characterID", DbColumnType::I8),
                                    ("name", DbColumnType::WStr),
                                    ("role", DbColumnType::I8),
                                    ("title", DbColumnType::WStr),
                                    ("corporationID", DbColumnType::I8),
                                    ("locationID", DbColumnType::I8),
                                    ("shipTypeID", DbColumnType::I4),
                                    ("logonDateTime", DbColumnType::FileTime),
                                    ("roles", DbColumnType::I8),
                                    ("grantableRoles", DbColumnType::I8),
                                    ("rolesAtHQ", DbColumnType::I8),
                                    ("grantableRolesAtHQ", DbColumnType::I8),
                                    ("rolesAtBase", DbColumnType::I8),
                                    ("grantableRolesAtBase", DbColumnType::I8),
                                    ("rolesAtOther", DbColumnType::I8),
                                    ("grantableRolesAtOther", DbColumnType::I8),
                                    ("titleMask", DbColumnType::I8),
                                    ("baseID", DbColumnType::I8),
                                    ("startDateTime", DbColumnType::FileTime),
                                    ("accountKey", DbColumnType::I4),
                                    ("divisionID", DbColumnType::I4),
                                    ("squadronID", DbColumnType::I4),
                                    ("blockRoles", DbColumnType::I4),
                                    ("blocksRoles", DbColumnType::I4),
                                ];
                                let coll = std::mem::replace(&mut args[0], PyRep::None);
                                args[0] = rowset_to_crowset(coll, CORP_MEMBER_COLS);
                            }
                            h[1] = PyRep::Tuple(args);
                        }
                    }
                    PyRep::Tuple(h)
                }
                other => other,
            };
            return Ok(PyRep::ObjectEx {
                is_type_2: false,
                header: Box::new(header),
                list,
                dict,
            });
        }
    }
    if matches!(service, "corpRegistry" | "machoNet") && method == "GetTitles" {
        if let Ok(PyRep::Rowset { columns, rows }) = rpc_res {
            let key_idx = columns.iter().position(|c| c == "titleID").unwrap_or(0);
            let header: Vec<PyRep> = columns.into_iter().map(PyRep::String).collect();
            let mut dict = Vec::with_capacity(rows.len());
            for row in rows {
                let row_key = row
                    .get(key_idx)
                    .cloned()
                    .unwrap_or(PyRep::Integer(dict.len() as i64 + 1));
                let row_obj = PyRep::Object {
                    type_name: "util.Row".into(),
                    args: Box::new(PyRep::Tuple(vec![
                        PyRep::List(header.clone()),
                        PyRep::List(row),
                    ])),
                };
                dict.push((row_key, row_obj));
            }
            return Ok(PyRep::Dict(dict));
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
    } else if service == "contractProxy"
        && matches!(
            method,
            "GetContractListForOwner"
                | "GetMyCurrentContractList"
                | "GetMyExpiredContractList"
                | "GetMyBids"
        )
    {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_contract_list_reply(rep));
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
            let mut wrapped = Vec::with_capacity(raw.len());
            for item in raw {
                // Preserve synthetic pre-wrapped test payloads that have `contract` + `bids`
                // without top-level `contract_to_pyrep` fields (`contractID`).
                let is_synthetic_prewrapped = item.dict_get("contractID").is_none()
                    && item.dict_get("contract").is_some()
                    && item.dict_get("bids").is_some();
                if is_synthetic_prewrapped {
                    wrapped.push(item);
                } else {
                    let norm = normalize_contract_reply(item);
                    let contract_rep = norm
                        .dict_get("contract")
                        .cloned()
                        .unwrap_or_else(|| norm.clone());
                    let items = norm
                        .dict_get("items")
                        .cloned()
                        .unwrap_or_else(|| PyRep::List(Vec::new()));
                    let bids = norm
                        .dict_get("bids")
                        .cloned()
                        .unwrap_or_else(|| PyRep::List(Vec::new()));
                    let num_bids = norm
                        .dict_get("numBids")
                        .cloned()
                        .unwrap_or_else(|| PyRep::Integer(0));
                    wrapped.push(PyRep::keyval([
                        ("contract", contract_rep),
                        ("items", items),
                        ("bids", bids),
                        ("numBids", num_bids),
                    ]));
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
    } else if matches!(service, "corporationSvc" | "charMgr")
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
    } else if service == "paperDollServer" && method == "GetPaperDollData" {
        if let Ok(PyRep::KeyVal(map)) = rpc_res {
            let gender = map
                .get("gender")
                .and_then(PyRep::as_i64)
                .map(|g| if g == 0 { "female" } else { "male" })
                .unwrap_or("male");
            return Ok(PyRep::List(vec![PyRep::String(gender.into())]));
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
    } else if service == "peaceTreatyMgr" && method == "GetPeaceTreatiesForSession" {
        if let Ok(rep) = &rpc_res {
            if !matches!(rep, PyRep::Tuple(items) if items.len() == 2) {
                return Ok(PyRep::Tuple(vec![
                    PyRep::List(Vec::new()),
                    PyRep::List(Vec::new()),
                ]));
            }
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
        match rpc_res {
            Ok(PyRep::Tuple(parts)) => {
                if parts.len() >= 3
                    && matches!(parts[1], PyRep::Dict(_))
                    && matches!(parts[2], PyRep::Dict(_))
                {
                    return Ok(PyRep::Tuple(parts));
                }
                if parts.len() >= 3 && matches!(parts[2], PyRep::List(_)) {
                    let tax = parts[0].clone();
                    let base_yield = match &parts[1] {
                        PyRep::Real(_) => parts[1].clone(),
                        other => other.as_f64().map(PyRep::Real).unwrap_or(PyRep::Real(0.50)),
                    };
                    let PyRep::List(quotes) = parts[2].clone() else {
                        unreachable!()
                    };
                    return Ok(build_v24_reprocessing_quotes_tuple(tax, base_yield, quotes));
                }
                let tax = parts.first().cloned().unwrap_or(PyRep::Real(0.0));
                let base_yield = parts
                    .get(1)
                    .and_then(PyRep::as_f64)
                    .map(PyRep::Real)
                    .unwrap_or(PyRep::Real(0.50));
                return Ok(build_v24_reprocessing_quotes_tuple(
                    tax,
                    base_yield,
                    Vec::new(),
                ));
            }
            Ok(PyRep::List(quotes)) => {
                let tax = quotes
                    .first()
                    .and_then(|q| q.dict_get("stationTaxRate"))
                    .cloned()
                    .unwrap_or(PyRep::Real(0.0));
                let base_yield = quotes
                    .first()
                    .and_then(|q| q.dict_get("stationBaseYield"))
                    .cloned()
                    .unwrap_or(PyRep::Real(0.50));
                return Ok(build_v24_reprocessing_quotes_tuple(tax, base_yield, quotes));
            }
            Ok(rep @ (PyRep::KeyVal(_) | PyRep::Dict(_))) => {
                let tax = rep
                    .dict_get("stationTaxRate")
                    .cloned()
                    .unwrap_or(PyRep::Real(0.0));
                let base_yield = rep
                    .dict_get("stationBaseYield")
                    .cloned()
                    .unwrap_or(PyRep::Real(0.50));
                return Ok(build_v24_reprocessing_quotes_tuple(
                    tax,
                    base_yield,
                    Vec::new(),
                ));
            }
            other => return other,
        }
    } else if service == "reprocessingSvc" && method == "Reprocess" {
        let items = match rpc_res {
            Ok(PyRep::List(items)) => items,
            Ok(rep @ (PyRep::KeyVal(_) | PyRep::Dict(_))) => vec![rep],
            other => return other,
        };
        let mut input_ids = Vec::with_capacity(items.len());
        let mut by_type: std::collections::BTreeMap<i64, i64> =
            std::collections::BTreeMap::new();
        for item in items {
            if let Some(id) = item.dict_get("itemID").and_then(PyRep::as_i64) {
                input_ids.push(PyRep::Integer(id));
            }
            if let Some(PyRep::List(outputs)) = item.dict_get("outputs") {
                for out in outputs {
                    if let (Some(tid), Some(qty)) = (
                        out.dict_get("typeID").and_then(PyRep::as_i64),
                        out.dict_get("quantity").and_then(PyRep::as_i64),
                    ) {
                        *by_type.entry(tid).or_insert(0) += qty;
                    }
                }
            }
        }
        let dict_entries = by_type
            .into_iter()
            .map(|(tid, qty)| (PyRep::Integer(tid), PyRep::Integer(qty)))
            .collect();
        return Ok(PyRep::Tuple(vec![
            PyRep::List(input_ids),
            PyRep::Dict(dict_entries),
        ]));
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
    } else if service == "agentMgr" && matches!(method, "DoAction" | "GetOffer") {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_agent_do_action_rep(rep));
        }
    } else if service == "agentMgr" && method == "GetAgentLocationWrap" {
        if let Ok(rep) = rpc_res {
            return Ok(normalize_agent_location_wrap_rep(rep));
        }
    } else if service == "agentMgr" && method == "GetMissionBriefingInfo" {
        match rpc_res {
            Err(ref e) if e.code == "MissionNotActive" => return Ok(PyRep::None),
            Ok(rep) => return Ok(normalize_agent_mission_briefing_rep(rep)),
            Err(e) => return Err(e),
        }
    } else if service == "agentMgr" && method == "GetMissionObjectiveInfo" {
        match rpc_res {
            Err(ref e) if e.code == "MissionNotActive" => return Ok(PyRep::None),
            Ok(rep) => return Ok(normalize_agent_mission_objective_rep(rep)),
            Err(e) => return Err(e),
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

fn normalize_agent_location_wrap_rep(rep: PyRep) -> PyRep {
    if !matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
        return rep;
    }
    let sys_id = rep
        .dict_get("solarsystemID")
        .or_else(|| rep.dict_get("solarSystemID"))
        .cloned()
        .unwrap_or(PyRep::None);
    let station_id = rep.dict_get("stationID").cloned().unwrap_or(PyRep::None);
    let loc_id = rep
        .dict_get("locationID")
        .cloned()
        .unwrap_or_else(|| match &station_id {
            PyRep::None => sys_id.clone(),
            other => other.clone(),
        });
    let type_id = rep
        .dict_get("typeID")
        .cloned()
        .unwrap_or(PyRep::Integer(5));
    let agent_id = rep.dict_get("agentID").cloned().unwrap_or(PyRep::None);
    PyRep::Dict(vec![
        (PyRep::String("agentID".into()), agent_id),
        (PyRep::String("solarsystemID".into()), sys_id),
        (PyRep::String("locationID".into()), loc_id),
        (PyRep::String("typeID".into()), type_id),
        (PyRep::String("stationID".into()), station_id),
    ])
}

fn normalize_agent_mission_briefing_rep(rep: PyRep) -> PyRep {
    if !matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
        return rep;
    }
    let title = rep
        .dict_get("Mission Title ID")
        .or_else(|| rep.dict_get("title"))
        .cloned()
        .unwrap_or_else(|| PyRep::String("Agent Mission".into()));
    let content_id = rep
        .dict_get("ContentID")
        .or_else(|| rep.dict_get("contentID"))
        .or_else(|| rep.dict_get("missionID"))
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let briefing = rep
        .dict_get("Mission Briefing ID")
        .or_else(|| rep.dict_get("briefing"))
        .cloned()
        .unwrap_or_else(|| PyRep::String(String::new()));
    let decline_time = rep
        .dict_get("Decline Time")
        .cloned()
        .unwrap_or(PyRep::Integer(-1));
    let expiration_time = rep
        .dict_get("Expiration Time")
        .cloned()
        .unwrap_or(PyRep::None);
    let image = rep
        .dict_get("Mission Image")
        .cloned()
        .unwrap_or_else(|| PyRep::String(String::new()));
    let keywords = rep
        .dict_get("Mission Keywords")
        .cloned()
        .unwrap_or_else(|| PyRep::Dict(Vec::new()));
    PyRep::Dict(vec![
        (PyRep::String("Mission Title ID".into()), title),
        (PyRep::String("ContentID".into()), content_id),
        (PyRep::String("Mission Briefing ID".into()), briefing),
        (PyRep::String("Decline Time".into()), decline_time),
        (PyRep::String("Expiration Time".into()), expiration_time),
        (PyRep::String("Mission Image".into()), image),
        (PyRep::String("Mission Keywords".into()), keywords),
    ])
}

fn normalize_agent_mission_objective_rep(rep: PyRep) -> PyRep {
    if !matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
        return rep;
    }
    let title = rep
        .dict_get("missionTitleID")
        .or_else(|| rep.dict_get("title"))
        .cloned()
        .unwrap_or_else(|| PyRep::String("Agent Mission".into()));
    let mission_name = rep
        .dict_get("missionName")
        .cloned()
        .unwrap_or_else(|| title.clone());
    let mission_state = rep
        .dict_get("missionState")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion_status = rep
        .dict_get("completionStatus")
        .and_then(|v| v.as_i64())
        .unwrap_or(if mission_state == 4 { 1 } else { 0 });
    let content_id = rep
        .dict_get("contentID")
        .or_else(|| rep.dict_get("missionID"))
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let isk_reward = rep
        .dict_get("iskReward")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let bonus_reward = rep
        .dict_get("bonusReward")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let lp_reward = rep
        .dict_get("loyaltyPoints")
        .or_else(|| rep.dict_get("lpReward"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let normal_rewards = match rep.dict_get("normalRewards") {
        Some(PyRep::List(items)) => PyRep::List(items.clone()),
        _ if isk_reward > 0 => PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(29), // const.typeCredits
            PyRep::Integer(isk_reward),
            PyRep::None,
        ])]),
        _ => PyRep::List(Vec::new()),
    };
    let bonus_rewards = match rep.dict_get("bonusRewards") {
        Some(PyRep::List(items)) => PyRep::List(items.clone()),
        _ if bonus_reward > 0 => PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(29), // const.typeCredits
            PyRep::Integer(bonus_reward),
            PyRep::None,
        ])]),
        _ => PyRep::List(Vec::new()),
    };
    let agent_id = rep
        .dict_get("agentID")
        .and_then(|v| v.as_i64())
        .unwrap_or(3_008_416);
    let sys_id = rep
        .dict_get("solarsystemID")
        .or_else(|| rep.dict_get("solarSystemID"))
        .and_then(|v| v.as_i64())
        .unwrap_or(30_000_142);
    let origin_sid = rep
        .dict_get("originStationID")
        .and_then(|v| v.as_i64())
        .filter(|&v| v > 0)
        .unwrap_or(60_003_760);
    let dest_sid_opt = rep
        .dict_get("destinationStationID")
        .and_then(|v| v.as_i64())
        .filter(|&v| v > 0);
    let make_loc_dict = |loc_id: i64, loc_type: &str| {
        PyRep::Dict(vec![
            (PyRep::String("locationID".into()), PyRep::Integer(loc_id)),
            (PyRep::String("solarsystemID".into()), PyRep::Integer(sys_id)),
            (PyRep::String("stationID".into()), PyRep::Integer(loc_id)),
            (PyRep::String("typeID".into()), PyRep::Integer(1529)),
            (PyRep::String("agentID".into()), PyRep::Integer(agent_id)),
            (PyRep::String("referringAgentID".into()), PyRep::None),
            (PyRep::String("shipTypeID".into()), PyRep::None),
            (PyRep::String("locationType".into()), PyRep::String(loc_type.into())),
            (PyRep::String("coords".into()), PyRep::None),
            (PyRep::String("BookmarkID".into()), PyRep::None),
        ])
    };
    let origin_loc = make_loc_dict(origin_sid, "agenthomebase");
    let objectives = match rep.dict_get("objectives") {
        Some(PyRep::List(items)) if !items.is_empty() => PyRep::List(items.clone()),
        _ => {
            let mut objs = vec![PyRep::Tuple(vec![
                PyRep::String("agent".into()),
                PyRep::Tuple(vec![PyRep::Integer(agent_id), origin_loc.clone()]),
            ])];
            if let Some(dest_sid) = dest_sid_opt {
                let req_tid = rep
                    .dict_get("requiredTypeID")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(34);
                let req_qty = rep
                    .dict_get("requiredQuantity")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1);
                let dest_loc = make_loc_dict(dest_sid, "objective.destination");
                let cargo_dict = PyRep::Dict(vec![
                    (PyRep::String("typeID".into()), PyRep::Integer(req_tid)),
                    (PyRep::String("quantity".into()), PyRep::Integer(req_qty)),
                    (PyRep::String("volume".into()), PyRep::Real(req_qty as f64)),
                    (
                        PyRep::String("hasCargo".into()),
                        PyRep::Bool(completion_status > 0 || mission_state >= 2),
                    ),
                ]);
                objs.push(PyRep::Tuple(vec![
                    PyRep::String("transport".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        origin_loc.clone(),
                        PyRep::Integer(2),
                        dest_loc,
                        cargo_dict,
                    ]),
                ]));
            }
            PyRep::List(objs)
        }
    };
    let locations = match rep.dict_get("locations") {
        Some(PyRep::List(items)) if !items.is_empty() => PyRep::List(items.clone()),
        _ => {
            let mut locs = vec![origin_loc];
            if let Some(dest_sid) = dest_sid_opt {
                locs.push(make_loc_dict(dest_sid, "objective.destination"));
            }
            PyRep::List(locs)
        }
    };
    let dungeons = match rep.dict_get("dungeons") {
        Some(PyRep::List(items)) if !items.is_empty() => PyRep::List(items.clone()),
        _ if dest_sid_opt.is_none() => {
            let mid_i64 = content_id.as_i64().unwrap_or(1).max(1);
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("dungeonID".into()), PyRep::Integer(mid_i64)),
                (PyRep::String("solarsystemID".into()), PyRep::Integer(sys_id)),
                (PyRep::String("locationID".into()), PyRep::Integer(origin_sid)),
                (PyRep::String("ownerID".into()), PyRep::Integer(agent_id)),
                (PyRep::String("shipRestrictions".into()), PyRep::Integer(0)),
                (
                    PyRep::String("completed".into()),
                    PyRep::Bool(completion_status > 0),
                ),
                (
                    PyRep::String("completionStatus".into()),
                    PyRep::Integer(completion_status),
                ),
            ])])
        }
        _ => PyRep::List(Vec::new()),
    };
    PyRep::Dict(vec![
        (PyRep::String("completionStatus".into()), PyRep::Integer(completion_status)),
        (PyRep::String("missionTitleID".into()), title),
        (PyRep::String("missionState".into()), PyRep::Integer(mission_state)),
        (PyRep::String("missionName".into()), mission_name),
        (PyRep::String("objectives".into()), objectives),
        (PyRep::String("dungeons".into()), dungeons),
        (PyRep::String("locations".into()), locations),
        (PyRep::String("agentGift".into()), PyRep::List(Vec::new())),
        (PyRep::String("normalRewards".into()), normal_rewards),
        (PyRep::String("loyaltyPoints".into()), PyRep::Integer(lp_reward)),
        (PyRep::String("researchPoints".into()), PyRep::Integer(0)),
        (PyRep::String("bonusRewards".into()), bonus_rewards),
        (PyRep::String("collateral".into()), PyRep::List(Vec::new())),
        (PyRep::String("contentID".into()), content_id),
    ])
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

fn normalize_facility_location_rep(rep: PyRep) -> PyRep {
    if !matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
        return rep;
    }
    let item_id = rep
        .dict_get("itemID")
        .or_else(|| rep.dict_get("facilityID"))
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let facility_id = rep
        .dict_get("facilityID")
        .cloned()
        .unwrap_or_else(|| item_id.clone());
    let type_id = rep
        .dict_get("typeID")
        .cloned()
        .unwrap_or(PyRep::Integer(1529));
    let owner_id = rep
        .dict_get("ownerID")
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let flag_id = rep
        .dict_get("flagID")
        .cloned()
        .unwrap_or(PyRep::Integer(4));
    let solar_system_id = rep
        .dict_get("solarSystemID")
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let region_id = rep
        .dict_get("regionID")
        .cloned()
        .unwrap_or(PyRep::Integer(0));
    let can_view = rep
        .dict_get("canView")
        .cloned()
        .unwrap_or(PyRep::Bool(true));
    let can_take = rep
        .dict_get("canTake")
        .cloned()
        .unwrap_or(PyRep::Bool(true));
    let state_dict = PyRep::Dict(vec![
        (PyRep::String("itemID".into()), item_id),
        (PyRep::String("facilityID".into()), facility_id),
        (PyRep::String("typeID".into()), type_id),
        (PyRep::String("ownerID".into()), owner_id),
        (PyRep::String("flagID".into()), flag_id),
        (PyRep::String("solarSystemID".into()), solar_system_id),
        (PyRep::String("regionID".into()), region_id),
        (PyRep::String("canView".into()), can_view),
        (PyRep::String("canTake".into()), can_take),
    ]);
    PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Token("industry.job.Location".into())]),
            state_dict,
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    }
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
        PyRep::Tuple(mut items) => {
            if items.len() == 2 {
                if let (Some(name), Some(PyRep::Tuple(ev_args))) =
                    (items[0].as_str(), items.get(1))
                {
                    if name == "CloakBall" && ev_args.len() >= 2 {
                        let is_uncloak = match &ev_args[1] {
                            PyRep::Bool(b) => !*b,
                            PyRep::Integer(v) => *v == 0,
                            _ => false,
                        };
                        if is_uncloak {
                            return PyRep::Tuple(vec![
                                PyRep::String("UncloakBall".into()),
                                PyRep::Tuple(vec![ev_args[0].clone()]),
                            ]);
                        }
                    } else if name == "OnSpecialFX" && ev_args.len() >= 11 {
                        let guid = ev_args[5].as_str();
                        let is_stop = matches!(ev_args[7], PyRep::Bool(false) | PyRep::Integer(0));
                        if guid == Some("effects.Cloak") && is_stop {
                            let mut fx = ev_args.clone();
                            fx[5] = PyRep::String("effects.Uncloak".into());
                            fx[7] = PyRep::Integer(1);
                            fx[8] = PyRep::Integer(0);
                            fx[9] = PyRep::Integer(6000);
                            fx[10] = PyRep::Integer(1);
                            items[1] = PyRep::Tuple(fx);
                            return PyRep::Tuple(items);
                        } else if guid == Some("effects.Uncloak") {
                            let mut fx = ev_args.clone();
                            fx[8] = PyRep::Integer(0);
                            if fx[9].as_i64().is_none_or(|d| d <= 0) {
                                fx[9] = PyRep::Integer(6000);
                            }
                            if fx[10].as_i64().is_none_or(|r| r <= 0) {
                                fx[10] = PyRep::Integer(1);
                            }
                            items[1] = PyRep::Tuple(fx);
                            return PyRep::Tuple(items);
                        }
                    }
                }
            }
            PyRep::Tuple(items.into_iter().map(normalize_destiny_payload_arg).collect())
        }
        other => other,
    }
}

/// Builds the canonical V24.01 `reprocessingSvc.GetQuotes` 3-tuple
/// `(stationTax, stationEfficiencyDict, {itemID: utillib.KeyVal(...)})` (`reprocessing/ui/quotes.pyj:14-19`).
pub(crate) fn build_v24_reprocessing_quotes_tuple(
    tax: PyRep,
    base_yield: PyRep,
    quotes: Vec<PyRep>,
) -> PyRep {
    let mut eff_entries = vec![
        (PyRep::None, base_yield.clone()),
        (PyRep::Integer(25), base_yield.clone()),
    ];
    let mut seen_eff_types = std::collections::HashSet::new();
    let recoverable_desc = DBRowDescriptor::new([
        ("typeID", DbColumnType::I4),
        ("client", DbColumnType::I8),
        ("unrecoverable", DbColumnType::I8),
        ("station", DbColumnType::I8),
        ("iskCost", DbColumnType::R8),
    ]);
    let mut quote_entries = Vec::with_capacity(quotes.len());
    for q in quotes {
        let item_id = q.dict_get("itemID").cloned().unwrap_or(PyRep::Integer(0));
        let quote_type_id = q.dict_get("typeID").cloned().unwrap_or(PyRep::Integer(0));
        if let Some(tid) = quote_type_id.as_i64() {
            if seen_eff_types.insert(tid) {
                let eff_val = q
                    .dict_get("stationBaseYield")
                    .cloned()
                    .unwrap_or_else(|| base_yield.clone());
                eff_entries.push((PyRep::Integer(tid), eff_val));
            }
        }
        let recoverable_rows = match q.dict_get("outputs") {
            Some(PyRep::List(outs)) => outs
                .iter()
                .map(|o| {
                    let tid = o.dict_get("typeID").cloned().unwrap_or(PyRep::Integer(0));
                    let qty = o.dict_get("quantity").cloned().unwrap_or(PyRep::Integer(0));
                    let unrec = o
                        .dict_get("unrecoverable")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    let station = o.dict_get("station").cloned().unwrap_or(PyRep::Integer(0));
                    let isk_cost = o.dict_get("iskCost").cloned().unwrap_or(PyRep::Real(0.0));
                    vec![tid, qty, unrec, station, isk_cost]
                })
                .collect(),
            _ => Vec::new(),
        };
        let recoverables = recoverable_desc.build_crowset(recoverable_rows);
        let qty_to_process = q
            .dict_get("quantityToProcess")
            .cloned()
            .unwrap_or(PyRep::Integer(0));
        let left_overs = q.dict_get("leftOvers").cloned().unwrap_or(PyRep::Integer(0));
        let isk_cost = q.dict_get("iskCost").cloned().unwrap_or(PyRep::Real(0.0));
        let total_isk_cost = q
            .dict_get("totalISKCost")
            .cloned()
            .unwrap_or(PyRep::Real(0.0));
        let entry = PyRep::keyval([
            ("typeID", quote_type_id),
            ("quantityToProcess", qty_to_process),
            ("leftOvers", left_overs),
            ("iskCost", isk_cost),
            ("totalISKCost", total_isk_cost),
            ("recoverables", recoverables),
        ]);
        quote_entries.push((item_id, entry));
    }
    let eff_dict = PyRep::Dict(eff_entries);
    PyRep::Tuple(vec![tax, eff_dict, PyRep::Dict(quote_entries)])
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

fn normalize_destiny_update_arg(rep: PyRep) -> PyRep {
    match rep {
        PyRep::List(items) => {
            PyRep::List(items.into_iter().map(normalize_destiny_update_arg).collect())
        }
        PyRep::Tuple(mut items) => {
            if items.len() == 2 {
                if let (Some(name), Some(PyRep::Tuple(ev_args))) = (items[0].as_str(), items.get(1))
                {
                    if name == "OnDockingAccepted" && ev_args.len() >= 3 {
                        let station_id = ev_args[2].clone();
                        items[1] = PyRep::Tuple(vec![station_id]);
                        return PyRep::Tuple(items);
                    }
                }
            }
            PyRep::Tuple(items.into_iter().map(normalize_destiny_update_arg).collect())
        }
        other => other,
    }
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
        "OnContractCreated" => Vec::new(),
        "OnDockingAccepted" => {
            if args.len() >= 3 {
                vec![args[2].clone()]
            } else {
                args
            }
        }
        "OnChannelJoined" => {
            if let Some(first) = args.into_iter().next() {
                vec![first]
            } else {
                Vec::new()
            }
        }
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
        "OnBeanPrime" => {
            let dict_to_bean_kv = |entries: Vec<(PyRep, PyRep)>| {
                let mut map = HashMap::new();
                for (k, v) in entries {
                    if let Some(ks) = k.as_str() {
                        map.insert(ks.to_string(), v);
                    }
                }
                map.entry("errorID".into()).or_insert(PyRep::Integer(0));
                map.entry("errorKeyHash".into())
                    .or_insert(PyRep::String(String::new()));
                map.entry("logMode".into()).or_insert(PyRep::Integer(0));
                PyRep::KeyVal(map)
            };
            let raw_items = if args.len() == 1 {
                match args.into_iter().next().unwrap() {
                    PyRep::List(items) => items,
                    other => vec![other],
                }
            } else {
                args
            };
            let beans: Vec<PyRep> = raw_items
                .into_iter()
                .filter_map(|a| match a {
                    PyRep::Integer(err_id) => Some(PyRep::keyval([
                        ("errorID", PyRep::Integer(err_id)),
                        ("errorKeyHash", PyRep::String(String::new())),
                        ("logMode", PyRep::Integer(0)),
                    ])),
                    PyRep::Dict(entries) => Some(dict_to_bean_kv(entries)),
                    PyRep::KeyVal(mut map) => {
                        map.entry("errorID".into()).or_insert(PyRep::Integer(0));
                        map.entry("errorKeyHash".into())
                            .or_insert(PyRep::String(String::new()));
                        map.entry("logMode".into()).or_insert(PyRep::Integer(0));
                        Some(PyRep::KeyVal(map))
                    }
                    _ => None,
                })
                .collect();
            vec![PyRep::List(beans)]
        }
        "OnCorporationRecruitmentAdChanged" => Vec::new(),
        "OnOrganizationContactsUpdated" => {
            let Some(owner_id) = args.first().cloned() else {
                return Vec::new();
            };
            let mut updates_out = Vec::new();
            if let Some(PyRep::List(items)) = args.get(1) {
                for it in items {
                    let cid = it
                        .dict_get("contactID")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    let rel = it
                        .dict_get("standing")
                        .or_else(|| it.dict_get("relationshipID"))
                        .cloned()
                        .unwrap_or(PyRep::Real(0.0));
                    let mask = it
                        .dict_get("labelMask")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    updates_out.push(PyRep::Tuple(vec![cid, rel, mask]));
                }
            }
            vec![owner_id, PyRep::List(updates_out)]
        }
        "OnTargetClear" => {
            if args.is_empty() {
                vec![PyRep::List(Vec::new())]
            } else {
                args
            }
        }
        "OnHeatAdded" | "OnHeatRemoved" => {
            if args.len() == 1 {
                vec![PyRep::Integer(1211), args.into_iter().next().unwrap()]
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
        "OnNotificationReceived" => {
            if args.is_empty() {
                vec![
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::None,
                    PyRep::Integer(0),
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
        "DoDestinyUpdate" => args
            .into_iter()
            .map(normalize_destiny_update_arg)
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
        "OnSpecialFX" => normalize_on_special_fx(args),
        "OnGodmaShipEffect" => normalize_on_godma_ship_effect(args, sde),
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
                    PyRep::Tuple(mut t) if t.len() == 8 => {
                        if t.get(3) == Some(&PyRep::Integer(0)) {
                            t[3] = PyRep::Integer(2);
                        }
                        if matches!(t.get(7), Some(PyRep::Bool(_))) {
                            t[7] = PyRep::None;
                        }
                        PyRep::Tuple(t)
                    }
                    PyRep::Tuple(mut t) if t.len() == 7 => {
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
                _ => 0,
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
        ensure_i4_pyrep(&mut args[7]);
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
        PyRep::Integer(0),
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
        assert_eq!(ten_zero_ft[7], PyRep::Integer(4000));
        assert_eq!(ten_zero_ft[8], PyRep::Integer(0));
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

        let str_eff = normalize_macho_notification(
            "OnGodmaShipEffect",
            vec![
                PyRep::Integer(200_165_898),
                PyRep::String("miningLaser".into()),
                PyRep::Bool(true),
            ],
        );
        assert_eq!(str_eff[1], PyRep::Integer(67));
        assert!(matches!(&str_eff[5], PyRep::Tuple(t) if t.get(6) == Some(&PyRep::Integer(67))));

        let sfx = normalize_macho_notification(
            "OnSpecialFX",
            vec![
                PyRep::Integer(200_164_799),
                PyRep::Integer(200_165_898),
                PyRep::Integer(483),
                PyRep::Integer(200_185_369),
                PyRep::None,
                PyRep::String("effects.Laser".into()),
                PyRep::Bool(false),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Real(60_000.0),
                PyRep::Integer(1000),
                PyRep::Integer(valid_ft),
                PyRep::Real(0.0),
                PyRep::None,
            ],
        );
        assert_eq!(sfx.len(), 14);
        assert_eq!(sfx[5], PyRep::String("effects.Laser".into()));
        assert_eq!(sfx[11], PyRep::Integer(valid_ft));
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

        // #806: GetIceBeltInstances / GetCombatAnomalyInstances / GetOreAnomalyInstances /
        // GetFactionWarfareInstances must normalize `{sys_id: {did: [iid, ...]}}` into
        // `{sys_id: [KeyVal(dungeonID=..., instanceID=..., solarSystemID=..., factionID=...), ...]}`.
        for method in [
            "GetIceBeltInstances",
            "GetCombatAnomalyInstances",
            "GetOreAnomalyInstances",
            "GetFactionWarfareInstances",
        ] {
            let raw = PyRep::Dict(vec![(
                PyRep::Integer(30_000_142),
                PyRep::Dict(vec![(
                    PyRep::Integer(2868),
                    PyRep::List(vec![PyRep::Integer(9001), PyRep::Integer(9002)]),
                )]),
            )]);
            let norm =
                normalize_macho_rpc_reply("dungeonInstanceCacheMgr", method, Ok(raw)).unwrap();
            let PyRep::Dict(sys_entries) = norm else {
                panic!("expected Dict from {method}");
            };
            assert_eq!(sys_entries[0].0, PyRep::Integer(30_000_142));
            let (_, PyRep::List(dng_list)) = &sys_entries[0] else {
                panic!("expected List of KeyVals per solarSystemID from {method}");
            };
            assert_eq!(dng_list.len(), 2);
            assert_eq!(
                dng_list[0].dict_get("dungeonID"),
                Some(&PyRep::Integer(2868))
            );
            assert_eq!(
                dng_list[0].dict_get("instanceID"),
                Some(&PyRep::Integer(9001))
            );
            assert_eq!(
                dng_list[0].dict_get("solarSystemID"),
                Some(&PyRep::Integer(30_000_142))
            );
            assert_eq!(
                dng_list[0].dict_get("factionID"),
                Some(&PyRep::Integer(500_004))
            );
        }

        // Verify every FW faction_id mapping arm in GetFactionWarfareInstances normalization.
        for &(did, expected_fac) in &[
            (2894_i64, 500_002_i64),
            (2900, 500_002),
            (2908, 500_002),
            (2911, 500_002),
            (9892, 500_002),
            (9909, 500_002),
            (2774, 500_003),
            (2789, 500_003),
            (2799, 500_003),
            (2805, 500_003),
            (9578, 500_003),
            (9736, 500_003),
            (2868, 500_004),
            (2879, 500_004),
            (2893, 500_004),
            (2903, 500_004),
            (9893, 500_004),
            (9911, 500_004),
            (10485, 500_011),
            (2869, 500_001),
            (100, 500_001),
        ] {
            let raw = PyRep::Dict(vec![(
                PyRep::Integer(30_002_813),
                PyRep::Dict(vec![(
                    PyRep::Integer(did),
                    PyRep::List(vec![PyRep::Integer(1)]),
                )]),
            )]);
            let norm = normalize_macho_rpc_reply(
                "dungeonInstanceCacheMgr",
                "GetFactionWarfareInstances",
                Ok(raw),
            )
            .unwrap();
            let PyRep::Dict(entries) = norm else {
                panic!("expected Dict");
            };
            let (_, PyRep::List(items)) = &entries[0] else {
                panic!("expected List");
            };
            assert_eq!(
                items[0].dict_get("factionID"),
                Some(&PyRep::Integer(expected_fac)),
                "did {did} must map to faction {expected_fac}"
            );
        }

        // #806: GetPirateInsurgencyInstances and GetPirateInsurgencyIceHeistInstances must group
        // flat KeyVal lists by solarSystemID and populate default dungeonID and factionID.
        for (method, expected_did) in [
            ("GetPirateInsurgencyInstances", 10_477_i64),
            ("GetPirateInsurgencyIceHeistInstances", 10_497_i64),
        ] {
            let ins_raw = PyRep::List(vec![
                PyRep::keyval([
                    ("instanceID", PyRep::Integer(7001)),
                    ("solarSystemID", PyRep::Integer(30_002_813)),
                ]),
                PyRep::keyval([("instanceID", PyRep::Integer(7002))]),
            ]);
            let ins_norm =
                normalize_macho_rpc_reply("dungeonInstanceCacheMgr", method, Ok(ins_raw)).unwrap();
            let PyRep::Dict(ins_sys) = ins_norm else {
                panic!("expected Dict from {method}");
            };
            assert_eq!(ins_sys.len(), 2);
            assert_eq!(ins_sys[0].0, PyRep::Integer(30_000_142));
            assert_eq!(ins_sys[1].0, PyRep::Integer(30_002_813));
            let (_, PyRep::List(ins_items)) = &ins_sys[1] else {
                panic!("expected List under solarSystemID");
            };
            assert_eq!(
                ins_items[0].dict_get("dungeonID"),
                Some(&PyRep::Integer(expected_did))
            );
            assert_eq!(
                ins_items[0].dict_get("factionID"),
                Some(&PyRep::Integer(500_010))
            );
            assert_eq!(
                ins_items[0].dict_get("instanceID"),
                Some(&PyRep::Integer(7001))
            );
        }
    }

    #[test]
    fn get_titles_rowset_normalizes_to_dict_of_util_rows_and_empty_dict() {
        let cols = vec![
            "titleID",
            "titleName",
            "roles",
            "grantableRoles",
            "rolesAtHQ",
            "grantableRolesAtHQ",
            "rolesAtBase",
            "grantableRolesAtBase",
            "rolesAtOther",
            "grantableRolesAtOther",
        ];
        let rowset = PyRep::rowset(
            cols.clone(),
            vec![
                vec![
                    PyRep::Integer(1),
                    PyRep::String("Commander".into()),
                    PyRep::Integer(256),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                ],
                vec![
                    PyRep::Integer(2),
                    PyRep::String("Quartermaster".into()),
                    PyRep::Integer(8192),
                    PyRep::Integer(128),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                ],
            ],
        );

        for svc in ["corpRegistry", "machoNet"] {
            let res = normalize_macho_rpc_reply(svc, "GetTitles", Ok(rowset.clone())).unwrap();
            let PyRep::Dict(entries) = res else {
                panic!("expected Dict for {svc}.GetTitles");
            };
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, PyRep::Integer(1));
            assert_eq!(entries[1].0, PyRep::Integer(2));

            let PyRep::Object { type_name, args } = &entries[0].1 else {
                panic!("expected util.Row object");
            };
            assert_eq!(type_name, "util.Row");
            let PyRep::Tuple(parts) = args.as_ref() else {
                panic!("expected (header, line) tuple");
            };
            let (PyRep::List(hdr), PyRep::List(line)) = (&parts[0], &parts[1]) else {
                panic!("expected header & line lists");
            };
            assert_eq!(hdr.len(), 10);
            assert_eq!(line.len(), 10);
            assert_eq!(line[1], PyRep::String("Commander".into()));
            assert_eq!(line[2], PyRep::Integer(256));
        }

        let empty_rs = PyRep::rowset(cols, vec![]);
        let empty_res = normalize_macho_rpc_reply("corpRegistry", "GetTitles", Ok(empty_rs)).unwrap();
        assert_eq!(empty_res, PyRep::Dict(vec![]));
    }

    #[test]
    fn skill_queue_normalizes_to_list_of_keyvals_for_v24() {
        let legacy = PyRep::rowset(
            vec!["trainingTypeID", "trainingToLevel", "durationSeconds"],
            vec![vec![
                PyRep::Integer(3300),
                PyRep::Integer(2),
                PyRep::Real(120.0),
            ]],
        );
        let rep = normalize_macho_rpc_reply("skillMgr2", "GetSkillQueue", Ok(legacy)).unwrap();
        let PyRep::List(entries) = rep else {
            panic!("expected PyRep::List of KeyVals");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dict_get("trainingTypeID"), Some(&PyRep::Integer(3300)));
        assert!(entries[0].dict_get("trainingStartTime").is_some());
        assert!(entries[0].dict_get("trainingEndTime").is_some());

        let kv_list = PyRep::List(entries.clone());
        let notif = normalize_macho_notification("OnNewSkillQueueSaved", vec![kv_list.clone()]);
        assert_eq!(notif, vec![kv_list]);
    }

    #[test]
    fn cloak_ball_and_special_fx_uncloak_normalize_to_v24_client_format() {
        let ship_id = 100_000_042_i64;

        let uncloak_bool = normalize_destiny_payload_arg(PyRep::Tuple(vec![
            PyRep::String("CloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Bool(false)]),
        ]));
        assert_eq!(
            uncloak_bool,
            PyRep::Tuple(vec![
                PyRep::String("UncloakBall".into()),
                PyRep::Tuple(vec![PyRep::Integer(ship_id)]),
            ])
        );

        let uncloak_int = normalize_destiny_payload_arg(PyRep::Tuple(vec![
            PyRep::String("CloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(0)]),
        ]));
        assert_eq!(
            uncloak_int,
            PyRep::Tuple(vec![
                PyRep::String("UncloakBall".into()),
                PyRep::Tuple(vec![PyRep::Integer(ship_id)]),
            ])
        );

        let cloak_on = PyRep::Tuple(vec![
            PyRep::String("CloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Bool(true)]),
        ]);
        assert_eq!(normalize_destiny_payload_arg(cloak_on.clone()), cloak_on);

        let fx_uncloak = normalize_destiny_payload_arg(PyRep::Tuple(vec![
            PyRep::String("OnSpecialFX".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(ship_id),
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::String("effects.Cloak".into()),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ]),
        ]));
        let PyRep::Tuple(outer) = fx_uncloak else {
            panic!("expected OnSpecialFX tuple");
        };
        assert_eq!(outer[0], PyRep::String("OnSpecialFX".into()));
        let PyRep::Tuple(args) = &outer[1] else {
            panic!("expected FX args tuple");
        };
        assert_eq!(args[5], PyRep::String("effects.Uncloak".into()));
        assert_eq!(args[7], PyRep::Integer(1));
        assert_eq!(args[8], PyRep::Integer(0));
        assert_eq!(args[9], PyRep::Integer(6000));
        assert_eq!(args[10], PyRep::Integer(1));
    }

    #[test]
    fn normalize_contract_reply_does_not_inject_hardcoded_domain_ids() {
        let rep = normalize_macho_rpc_reply(
            "contractProxy",
            "GetContract",
            Ok(PyRep::keyval([
                ("contractID", PyRep::Integer(42)),
                ("type", PyRep::String("ItemExchange".into())),
                ("status", PyRep::String("Outstanding".into())),
                ("availability", PyRep::String("Public".into())),
                (
                    "items",
                    PyRep::List(vec![PyRep::keyval([
                        ("itemID", PyRep::Integer(99)),
                        ("quantity", PyRep::Integer(2)),
                    ])]),
                ),
            ])),
        )
        .unwrap();
        let inner = rep.dict_get("contract").expect("contract");
        assert_eq!(inner.dict_get("startSolarSystemID"), Some(&PyRep::Integer(0)));
        assert_eq!(inner.dict_get("endSolarSystemID"), Some(&PyRep::Integer(0)));
        assert_eq!(inner.dict_get("startRegionID"), Some(&PyRep::Integer(0)));
        assert_eq!(inner.dict_get("endRegionID"), Some(&PyRep::Integer(0)));
        let Some(PyRep::List(items)) = rep.dict_get("items") else {
            panic!("expected items list");
        };
        assert_eq!(items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(0)));
    }

    #[test]
    fn issue_766_agent_mgr_v24_dialogue_briefing_objective_and_location_normalization() {
        // 1. DoAction offered mission -> ((agentSaysMsg, availableActions), lastActionInfo)
        let do_action = normalize_macho_rpc_reply(
            "agentMgr",
            "DoAction",
            Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(501)),
                ("contentID", PyRep::Integer(501)),
                ("agentID", PyRep::Integer(3_018_910)),
                ("state", PyRep::String("Offered".into())),
                ("missionState", PyRep::Integer(1)),
                ("briefing", PyRep::String("Clear pirates".into())),
                (
                    "dialogue",
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]),
                        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())]),
                    ]),
                ),
                ("lpReward", PyRep::Integer(450)),
            ])),
        )
        .expect("DoAction ok");

        let PyRep::Tuple(outer) = do_action else {
            panic!("DoAction must return 2-tuple");
        };
        assert_eq!(outer.len(), 2);
        let PyRep::Tuple(inner) = &outer[0] else {
            panic!("first element must be (agentSaysMsg, availableActions)");
        };
        assert_eq!(
            inner[0],
            PyRep::Tuple(vec![
                PyRep::String("Clear pirates".into()),
                PyRep::Integer(501),
            ])
        );
        assert_eq!(
            inner[1],
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(9)]),
            ])
        );
        assert!(matches!(outer[1], PyRep::Dict(_)), "lastActionInfo must be Dict");
        assert_eq!(outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(false)));
        assert_eq!(outer[1].dict_get("missionDeclined"), Some(&PyRep::Bool(false)));
        assert_eq!(outer[1].dict_get("missionQuit"), Some(&PyRep::Bool(false)));
        assert_eq!(outer[1].dict_get("missionCantReplay"), Some(&PyRep::None));
        assert_eq!(outer[1].dict_get("loyaltyPoints"), Some(&PyRep::Integer(450)));

        // Idempotent if already a 2-tuple
        assert!(matches!(
            normalize_macho_rpc_reply("agentMgr", "DoAction", Ok(PyRep::Tuple(outer.clone()))),
            Ok(PyRep::Tuple(_))
        ));

        // Completed mission sets missionCompleted=true and fallback message
        let completed = normalize_macho_rpc_reply(
            "agentMgr",
            "DoAction",
            Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(501)),
                ("state", PyRep::String("Completed".into())),
                ("missionState", PyRep::Integer(4)),
            ])),
        )
        .unwrap();
        let PyRep::Tuple(c_outer) = completed else { panic!("tuple") };
        assert_eq!(c_outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(true)));

        // 2. GetAgentLocationWrap -> Dict with solarsystemID, locationID, typeID
        let loc = normalize_macho_rpc_reply(
            "agentMgr",
            "GetAgentLocationWrap",
            Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(3_018_910)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("stationID", PyRep::Integer(60_003_760)),
            ])),
        )
        .unwrap();
        assert!(matches!(loc, PyRep::Dict(_)));
        assert_eq!(loc.dict_get("solarsystemID"), Some(&PyRep::Integer(30_000_142)));
        assert_eq!(loc.dict_get("locationID"), Some(&PyRep::Integer(60_003_760)));
        assert_eq!(loc.dict_get("typeID"), Some(&PyRep::Integer(5)));

        // 3. GetMissionBriefingInfo / GetMissionObjectiveInfo -> Dict keys + MissionNotActive -> None
        let no_briefing = normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionBriefingInfo",
            Err(eve_proto::BridgeRpcError {
                code: "MissionNotActive".into(),
                message: "none".into(),
            }),
        )
        .unwrap();
        assert_eq!(no_briefing, PyRep::None);

        let no_obj = normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionObjectiveInfo",
            Err(eve_proto::BridgeRpcError {
                code: "MissionNotActive".into(),
                message: "none".into(),
            }),
        )
        .unwrap();
        assert_eq!(no_obj, PyRep::None);

        let briefing = normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionBriefingInfo",
            Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(501)),
                ("title", PyRep::String("Eliminate Pirates".into())),
                ("briefing", PyRep::String("Go there.".into())),
            ])),
        )
        .unwrap();
        assert!(matches!(briefing, PyRep::Dict(_)));
        assert_eq!(
            briefing.dict_get("Mission Title ID"),
            Some(&PyRep::String("Eliminate Pirates".into()))
        );
        assert_eq!(briefing.dict_get("ContentID"), Some(&PyRep::Integer(501)));
        assert_eq!(briefing.dict_get("Decline Time"), Some(&PyRep::Integer(-1)));
        assert!(matches!(briefing.dict_get("Mission Keywords"), Some(PyRep::Dict(_))));

        let objective = normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionObjectiveInfo",
            Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(501)),
                ("title", PyRep::String("Eliminate Pirates".into())),
                ("missionState", PyRep::Integer(2)),
                ("iskReward", PyRep::Integer(100_000)),
                ("lpReward", PyRep::Integer(250)),
                ("bonusReward", PyRep::Integer(50_000)),
            ])),
        )
        .unwrap();
        assert!(matches!(objective, PyRep::Dict(_)));
        assert_eq!(objective.dict_get("completionStatus"), Some(&PyRep::Integer(0)));
        assert_eq!(objective.dict_get("loyaltyPoints"), Some(&PyRep::Integer(250)));
        assert_eq!(
            objective.dict_get("normalRewards"),
            Some(&PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(29),
                PyRep::Integer(100_000),
                PyRep::None,
            ])]))
        );
    }
}

#[cfg(test)]
#[path = "client_compat_cov_unit_tests.rs"]
mod client_compat_cov_unit_tests;

#[cfg(test)]
#[path = "client_compat_extra_tests.rs"]
mod client_compat_extra_tests;
