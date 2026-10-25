//! `corp_medals_bulletins` — Corporation and Alliance Medals and Bulletins RPC handlers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(crate) fn handle_medal_queries(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "GetAllCorpMedals" => {
            let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let (medal_rows, graphic_rows) = ctx.db.with_snapshot(|snap| {
                let mut m_rows = Vec::new();
                let mut g_rows = Vec::new();
                if let Some(state) = snap.corp_governance.get(&target_corp) {
                    for (mid, title, desc, creator, ts) in &state.medals {
                        let recip_cnt = state.medal_recipients.iter().filter(|r| r.0 == *mid).count() as i64;
                        m_rows.push(vec![
                            PyRep::Integer(*mid), PyRep::String(title.clone()), PyRep::String(desc.clone()),
                            PyRep::Integer(*creator), PyRep::Integer(*ts as i64), PyRep::Integer(target_corp),
                            PyRep::Integer(recip_cnt),
                        ]);
                        let layers = state.medal_graphics.get(mid).cloned().unwrap_or_else(|| vec![(1, 1, "ribbon_1".into(), 10)]);
                        for (part, layer, graphic, color) in layers {
                            g_rows.push(vec![PyRep::Integer(*mid), PyRep::Integer(part), PyRep::Integer(layer), PyRep::String(graphic), PyRep::Integer(color)]);
                        }
                    }
                }
                (m_rows, g_rows)
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::rowset(vec!["medalID", "title", "description", "creatorID", "date", "corporationID", "numberOfRecipients"], medal_rows),
                PyRep::rowset(vec!["medalID", "part", "layer", "graphic", "color"], graphic_rows),
            ])))
        }
        "GetMedalDetails" => {
            let medal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let res = ctx.db.with_snapshot(|snap| {
                for (&cid, state) in &snap.corp_governance {
                    if let Some((mid, title, desc, creator, ts)) = state.medals.iter().find(|m| m.0 == medal_id) {
                        let recip_cnt = state.medal_recipients.iter().filter(|r| r.0 == *mid).count() as i64;
                        let layers = state.medal_graphics.get(mid).cloned().unwrap_or_else(|| vec![(1, 1, "ribbon_1".into(), 10)]);
                        let g_rows: Vec<Vec<PyRep>> = layers
                            .into_iter()
                            .map(|(part, layer, graphic, color)| vec![PyRep::Integer(*mid), PyRep::Integer(part), PyRep::Integer(layer), PyRep::String(graphic), PyRep::Integer(color)])
                            .collect();
                        let info_row = vec![
                            PyRep::Integer(*mid), PyRep::String(title.clone()), PyRep::String(desc.clone()),
                            PyRep::Integer(*creator), PyRep::Integer(*ts as i64), PyRep::Integer(cid), PyRep::Integer(recip_cnt),
                        ];
                        return Ok(PyRep::keyval([
                            ("medalID", PyRep::Integer(*mid)),
                            ("corporationID", PyRep::Integer(cid)),
                            ("title", PyRep::String(title.clone())),
                            ("description", PyRep::String(desc.clone())),
                            ("creatorID", PyRep::Integer(*creator)),
                            ("date", PyRep::Integer(*ts as i64)),
                            ("numberOfRecipients", PyRep::Integer(recip_cnt)),
                            ("info", PyRep::rowset(vec!["medalID", "title", "description", "creatorID", "date", "corporationID", "numberOfRecipients"], vec![info_row])),
                            ("graphics", PyRep::rowset(vec!["medalID", "part", "layer", "graphic", "color"], g_rows)),
                        ]));
                    }
                }
                Err(BridgeRpcError { code: "MedalNotFound".into(), message: format!("Medal {medal_id} not found") })
            });
            Some(res)
        }
        "GetMedalsReceived" => {
            let target_char = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let min_status = if target_char == char_id { 2 } else { 3 };
            let (m_rows, g_rows) = ctx.db.with_snapshot(|snap| {
                let mut m_rows = Vec::new();
                let mut g_rows = Vec::new();
                for (&cid, state) in &snap.corp_governance {
                    for (mid, rid, iid, reason, status) in &state.medal_recipients {
                        if *rid == target_char && *status >= min_status {
                            let (title, desc, create_ts) = state.medals.iter().find(|m| m.0 == *mid)
                                .map(|m| (m.1.clone(), m.2.clone(), m.4))
                                .unwrap_or_else(|| ("Medal".into(), String::new(), 0));
                            let award_ts = snap.corp_issued_medals.get(&cid)
                                .and_then(|list| list.iter().find(|im| im.medal_id == *mid && im.character_id == *rid).map(|im| im.issued_at_unix))
                                .unwrap_or(create_ts);
                            m_rows.push(vec![
                                PyRep::Integer(*mid), PyRep::String(title), PyRep::String(desc),
                                PyRep::Integer(*iid), PyRep::Integer(award_ts as i64), PyRep::String(reason.clone()),
                                PyRep::Integer(*status), PyRep::Integer(*rid), PyRep::Integer(cid),
                            ]);
                            let layers = state.medal_graphics.get(mid).cloned().unwrap_or_else(|| vec![(1, 1, "ribbon_1".into(), 10)]);
                            for (part, layer, graphic, color) in layers {
                                g_rows.push(vec![PyRep::Integer(*mid), PyRep::Integer(part), PyRep::Integer(layer), PyRep::String(graphic), PyRep::Integer(color)]);
                            }
                        }
                    }
                }
                (m_rows, g_rows)
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::rowset(vec!["medalID", "title", "description", "issuerID", "date", "reason", "status", "wearerID", "corporationID"], m_rows),
                PyRep::rowset(vec!["medalID", "part", "layer", "graphic", "color"], g_rows),
            ])))
        }
        _ => {
            let medal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let rows = ctx.db.with_snapshot(|snap| {
                let mut rows = Vec::new();
                for (&cid, state) in &snap.corp_governance {
                    for (mid, rid, iid, reason, status) in &state.medal_recipients {
                        if *mid == medal_id {
                            let (title, create_ts) = state.medals.iter().find(|m| m.0 == *mid).map(|m| (m.1.clone(), m.4)).unwrap_or_else(|| ("Medal".into(), 0));
                            let award_ts = snap.corp_issued_medals.get(&cid)
                                .and_then(|list| list.iter().find(|im| im.medal_id == *mid && im.character_id == *rid).map(|im| im.issued_at_unix))
                                .unwrap_or(create_ts);
                            rows.push(vec![
                                PyRep::Integer(*rid), PyRep::Integer(*rid), PyRep::Integer(*iid),
                                PyRep::Integer(cid), PyRep::Integer(*mid), PyRep::String(title),
                                PyRep::String(reason.clone()), PyRep::Integer(*status), PyRep::Integer(award_ts as i64),
                            ]);
                        }
                    }
                }
                rows
            });
            Some(Ok(PyRep::rowset(vec!["recipientID", "wearerID", "issuerID", "corporationID", "medalID", "title", "reason", "status", "date"], rows)))
        }
    }
}

pub(crate) fn handle_corp_bulletins(
    ctx: &ServerContext,
    session: &SessionState,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
    has_comms: bool,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if method != "GetBulletins" && !has_comms {
        return Some(Err(BridgeRpcError {
            code: "CorpRoleDenied".into(),
            message: "Managing corporation bulletins requires Communications Officer or Director role".into(),
        }));
    }
    match method {
        "AddBulletin" => {
            let title = args.first().and_then(|a| a.as_str()).unwrap_or("Corporation Bulletin");
            let body = args.get(1).and_then(|a| a.as_str()).unwrap_or("All hands on deck");
            let is_alliance = matches!(args.get(2), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if is_alliance {
                    let aid = session.alliance_id.unwrap_or(0);
                    if !snap.alliances.get(&aid).is_some_and(|a| a.executor_corp_id == corp_id) {
                        return Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Managing alliance bulletins requires Executor corporation privileges".into() });
                    }
                    let meta = snap.alliance_war_state.alliances.entry(aid).or_default();
                    let state = snap.corp_governance.entry(aid).or_default();
                    let max_b = state.bulletins.iter().map(|b| b.0).chain(meta.bulletins.iter().map(|b| b.0)).max().unwrap_or(0);
                    let bid = state.next_bulletin_id.max(max_b) + 1;
                    state.next_bulletin_id = bid;
                    let entry = (bid, title.to_string(), body.to_string(), char_id, now_unix);
                    state.bulletins.push(entry.clone());
                    meta.bulletins.push(entry);
                    Ok(bid)
                } else {
                    let state = snap.corp_governance.entry(corp_id).or_default();
                    let max_b = state.bulletins.iter().map(|b| b.0).max().unwrap_or(0);
                    let bid = state.next_bulletin_id.max(max_b) + 1;
                    state.next_bulletin_id = bid;
                    state.bulletins.push((bid, title.to_string(), body.to_string(), char_id, now_unix));
                    Ok(bid)
                }
            });
            match res {
                Ok(bid) => Some(Ok(PyRep::keyval([("bulletinID", PyRep::Integer(bid)), ("title", PyRep::String(title.to_string())), ("body", PyRep::String(body.to_string()))]))),
                Err(e) => Some(Err(e)),
            }
        }
        "UpdateBulletinOrder" => {
            let order_pairs: Vec<(i64, i64)> = match args.first() {
                Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.iter().enumerate().filter_map(|(idx, item)| match item {
                    PyRep::List(p) | PyRep::Tuple(p) => Some((p.first()?.as_i64()?, p.get(1).and_then(|x| x.as_i64()).unwrap_or(idx as i64))),
                    PyRep::Integer(bid) => Some((*bid, idx as i64)),
                    _ => None,
                }).collect(),
                _ => args.iter().enumerate().filter_map(|(idx, x)| x.as_i64().map(|bid| (bid, idx as i64))).collect(),
            };
            let is_alliance = matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let owner_key = if is_alliance {
                    let aid = session.alliance_id.unwrap_or(0);
                    if !snap.alliances.get(&aid).is_some_and(|a| a.executor_corp_id == corp_id) {
                        return Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Managing alliance bulletins requires Executor corporation privileges".into() });
                    }
                    aid
                } else {
                    corp_id
                };
                if !order_pairs.is_empty() {
                    let sort_fn = |b: &(i64, String, String, i64, u64)| order_pairs.iter().find(|&&(id, _)| id == b.0).map(|&(_, ord)| ord).unwrap_or(i64::MAX);
                    snap.corp_governance.entry(owner_key).or_default().bulletins.sort_by_key(sort_fn);
                    if is_alliance {
                        snap.alliance_war_state.alliances.entry(owner_key).or_default().bulletins.sort_by_key(sort_fn);
                    }
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }
        "GetBulletins" => {
            let owner_key = match args.first() {
                Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)) => session.alliance_id.unwrap_or(corp_id),
                Some(PyRep::Bool(false)) | Some(PyRep::Integer(0)) | None => corp_id,
                Some(PyRep::Integer(id)) => *id,
                _ => corp_id,
            };
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                let list = snap.corp_governance.get(&owner_key).map(|s| &s.bulletins).filter(|b| !b.is_empty())
                    .or_else(|| snap.alliance_war_state.alliances.get(&owner_key).map(|m| &m.bulletins));
                list.map(|bulletins| {
                    bulletins.iter().enumerate().map(|(idx, (bid, title, body, aid, ts))| {
                        vec![PyRep::Integer(*bid), PyRep::String(title.clone()), PyRep::String(body.clone()), PyRep::Integer(*aid), PyRep::Integer(*ts as i64), PyRep::Integer(idx as i64)]
                    }).collect()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["bulletinID", "title", "body", "editCharacterID", "editDateTime", "sortOrder"], rows)))
        }
        _ => {
            let bid = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let explicit_alliance = matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let in_corp = snap.corp_governance.get(&corp_id).is_some_and(|s| s.bulletins.iter().any(|b| b.0 == bid));
                let aid_opt = session.alliance_id.filter(|&aid| {
                    explicit_alliance || (!in_corp && (snap.corp_governance.get(&aid).is_some_and(|s| s.bulletins.iter().any(|b| b.0 == bid)) || snap.alliance_war_state.alliances.get(&aid).is_some_and(|m| m.bulletins.iter().any(|b| b.0 == bid))))
                });
                if let Some(aid) = aid_opt {
                    if !snap.alliances.get(&aid).is_some_and(|a| a.executor_corp_id == corp_id) {
                        return Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Deleting alliance bulletins requires Executor corporation privileges".into() });
                    }
                    let mut removed = false;
                    if let Some(s) = snap.corp_governance.get_mut(&aid) {
                        let before = s.bulletins.len();
                        s.bulletins.retain(|b| b.0 != bid);
                        removed |= s.bulletins.len() != before;
                    }
                    if let Some(m) = snap.alliance_war_state.alliances.get_mut(&aid) {
                        let before = m.bulletins.len();
                        m.bulletins.retain(|b| b.0 != bid);
                        removed |= m.bulletins.len() != before;
                    }
                    if !removed {
                        return Err(BridgeRpcError { code: "BulletinNotFound".into(), message: format!("Bulletin {bid} not found") });
                    }
                    Ok(())
                } else {
                    if explicit_alliance {
                        return Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Deleting alliance bulletins requires Executor corporation privileges".into() });
                    }
                    let removed = snap.corp_governance.get_mut(&corp_id).map(|s| {
                        let before = s.bulletins.len();
                        s.bulletins.retain(|b| b.0 != bid);
                        s.bulletins.len() != before
                    }).unwrap_or(false);
                    if !removed {
                        return Err(BridgeRpcError { code: "BulletinNotFound".into(), message: format!("Bulletin {bid} not found") });
                    }
                    Ok(())
                }
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([("bulletinID", PyRep::Integer(bid)), ("deleted", PyRep::Bool(true))]))),
                Err(e) => Some(Err(e)),
            }
        }
    }
}
