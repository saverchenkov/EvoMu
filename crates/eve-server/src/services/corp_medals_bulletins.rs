//! `corp_medals_bulletins` — Corporation and Alliance Medals and Bulletins RPC handlers.

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(crate) fn handle_corp_medals(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
    has_comms: bool,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "CreateMedal" => {
            if !has_comms {
                return Some(Err(BridgeRpcError::user_error("CrpAccessDenied")));
            }
            let title = args.first().and_then(|a| a.as_str()).unwrap_or("Valorous Service Medal");
            let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Awarded for fleet valor");
            let mut graphics: Vec<(i64, i64, String, i64)> = Vec::new();
            if let Some(PyRep::List(items) | PyRep::Tuple(items)) = args.get(2) {
                for (idx, item) in items.iter().enumerate() {
                    if let PyRep::List(els) | PyRep::Tuple(els) = item {
                        let part = els.first().and_then(|x| x.as_i64()).unwrap_or(1);
                        let layer = els.get(1).and_then(|x| x.as_i64()).unwrap_or((idx as i64) + 1);
                        let graphic = els
                            .get(2)
                            .and_then(|x| x.as_str())
                            .map(String::from)
                            .or_else(|| els.get(2).and_then(|x| x.as_i64()).map(|n| n.to_string()))
                            .unwrap_or_else(|| "ribbon_1".into());
                        let color = els.get(3).and_then(|x| x.as_i64()).unwrap_or(10);
                        graphics.push((part, layer, graphic, color));
                    }
                }
            }
            if graphics.is_empty() {
                graphics.push((1, 1, "ribbon_1".into(), 10));
            }
            let medal_fee = IskCents::from_isk(5_000_000);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: "Character not found".into() });
                };
                ch.balance = ch.balance.checked_sub(medal_fee).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Creating a corporation medal costs 5,000,000 ISK".into(),
                })?;
                let max_gov = snap
                    .corp_governance
                    .values()
                    .flat_map(|s| s.medals.iter().map(|m| m.0))
                    .max()
                    .unwrap_or(0);
                let max_ext = snap
                    .corp_medals
                    .values()
                    .flatten()
                    .map(|m| m.medal_id)
                    .max()
                    .unwrap_or(0);
                let mid = max_gov.max(max_ext) + 1;
                let state = snap.corp_governance.entry(corp_id).or_default();
                state.medals.push((mid, title.to_string(), desc.to_string(), char_id, now_unix));
                state.medal_graphics.insert(mid, graphics);
                snap.corp_medals.entry(corp_id).or_default().push(eve_db::snapshot_ext::CorpMedalEntry {
                    medal_id: mid,
                    title: title.to_string(),
                    description: desc.to_string(),
                    creator_id: char_id,
                    created_at_unix: now_unix,
                });
                Ok(mid)
            });
            let medal_id = match res {
                Ok(mid) => mid,
                Err(e) => return Some(Err(e)),
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMedalAdded", "corpid", vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id)]);
            Some(Ok(PyRep::keyval([
                ("medalID", PyRep::Integer(medal_id)),
                ("title", PyRep::String(title.to_string())),
                ("description", PyRep::String(desc.to_string())),
            ])))
        }

        "GiveMedalToCharacters" => {
            if !has_comms {
                return Some(Err(BridgeRpcError::user_error("CrpAccessDenied")));
            }
            let medal_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                _ => return Some(Err(BridgeRpcError::user_error("InvalidParameter"))),
            };
            let recipient_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.iter().filter_map(|v| v.as_i64()).filter(|&id| id > 0).collect()
                }
                _ => Vec::new(),
            };
            if recipient_ids.is_empty() || recipient_ids.len() > 100 {
                return Some(Err(BridgeRpcError::user_error("InvalidParameter")));
            }
            let reason = args.get(2).and_then(|a| a.as_str()).unwrap_or("Combat distinction");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                for &recipient_id in &recipient_ids {
                    if !snap.characters.contains_key(&recipient_id) {
                        return Err(BridgeRpcError::user_error("CharNotFound"));
                    }
                }
                let state = snap.corp_governance.entry(corp_id).or_default();
                let Some((m_title, m_desc)) = state
                    .medals
                    .iter()
                    .find(|m| m.0 == medal_id)
                    .map(|m| (m.1.clone(), m.2.clone()))
                else {
                    return Err(BridgeRpcError::user_error("MedalNotFound"));
                };
                let gfx_layers: Vec<eve_db::snapshot_ext::CharacterMedalGraphicRecord> = state
                    .medal_graphics
                    .get(&medal_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(part, layer, graphic, color)| eve_db::snapshot_ext::CharacterMedalGraphicRecord {
                        part,
                        layer,
                        graphic,
                        color,
                    })
                    .collect();
                for &recipient_id in &recipient_ids {
                    state.medal_recipients.push((medal_id, recipient_id, char_id, reason.to_string(), 2));
                    snap.corp_issued_medals.entry(corp_id).or_default().push(eve_db::snapshot_ext::CorpIssuedMedalEntry {
                        medal_id, character_id: recipient_id, reason: reason.to_string(), status: "private".into(), issuer_id: char_id, issued_at_unix: now_unix,
                    });
                    snap.char_medals.entry(recipient_id).or_default().push(eve_db::snapshot_ext::CharacterMedalRecord {
                        medal_id, title: m_title.clone(), description: m_desc.clone(), corporation_id: corp_id, issuer_id: char_id, date_unix: now_unix, reason: reason.to_string(), status: "private".into(), graphics: gfx_layers.clone(),
                    });
                    if let Some(list) = snap.char_medals.get(&recipient_id) {
                        if let Ok(json_str) = serde_json::to_string(list) {
                            snap.service_state_blobs.insert(format!("character_medals:{recipient_id}"), json_str.clone());
                            snap.service_state_blobs.insert(format!("char_medals:{recipient_id}"), json_str);
                        }
                    }
                }
                Ok(())
            });
            if let Err(e) = res {
                return Some(Err(e));
            }
            for &recipient_id in &recipient_ids {
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMedalAdded", "corpid", vec![PyRep::Integer(medal_id), PyRep::Integer(recipient_id)]);
                ctx.notify(NotificationTarget::Character(recipient_id), "OnMedalIssued", "charid", vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id), PyRep::Integer(char_id)]);
            }
            let first_recipient = recipient_ids[0];
            Some(Ok(PyRep::keyval([("medalID", PyRep::Integer(medal_id)), ("recipientID", PyRep::Integer(first_recipient)), ("status", PyRep::Integer(2))])))
        }

        // parity: feat:character.medals-display, rpc:corporationSvc.SetMedalStatus
        "SetMedalStatus" => {
            let mut updates: Vec<(i64, i64)> = Vec::new();
            if let Some(PyRep::Dict(dict)) = args.first() {
                for (k, v) in dict {
                    if let Some(mid) = k.as_i64() {
                        let st = match v {
                            PyRep::Integer(i) => *i,
                            PyRep::String(s) => {
                                if s.eq_ignore_ascii_case("public") {
                                    3
                                } else if s.eq_ignore_ascii_case("deleted") || s.eq_ignore_ascii_case("delete") {
                                    1
                                } else {
                                    2
                                }
                            }
                            PyRep::Bool(b) => if *b { 3 } else { 2 },
                            _ => 3,
                        };
                        updates.push((mid, st));
                    }
                }
            } else if let Some(mid) = args.first().and_then(|a| a.as_i64()) {
                let st = args.get(1).and_then(|a| match a {
                    PyRep::Integer(i) => Some(*i),
                    PyRep::String(s) => {
                        if s.eq_ignore_ascii_case("public") {
                            Some(3)
                        } else if s.eq_ignore_ascii_case("deleted") || s.eq_ignore_ascii_case("delete") {
                            Some(1)
                        } else {
                            Some(2)
                        }
                    }
                    PyRep::Bool(b) => if *b { Some(3) } else { Some(2) },
                    _ => None,
                }).unwrap_or(3);
                updates.push((mid, st));
            }
            ctx.db.with_mut_snapshot(|snap| {
                for &(medal_id, new_status) in &updates {
                    if new_status == 1 {
                        for state in snap.corp_governance.values_mut() {
                            for rec in &mut state.medal_recipients {
                                if rec.0 == medal_id && rec.1 == char_id { rec.4 = 1; }
                            }
                        }
                        for list in snap.corp_issued_medals.values_mut() {
                            list.retain(|m| !(m.medal_id == medal_id && m.character_id == char_id));
                        }
                        if let Some(list) = snap.char_medals.get_mut(&char_id) {
                            list.retain(|m| m.medal_id != medal_id);
                        }
                    } else {
                        let status_str = if new_status >= 3 { "public" } else { "private" };
                        for state in snap.corp_governance.values_mut() {
                            for rec in &mut state.medal_recipients {
                                if rec.0 == medal_id && rec.1 == char_id { rec.4 = new_status; }
                            }
                        }
                        for list in snap.corp_issued_medals.values_mut() {
                            for m in list {
                                if m.medal_id == medal_id && m.character_id == char_id { m.status = status_str.into(); }
                            }
                        }
                        let mut found = false;
                        if let Some(list) = snap.char_medals.get_mut(&char_id) {
                            for m in list {
                                if m.medal_id == medal_id {
                                    m.status = status_str.into();
                                    found = true;
                                }
                            }
                        }
                        if !found {
                            for (&cid, state) in &snap.corp_governance {
                                if let Some((_, title, desc, _issuer, ts)) = state.medals.iter().find(|m| m.0 == medal_id) {
                                    if let Some(rec) = state.medal_recipients.iter().find(|r| r.0 == medal_id && r.1 == char_id) {
                                        let gfx_layers: Vec<eve_db::snapshot_ext::CharacterMedalGraphicRecord> = state
                                            .medal_graphics
                                            .get(&medal_id)
                                            .cloned()
                                            .unwrap_or_default()
                                            .into_iter()
                                            .map(|(part, layer, graphic, color)| eve_db::snapshot_ext::CharacterMedalGraphicRecord {
                                                part, layer, graphic, color,
                                            })
                                            .collect();
                                        snap.char_medals.entry(char_id).or_default().push(eve_db::snapshot_ext::CharacterMedalRecord {
                                            medal_id,
                                            title: title.clone(),
                                            description: desc.clone(),
                                            corporation_id: cid,
                                            issuer_id: rec.2,
                                            date_unix: *ts,
                                            reason: rec.3.clone(),
                                            status: status_str.into(),
                                            graphics: gfx_layers,
                                        });
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(list) = snap.char_medals.get(&char_id) {
                    if let Ok(json_str) = serde_json::to_string(list) {
                        snap.service_state_blobs.insert(format!("character_medals:{char_id}"), json_str.clone());
                        snap.service_state_blobs.insert(format!("char_medals:{char_id}"), json_str);
                    }
                }
            });
            for &(medal_id, new_status) in &updates {
                ctx.notify(NotificationTarget::Character(char_id), "OnMedalStatusChanged", "charid", vec![PyRep::Integer(medal_id), PyRep::Integer(new_status)]);
            }
            let last_update = updates.last().cloned().unwrap_or((1, 3));
            Some(Ok(PyRep::keyval([("medalID", PyRep::Integer(last_update.0)), ("status", PyRep::Integer(last_update.1))])))
        }

        "GetAllCorpMedals"
        | "GetMedalDetails"
        | "GetMedalsReceived"
        | "GetRecipientsOfMedal" => handle_medal_queries(ctx, corp_id, char_id, method, args),

        _ => None,
    }
}

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
                let mut seen_medals = std::collections::HashSet::new();
                for (&cid, state) in &snap.corp_governance {
                    for (mid, rid, iid, reason, status) in &state.medal_recipients {
                        if *rid == target_char && *status >= min_status {
                            seen_medals.insert(*mid);
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
                if let Some(list) = snap.char_medals.get(&target_char) {
                    for cm in list {
                        if !seen_medals.contains(&cm.medal_id) {
                            let status_num = if cm.status.eq_ignore_ascii_case("public") { 3 } else { 2 };
                            if status_num >= min_status {
                                m_rows.push(vec![
                                    PyRep::Integer(cm.medal_id), PyRep::String(cm.title.clone()), PyRep::String(cm.description.clone()),
                                    PyRep::Integer(cm.issuer_id), PyRep::Integer(cm.date_unix as i64), PyRep::String(cm.reason.clone()),
                                    PyRep::Integer(status_num), PyRep::Integer(target_char), PyRep::Integer(cm.corporation_id),
                                ]);
                                for g in &cm.graphics {
                                    g_rows.push(vec![PyRep::Integer(cm.medal_id), PyRep::Integer(g.part), PyRep::Integer(g.layer), PyRep::String(g.graphic.clone()), PyRep::Integer(g.color)]);
                                }
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
                Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)) => session.alliance_id.unwrap_or(0),
                Some(PyRep::Bool(false)) | Some(PyRep::Integer(0)) | None => corp_id,
                Some(PyRep::Integer(id)) => {
                    if *id != corp_id && Some(*id) != session.alliance_id {
                        return Some(Err(BridgeRpcError::user_error("CrpAccessDenied")));
                    }
                    *id
                }
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
