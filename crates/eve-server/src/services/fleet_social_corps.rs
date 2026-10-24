//! Corporation Core Services (`SO-3`): `corpRegistry`, `corpmgr`, `corporationSvc`, `corpStationMgr`, `account`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
pub use eve_db::CorpGovState;
use eve_db::{corp_roles, CorpApplicationRecord, IskCents, MailRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub fn with_corp_gov<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut HashMap<i64, CorpGovState>) -> R,
) -> R {
    ctx.db.with_mut_snapshot(|snap| f(&mut snap.corp_governance))
}

pub fn read_corp_gov<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&HashMap<i64, CorpGovState>) -> R,
) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.corp_governance))
}

pub(crate) fn get_character_medals_and_decorations(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> (Vec<PyRep>, Vec<PyRep>) {
    let g = &snap.corp_governance;
    let mut medals = Vec::new();
    let mut decorations = Vec::new();
    for (&corp_id, state) in g.iter() {
        for (mid, rid, iid, reason, status) in &state.medal_recipients {
            if *rid == char_id {
                let title = state
                    .medals
                    .iter()
                    .find(|m| m.0 == *mid)
                    .map(|m| m.1.clone())
                    .unwrap_or_else(|| "Medal".into());
                let kv = PyRep::keyval([
                    ("medalID", PyRep::Integer(*mid)),
                    ("wearerID", PyRep::Integer(*rid)),
                    ("issuerID", PyRep::Integer(*iid)),
                    ("corporationID", PyRep::Integer(corp_id)),
                    ("title", PyRep::String(title)),
                    ("reason", PyRep::String(reason.clone())),
                    ("status", PyRep::Integer(*status)),
                ]);
                decorations.push(kv.clone());
                if *status > 1 {
                    medals.push(kv);
                }
            }
        }
    }
    (medals, decorations)
}

pub(super) fn compute_corp_member_limit(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    ceo_id: i64,
) -> usize {
    if let Some(lim) = snap
        .corp_governance
        .get(&corp_id)
        .and_then(|g| g.member_limit)
    {
        return lim;
    }
    let bonus: usize = snap
        .skills
        .get(&ceo_id)
        .map(|skills| {
            skills
                .values()
                .map(|s| {
                    let per_lvl = ctx
                        .sde
                        .types
                        .get(&s.type_id)
                        .and_then(|t| t.default_attributes.get(&191).copied())
                        .unwrap_or(0.0);
                    (per_lvl * s.level as f64) as usize
                })
                .sum()
        })
        .unwrap_or(20);
    if bonus == 0 {
        1
    } else {
        bonus
    }
}

pub(super) fn record_corp_join_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    old_corp_id: i64,
    corp_id: i64,
    now_unix: u64,
) {
    let list = snap.employment_history.entry(char_id).or_default();
    if list.is_empty() {
        let cdate = snap.characters.get(&char_id).map(|c| c.create_date_unix).unwrap_or(0);
        let prev = if old_corp_id > 0 { old_corp_id } else { 1_000_009 };
        list.push((prev, cdate, Some(now_unix)));
    } else if let Some(last) = list.last_mut() {
        if last.2.is_none() {
            last.2 = Some(now_unix);
        }
    }
    list.push((corp_id, now_unix, None));
}

const MEMBER_COLS: [&str; 8] = [
    "characterID", "name", "role", "title", "corporationID", "locationID", "shipTypeID", "logonDateTime",
];

pub(crate) fn require_corp_caller(
    ctx: &ServerContext,
    session: &mut SessionState,
    target_corp_arg: Option<i64>,
) -> Result<(i64, i64, bool), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.corporation_id.is_none() || session.corporation_id == Some(0) {
        if let Some(cid) = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).filter(|id| s.corporations.contains_key(id))) {
            session.corporation_id = Some(cid);
        }
    }
    let caller_corp = match session.corporation_id {
        Some(cid) if cid > 0 => cid,
        _ => return Err(BridgeRpcError { code: "CorpAccessDenied".into(), message: "Character does not belong to a corporation".into() }),
    };
    if let Some(target) = target_corp_arg {
        if target != caller_corp {
            return Err(BridgeRpcError { code: "CorpAccessDenied".into(), message: "Access denied to external corporation members".into() });
        }
    }
    let has_director = ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&caller_corp);
        let is_ceo = corp.map(|c| c.ceo_id == char_id).unwrap_or(false);
        let roles = corp.and_then(|c| c.member_roles.get(&char_id)).copied().unwrap_or(0);
        is_ceo || (roles & (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER) != 0)
    });
    Ok((caller_corp, char_id, has_director))
}

fn get_corp_member_snapshot(
    ctx: &ServerContext,
    corp_id: i64,
    viewer_id: i64,
    has_director: bool,
) -> Vec<(i64, String, u64, String, i64, i64, i64, u64)> {
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let gov_opt = snap.corp_governance.get(&corp_id);
        let Some(corp) = snap.corporations.get(&corp_id) else { return Vec::new(); };
        let mut mids: Vec<i64> = corp.members.iter().copied().collect();
        mids.sort_unstable();
        mids.into_iter().filter_map(|mid| {
            snap.characters.get(&mid).map(|ch| {
                let mut role_mask = corp.member_roles.get(&mid).copied().unwrap_or(0);
                if let Some(g) = gov_opt {
                    if let Some(&(p_mask, eff)) = g.pending_role_grants.get(&mid) {
                        if now_unix >= eff { role_mask |= p_mask; }
                    }
                    if let Some(&tmask) = g.member_titles.get(&mid) {
                        for (&tid, &(_, troles)) in &g.titles {
                            if (tmask & (1u64 << (tid.rem_euclid(16)))) != 0 { role_mask |= troles; }
                        }
                    }
                }
                let title = if mid == corp.ceo_id { "CEO" } else { "Member" };
                let (loc_id, ship_id, logon_ts) = if has_director || mid == viewer_id {
                    let s_id = snap.items.get(&ch.active_ship_id).map(|i| i.type_id as i64).unwrap_or(606);
                    (ch.solar_system_id as i64, s_id, now_unix)
                } else {
                    (0, 0, 0)
                };
                (ch.character_id, ch.name.clone(), role_mask, title.to_string(), corp_id, loc_id, ship_id, logon_ts)
            })
        }).collect()
    })
}

fn member_row_rep(m: &(i64, String, u64, String, i64, i64, i64, u64)) -> Vec<PyRep> {
    vec![
        PyRep::Integer(m.0), PyRep::String(m.1.clone()), PyRep::Integer(m.2 as i64),
        PyRep::String(m.3.clone()), PyRep::Integer(m.4), PyRep::Integer(m.5),
        PyRep::Integer(m.6), PyRep::Integer(m.7 as i64),
    ]
}

pub(super) fn handle_corps(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(
        service,
        "corpRegistry"
            | "corpmgr"
            | "corporationSvc"
            | "corpStationMgr"
            | "corpRecProxy"
            | "allianceRegistry"
            | "voteManager"
            | "account"
    ) {
        return None;
    }

    match (service, method) {
        ("corpRegistry", "AddCorporation") => {
            super::fleet_social_corp_gov::handle_add_corporation(ctx, session, args)
        }
        ("corpmgr", m) if m.eq_ignore_ascii_case("createcorporation") => {
            super::fleet_social_corp_gov::handle_add_corporation(ctx, session, args)
        }

        ("corpRegistry", "GetInfoWindowDataForChar") => {
            crate::services::corp_registry_ops::handle_get_info_window_data_for_char(ctx, session, args)
        }

        ("corpmgr", "GetPublicInfo")
        | ("corpRegistry", "GetCorporation")
        | ("corpmgr", "GetCorporations")
        | ("corporationSvc", "GetCorpInfo") => {
            if service == "corporationSvc" && method == "GetCorpInfo" {
                if let Some(res) = crate::services::corp_registry_ops::handle_get_corp_market_info(ctx, session, args) {
                    return Some(res);
                }
            }
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(1_000_009);
            let corp_info = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).map(|c| {
                    let h_divs = snap.corp_hangar_divisions.get(&corp_id).cloned().unwrap_or_default();
                    let w_divs: Vec<(u8, String)> = c.wallet_divisions.iter().map(|d| (d.division_id, d.name.clone())).collect();
                    (
                        c.corporation_id, c.name.clone(), c.ticker.clone(), c.ceo_id,
                        c.members.len() as i64, c.tax_rate, c.alliance_id,
                        c.hq_station_id.unwrap_or(60_003_760), c.description.clone(), c.url.clone(),
                        h_divs, w_divs,
                    )
                })
            }).or_else(|| {
                // DERIVED: SDE NPC corporations fallback for starter/NPC corps
                ctx.sde.npc_corporations.get(&corp_id).map(|c| {
                    (
                        c.corporation_id, c.name.clone(), c.ticker_name.clone(), 0i64,
                        10_000i64, c.tax_rate, None, c.station_id.unwrap_or(60_003_760),
                        format!("NPC Corporation {}", c.name), String::new(),
                        HashMap::new(), Vec::new(),
                    )
                })
            });

            let Some((cid, name, ticker, ceo_id, m_count, tax, alliance_id, hq_station, desc, url, h_divs, w_divs)) = corp_info else {
                return Some(Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                }));
            };
            let ((s1, s2, s3, c1), (c2, c3), apps_enabled) = read_corp_gov(ctx, |g| {
                g.get(&cid)
                    .map(|s| (s.logo, s.logo_extra_colors, s.applications_enabled.unwrap_or(true)))
                    .unwrap_or(((1, 2, 3, 10), (0, 0), true))
            });
            let default_h = ["1st Division", "2nd Division", "3rd Division", "4th Division", "5th Division", "6th Division", "7th Division"];
            let default_w = ["Master Wallet", "2nd Wallet", "3rd Wallet", "4th Wallet", "5th Wallet", "6th Wallet", "7th Wallet"];
            let mut entries = vec![
                ("corporationID".to_string(), PyRep::Integer(cid)),
                ("corporationName".into(), PyRep::String(name)),
                ("ticker".into(), PyRep::String(ticker.clone())),
                ("tickerName".into(), PyRep::String(ticker)),
                ("ceoID".into(), PyRep::Integer(ceo_id)),
                ("memberCount".into(), PyRep::Integer(m_count)),
                ("taxRate".into(), PyRep::Real(tax)),
                ("allianceID".into(), alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("stationID".into(), PyRep::Integer(hq_station)),
                ("description".into(), PyRep::String(desc)),
                ("url".into(), PyRep::String(url)),
                ("shares".into(), PyRep::Integer(1000)),
                ("shape1".into(), PyRep::Integer(s1)),
                ("shape2".into(), PyRep::Integer(s2)),
                ("shape3".into(), PyRep::Integer(s3)),
                ("color1".into(), PyRep::Integer(c1)),
                ("color2".into(), PyRep::Integer(c2)),
                ("color3".into(), PyRep::Integer(c3)),
                ("applicationsEnabled".into(), PyRep::Bool(apps_enabled)),
                ("isRecruiting".into(), PyRep::Bool(apps_enabled)),
                ("deleted".into(), PyRep::Integer(0)),
            ];
            for i in 1..=7u8 {
                let h_name = h_divs.get(&i).cloned().unwrap_or_else(|| default_h[(i - 1) as usize].into());
                let w_name = w_divs.iter().find(|(id, _)| *id == i).map(|(_, n)| n.clone()).unwrap_or_else(|| default_w[(i - 1) as usize].into());
                entries.push((format!("division{i}"), PyRep::String(h_name)));
                entries.push((format!("walletDivision{i}"), PyRep::String(w_name)));
            }
            Some(Ok(PyRep::KeyVal(entries.into_iter().collect())))
        }

        ("corpmgr", "allianceID")
        | ("corpmgr", "ceoID")
        | ("corpmgr", "corporationID") => {
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(1_000_009);
            let info = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).map(|c| (c.corporation_id, c.ceo_id, c.alliance_id, c.hq_station_id.unwrap_or(60_003_760)))
            }).or_else(|| {
                ctx.sde.npc_corporations.get(&corp_id).map(|c| (c.corporation_id, 0i64, None, c.station_id.unwrap_or(60_003_760)))
            });
            let (cid, ceo, aid, _st) = info.unwrap_or((corp_id, 0, None, 60_003_760));
            let rep = match method {
                "allianceID" => aid.map(PyRep::Integer).unwrap_or(PyRep::None),
                "ceoID" => PyRep::Integer(ceo),
                "corporationID" => PyRep::Integer(cid),
                _ => PyRep::None,
            };
            Some(Ok(rep))
        }

        ("corpmgr", "GetCorporationIDForCharacter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let cid = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&target_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009)
            });
            Some(Ok(PyRep::Integer(cid)))
        }

        ("corpRegistry", "UpdateCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NoCorporation".into(),
                        message: "Character has no active corporation".into(),
                    }))
                }
            };
            let (desc, url, tax_rate_opt) = if args.first().and_then(|a| a.as_i64()).is_some() {
                (args.get(1).and_then(|a| a.as_str()).unwrap_or(""), args.get(2).and_then(|a| a.as_str()).unwrap_or(""), args.get(3).and_then(|a| a.as_f64()))
            } else {
                (args.first().and_then(|a| a.as_str()).unwrap_or(""), args.get(1).and_then(|a| a.as_str()).unwrap_or(""), args.get(2).and_then(|a| a.as_f64()))
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Only CEO or Director can update corporation details".into(),
                    });
                }
                let (old_desc, old_url, old_tax) = (corp.description.clone(), corp.url.clone(), corp.tax_rate);
                corp.description = desc.to_string();
                corp.url = url.to_string();
                if let Some(t) = tax_rate_opt {
                    corp.tax_rate = t.clamp(0.0, 1.0);
                }
                Ok((old_desc, old_url, old_tax, corp.description.clone(), corp.url.clone(), corp.tax_rate))
            });

            match res {
                Ok((old_desc, old_url, old_tax, desc, url, tax)) => {
                    let change = PyRep::Dict(vec![
                        (PyRep::String(("description").into()), PyRep::Tuple(vec![PyRep::String(old_desc), PyRep::String(desc.clone())])),
                        (PyRep::String(("url").into()), PyRep::Tuple(vec![PyRep::String(old_url), PyRep::String(url.clone())])),
                        (PyRep::String(("taxRate").into()), PyRep::Tuple(vec![PyRep::Real(old_tax), PyRep::Real(tax)])),
                    ]);
                    ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)), ("description", PyRep::String(desc)),
                        ("url", PyRep::String(url)), ("taxRate", PyRep::Real(tax)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "UpdateLogo") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let s1 = args.first().and_then(|a| a.as_i64()).unwrap_or(10);
            let s2 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(20);
            let s3 = args.get(2).and_then(|a| a.as_i64()).unwrap_or(30);
            let c1 = args.get(3).and_then(|a| a.as_i64()).unwrap_or(5);
            let is_ceo_or_dir = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_some_and(|c| {
                    char_id == c.ceo_id || corp_roles::has_role(c.member_roles.get(&char_id).copied().unwrap_or(0), corp_roles::ROLE_DIRECTOR)
                })
            });
            if !is_ceo_or_dir {
                return Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update the corporation logo".into() }));
            }
            let old_logo = with_corp_gov(ctx, |g| {
                let state = g.entry(corp_id).or_default();
                std::mem::replace(&mut state.logo, (s1, s2, s3, c1))
            });
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorpLogoReady", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(128)]);
            let change = PyRep::Dict(vec![
                (PyRep::String(("shape1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.0), PyRep::Integer(s1)])),
                (PyRep::String(("shape2").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.1), PyRep::Integer(s2)])),
                (PyRep::String(("shape3").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.2), PyRep::Integer(s3)])),
                (PyRep::String(("color1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.3), PyRep::Integer(c1)])),
            ]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)), ("shape1", PyRep::Integer(s1)),
                ("shape2", PyRep::Integer(s2)), ("shape3", PyRep::Integer(s3)), ("color1", PyRep::Integer(c1)),
            ])))
        }

        ("corpRegistry", "UpdateDivisionNames")
        | ("corpmgr", "UpdateDivisionNames")
        | ("corporationSvc", "UpdateDivisionNames") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(corp_id) = session.corporation_id else {
                return Some(Err(BridgeRpcError { code: "NoCorporation".into(), message: "Character has no active corporation".into() }));
            };
            if args.len() >= 14 || args.first().and_then(|a| a.as_str()).is_some() {
                let default_h = ["1st Division", "2nd Division", "3rd Division", "4th Division", "5th Division", "6th Division", "7th Division"];
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                        return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                    };
                    let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                        return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update division names".into() });
                    }
                    corp.ensure_wallet_divisions();
                    let mut diff = Vec::new();
                    let h_divs = snap.corp_hangar_divisions.entry(corp_id).or_default();
                    for i in 0..7usize {
                        let d_id = (i + 1) as u8;
                        if let Some(h_name) = args.get(i).and_then(|a| a.as_str()) {
                            let old_h = h_divs.get(&d_id).cloned().unwrap_or_else(|| default_h[i].to_string());
                            if old_h != h_name {
                                diff.push((PyRep::String(format!("division{d_id}")), PyRep::Tuple(vec![PyRep::String(old_h), PyRep::String(h_name.to_string())])));
                            }
                            h_divs.insert(d_id, h_name.to_string());
                        }
                    }
                    for i in 0..7usize {
                        let d_id = (i + 1) as u8;
                        if let Some(w_name) = args.get(7 + i).and_then(|a| a.as_str()) {
                            if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == d_id) {
                                let old_w = std::mem::replace(&mut div.name, w_name.to_string());
                                if old_w != w_name {
                                    diff.push((PyRep::String(format!("walletDivision{d_id}")), PyRep::Tuple(vec![PyRep::String(old_w), PyRep::String(w_name.to_string())])));
                                }
                            }
                        }
                    }
                    Ok(diff)
                });
                return match res {
                    Ok(diff) => {
                        let first_h = args.first().and_then(|a| a.as_str()).unwrap_or("1st Division").to_string();
                        if !diff.is_empty() {
                            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Dict(diff)]);
                        }
                        Some(Ok(PyRep::keyval([("divisionID", PyRep::Integer(1)), ("name", PyRep::String(first_h))])))
                    }
                    Err(e) => Some(Err(e)),
                };
            }
            let div_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Division");
            let wallet_name = args.get(2).and_then(|a| a.as_str()).unwrap_or(new_name);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update division names".into() });
                }
                let old_w = if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                    std::mem::replace(&mut div.name, wallet_name.to_string())
                } else {
                    return Err(BridgeRpcError { code: "DivisionNotFound".into(), message: format!("Division {div_id} not found") });
                };
                snap.corp_hangar_divisions.entry(corp_id).or_default().insert(div_id, new_name.to_string());
                Ok(old_w)
            });

            match res {
                Ok(old_name) => {
                    let change = PyRep::Dict(vec![(
                        PyRep::String(format!("division{div_id}")),
                        PyRep::Tuple(vec![PyRep::String(old_name), PyRep::String(new_name.to_string())]),
                    )]);
                    ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
                    Some(Ok(PyRep::keyval([
                        ("divisionID", PyRep::Integer(div_id as i64)),
                        ("name", PyRep::String(new_name.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "GetMember") if !args.first().and_then(|a| a.as_i64()).is_some_and(|id| id >= 98_000_000) => {
            crate::services::corp_registry_ops::handle_get_member(ctx, session, args)
        }
        ("corpRegistry", "GetMemberTrackingInfo")
        | ("corpRegistry", "GetMemberTrackingInfoSimple") => {
            crate::services::corp_registry_ops::handle_get_member_tracking_info(ctx, session, method, args)
        }

        ("corpRegistry", "GetMembers")
        | ("corpRegistry", "GetMember")
        | ("corpmgr", "GetMembers")
        | ("corporationSvc", "GetMembers") => {
            let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || id == 1_000_009);
            let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
            let rows: Vec<Vec<PyRep>> = members.iter().map(member_row_rep).collect();
            Some(Ok(PyRep::rowset(MEMBER_COLS.to_vec(), rows)))
        }

        ("corpRegistry", "GetMembersPaged") => {
            let (target_cid, page, page_size) = if let Some(cid) = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || id == 1_000_009) {
                (
                    Some(cid),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as usize,
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(50).max(1) as usize,
                )
            } else {
                (
                    None,
                    args.first().and_then(|a| a.as_i64()).unwrap_or(1).max(1) as usize,
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(50).max(1) as usize,
                )
            };
            let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
            let offset = (page.saturating_sub(1)) * page_size;
            let rows: Vec<Vec<PyRep>> = members.iter().skip(offset).take(page_size).map(member_row_rep).collect();
            Some(Ok(PyRep::rowset(MEMBER_COLS.to_vec(), rows)))
        }

        ("corpRegistry", "GetMembersByIds") => {
            let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || id == 1_000_009);
            let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_ids: HashSet<i64> = match args.get(1).or_else(|| args.first()) {
                Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
                _ => HashSet::new(),
            };
            let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
            let rows: Vec<Vec<PyRep>> = members.iter().filter(|m| target_ids.contains(&m.0)).map(member_row_rep).collect();
            Some(Ok(PyRep::rowset(MEMBER_COLS.to_vec(), rows)))
        }

        ("corpRegistry", "GetMemberIDsByQuery") => {
            let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || id == 1_000_009);
            let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let query = args.iter().find_map(|a| match a { PyRep::String(s) => Some(s.to_lowercase()), _ => None })
                .or_else(|| kwargs.get("query").and_then(|v| match v { PyRep::String(s) => Some(s.to_lowercase()), _ => None }))
                .unwrap_or_default();
            let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
            let rows: Vec<Vec<PyRep>> = members.into_iter()
                .filter(|m| query.is_empty() || m.1.to_lowercase().contains(&query) || m.0.to_string().contains(&query))
                .map(|m| vec![PyRep::Integer(m.0)]).collect();
            Some(Ok(PyRep::rowset(vec!["characterID"], rows)))
        }

        ("corpRegistry", "UpdateMember") | ("corpRegistry", "UpdateMembers") => {
            super::fleet_social_persistence::handle_update_members(ctx, session, method, args)
        }

        ("corpmgr", "AuditMember") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let target_id = args.first().and_then(|a| a.as_i64());
            let rows: Vec<Vec<PyRep>> = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|g| {
                        g.role_audit_log
                            .iter()
                            .filter(|(tid, _, _, _, _)| target_id.is_none_or(|t| t == *tid))
                            .map(|&(tid, issuer_id, old_r, new_r, ts)| {
                                vec![
                                    PyRep::Integer(tid),
                                    PyRep::Integer(issuer_id),
                                    PyRep::Integer(old_r as i64),
                                    PyRep::Integer(new_r as i64),
                                    PyRep::Integer(ts as i64),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["characterID", "issuerID", "oldRoles", "newRoles", "changeTime"],
                rows,
            )))
        }

        ("corpRegistry", "InsertApplication") | ("corpmgr", "ApplyForMembership") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(corp_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "InsertApplication requires target corporationID".into(),
                }));
            };
            let message = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("o7 requesting to join");
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                if corp.members.contains(&char_id) {
                    return Err(BridgeRpcError {
                        code: "AlreadyInCorporation".into(),
                        message: "Character is already a member of this corporation".into(),
                    });
                }
                snap.next_entity_id += 1;
                let app_id = snap.next_entity_id;
                corp.applications.insert(
                    app_id,
                    CorpApplicationRecord {
                        application_id: app_id,
                        character_id: char_id,
                        corporation_id: corp_id,
                        message: message.to_string(),
                        created_unix: now_unix,
                        status: "Applied".into(),
                    },
                );
                Ok(app_id)
            });
            match res {
                Ok(app_id) => {
                    let app_kv = PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("characterID", PyRep::Integer(char_id)),
                        ("applicationText", PyRep::String(message.to_string())),
                        ("status", PyRep::String("Applied".into())),
                        ("applicationDateTime", PyRep::Integer(now_unix as i64)),
                    ]);
                    let app_args = vec![
                        PyRep::Integer(corp_id),
                        PyRep::Integer(char_id),
                        PyRep::Integer(app_id),
                        app_kv,
                    ];
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorporationApplicationChanged",
                        "corpid",
                        app_args.clone(),
                    );
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnCorporationApplicationChanged",
                        "charid",
                        app_args,
                    );
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("status", PyRep::String("Applied".into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "GetApplications")
        | ("corpmgr", "GetApplications")
        | ("corporationSvc", "GetApplications")
        | ("corpRegistry", "GetMyApplications")
        | ("corpRegistry", "GetOldApplications")
        | ("corpRegistry", "GetMyOldApplications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_old = matches!(method, "GetOldApplications" | "GetMyOldApplications");
            let status_matches = |status: &str| -> bool {
                let is_open = matches!(status, "Applied" | "Pending" | "Invited");
                if is_old { !is_open } else { is_open }
            };
            if matches!(method, "GetMyApplications" | "GetMyOldApplications") {
                let rows = ctx.db.with_snapshot(|snap| {
                    let mut out = Vec::new();
                    for corp in snap.corporations.values() {
                        for app in corp.applications.values() {
                            if app.character_id == char_id && status_matches(&app.status) {
                                out.push(vec![
                                    PyRep::Integer(app.application_id), PyRep::Integer(app.corporation_id),
                                    PyRep::Integer(app.character_id), PyRep::String(app.message.clone()),
                                    PyRep::String(app.status.clone()), PyRep::Integer(app.created_unix as i64),
                                ]);
                            }
                        }
                    }
                    out
                });
                return Some(Ok(PyRep::rowset(
                    vec!["applicationID", "corporationID", "characterID", "message", "status", "createdDate"],
                    rows,
                )));
            }
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER) {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Viewing applications requires Personnel Manager or Director role".into() });
                }
                let rows: Vec<Vec<PyRep>> = corp
                    .applications
                    .values()
                    .filter(|app| status_matches(&app.status))
                    .map(|app| {
                        vec![
                            PyRep::Integer(app.application_id),
                            PyRep::Integer(app.character_id),
                            PyRep::String(app.message.clone()),
                            PyRep::String(app.status.clone()),
                            PyRep::Integer(app.created_unix as i64),
                        ]
                    })
                    .collect();
                Ok(rows)
            });
            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(vec!["applicationID", "characterID", "message", "status", "createdDate"], rows))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "UpdateApplicationOffer") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(app_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "UpdateApplicationOffer requires applicationID".into(),
                }));
            };
            let action = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Accepted");
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Find the corporation owning app_id
                let corp_id = snap
                    .corporations
                    .values()
                    .find(|c| c.applications.contains_key(&app_id))
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .ok_or_else(|| BridgeRpcError {
                        code: "NoCorporation".into(),
                        message: "Application corporation not found".into(),
                    })?;
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let ceo_id = corp.ceo_id;
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                let (applicant_id, app_msg, app_ts) = corp
                    .applications
                    .get(&app_id)
                    .map(|a| (a.character_id, a.message.clone(), a.created_unix))
                    .ok_or_else(|| BridgeRpcError {
                        code: "ApplicationNotFound".into(),
                        message: format!("Application {app_id} not found"),
                    })?;
                let old_corp_id = snap
                    .characters
                    .get(&applicant_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009);

                if action.eq_ignore_ascii_case("withdrawn") {
                    if char_id != applicant_id {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Only the applicant can withdraw their application".into(),
                        });
                    }
                    let corp_mut = snap.corporations.get_mut(&corp_id).unwrap();
                    if let Some(app) = corp_mut.applications.get_mut(&app_id) {
                        app.status = "Withdrawn".into();
                    }
                    return Ok((corp_id, applicant_id, old_corp_id, "Withdrawn".to_string(), app_msg, app_ts));
                }

                if char_id != ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message:
                            "Updating applications requires Personnel Manager or Director role"
                                .into(),
                    });
                }

                if action.eq_ignore_ascii_case("rejected") {
                    let corp_mut = snap.corporations.get_mut(&corp_id).unwrap();
                    if let Some(app) = corp_mut.applications.get_mut(&app_id) {
                        app.status = "Rejected".into();
                    }
                    return Ok((corp_id, applicant_id, old_corp_id, "Rejected".to_string(), app_msg, app_ts));
                }

                let max_members = compute_corp_member_limit(ctx, snap, corp_id, ceo_id);
                if corp.members.len() >= max_members {
                    return Err(BridgeRpcError {
                        code: "CorporationFull".into(),
                        message: format!(
                            "Corporation has reached its CEO skill member limit of {max_members}"
                        ),
                    });
                }

                let corp_mut = snap.corporations.get_mut(&corp_id).unwrap();
                if let Some(app) = corp_mut.applications.get_mut(&app_id) {
                    app.status = "Accepted".into();
                }
                corp_mut.members.insert(applicant_id);
                corp_mut.member_roles.insert(applicant_id, 0);
                let alliance_id = corp_mut.alliance_id;
                if let Some(ch) = snap.characters.get_mut(&applicant_id) {
                    ch.corporation_id = corp_id;
                    ch.alliance_id = alliance_id;
                }
                if let Some(ch_rec) = snap.chat_channels.get_mut(&format!("corpid:{corp_id}")) {
                    ch_rec.members.insert(applicant_id);
                }

                // Deliver corp welcome mail if enabled
                let welcome = snap
                    .corp_governance
                    .get(&corp_id)
                    .map(|g| g.welcome_mail.clone())
                    .unwrap_or((false, String::new(), String::new()));
                if welcome.0 && !welcome.1.is_empty() {
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.mails.push(MailRecord { message_id: mid, sender_id: ceo_id, recipient_id: applicant_id, subject: welcome.1, body: welcome.2 });
                    snap.mail_timestamps.insert(mid, now_unix);
                    snap.mail_assigned_labels.insert((applicant_id, mid), vec![1]);
                }
                record_corp_join_in_snap(snap, applicant_id, old_corp_id, corp_id, now_unix);

                Ok((corp_id, applicant_id, old_corp_id, "Accepted".to_string(), app_msg, app_ts))
            });

            match res {
                Ok((corp_id, applicant_id, old_corp_id, status_str, app_msg, app_ts)) => {
                    if status_str == "Accepted" {
                        let m_change = PyRep::Dict(vec![(PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::Integer(old_corp_id), PyRep::Integer(corp_id)]))]);
                        ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMemberChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(applicant_id), m_change]);
                    }
                    let app_kv = PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)), ("corporationID", PyRep::Integer(corp_id)),
                        ("characterID", PyRep::Integer(applicant_id)), ("applicationText", PyRep::String(app_msg)),
                        ("status", PyRep::String(status_str.clone())), ("applicationDateTime", PyRep::Integer(app_ts as i64)),
                    ]);
                    let app_args = vec![PyRep::Integer(corp_id), PyRep::Integer(applicant_id), PyRep::Integer(app_id), app_kv.clone()];
                    if status_str != "Accepted" {
                        ctx.notify(
                            NotificationTarget::Character(applicant_id),
                            "OnCorporationApplicationChanged",
                            "charid",
                            app_args.clone(),
                        );
                    }
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorporationApplicationChanged",
                        "corpid",
                        app_args,
                    );
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("status", PyRep::String(status_str)),
                        ("characterID", PyRep::Integer(applicant_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "InsertInvitation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let accept_now = matches!(args.get(1), Some(PyRep::Bool(true)));
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let ceo_id = corp.ceo_id;
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Inviting characters requires Personnel Manager role".into(),
                    });
                }
                let max_members = compute_corp_member_limit(ctx, snap, corp_id, ceo_id);
                if corp.members.len() >= max_members {
                    return Err(BridgeRpcError {
                        code: "CorporationFull".into(),
                        message: format!(
                            "Corporation has reached its CEO skill member limit of {max_members}"
                        ),
                    });
                }
                snap.next_entity_id += 1;
                let app_id = snap.next_entity_id;
                let corp = snap.corporations.get_mut(&corp_id).unwrap();
                corp.applications.insert(
                    app_id,
                    CorpApplicationRecord {
                        application_id: app_id,
                        character_id: target_id,
                        corporation_id: corp_id,
                        message: "Corporation Invitation".into(),
                        created_unix: now_unix,
                        status: if accept_now { "Accepted".into() } else { "Invited".into() },
                    },
                );
                if accept_now {
                    corp.members.insert(target_id);
                    corp.member_roles.insert(target_id, 0);
                    let alliance_id = corp.alliance_id;
                    let old_corp_id = snap.characters.get(&target_id).map(|c| c.corporation_id).unwrap_or(1_000_009);
                    if let Some(ch) = snap.characters.get_mut(&target_id) {
                        ch.corporation_id = corp_id;
                        ch.alliance_id = alliance_id;
                    }
                    record_corp_join_in_snap(snap, target_id, old_corp_id, corp_id, now_unix);
                }
                Ok(app_id)
            });
            match res {
                Ok(app_id) => {
                    let inv_status = if accept_now { "Accepted" } else { "Invited" };
                    let app_kv = PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("characterID", PyRep::Integer(target_id)),
                        ("applicationText", PyRep::String(("Corporation Invitation").into())),
                        ("status", PyRep::String((inv_status).into())),
                        ("applicationDateTime", PyRep::Integer(now_unix as i64)),
                    ]);
                    ctx.notify(
                        NotificationTarget::Character(target_id),
                        "OnCorporationApplicationChanged",
                        "charid",
                        vec![
                            PyRep::Integer(corp_id),
                            PyRep::Integer(target_id),
                            PyRep::Integer(app_id),
                            app_kv,
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("characterID", PyRep::Integer(target_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("joined", PyRep::Bool(accept_now)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "CanLeaveCurrentCorporation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(1_000_009);
            let _now_unix = ctx.now_unix();
            let (can_leave, reason) = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return (false, "NPC Corporation");
                };
                if corp.ceo_id == char_id && corp.members.len() > 1 {
                    return (false, "CEO cannot leave while other members remain");
                }
                let has_active_war = snap.wars.values().any(|w| {
                    w.active
                        && (w.aggressor_corp_id == corp_id
                            || w.defender_corp_id == corp_id
                            || corp.alliance_id.is_some_and(|aid| {
                                w.aggressor_corp_id == aid || w.defender_corp_id == aid
                            }))
                });
                if corp.members.len() == 1 && has_active_war {
                    return (false, "Corporation has active wars");
                }
                (true, "OK")
            });
            Some(Ok(PyRep::keyval([
                ("canLeave", PyRep::Bool(can_leave)),
                ("reason", PyRep::String(reason.into())),
            ])))
        }

        ("corpRegistry", "CanBeKickedOut") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let corp_id = session.corporation_id.unwrap_or(0);
            let can_kick = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_some_and(|c| {
                    target_id != c.ceo_id
                        && c.member_roles.get(&target_id).copied().unwrap_or(0) == 0
                })
            });
            Some(Ok(PyRep::Bool(can_kick)))
        }

        ("corpRegistry", "KickOutMember") | ("corpRegistry", "KickOutMembers") => {
            super::fleet_social_persistence::handle_kick_out_members(ctx, session, args)
        }

        ("corpRegistry", "GetPendingAutoKicks") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let pending: Vec<(i64, u64)> = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|g| g.pending_auto_kicks.iter().map(|(&k, &v)| (k, v)).collect())
                    .unwrap_or_default()
            });
            let rows: Vec<Vec<PyRep>> = pending
                .into_iter()
                .map(|(cid, ts)| vec![PyRep::Integer(cid), PyRep::Integer(ts as i64)])
                .collect();
            Some(Ok(PyRep::rowset(vec!["characterID", "queuedDateTime"], rows)))
        }

        ("account", "GetWalletDivisionsInfo") => {
            super::fleet_social_corp_gov::handle_get_wallet_divisions_info(ctx, session, args)
        }
        ("corpmgr", m) if m.eq_ignore_ascii_case("getwalletdivisions") => {
            super::fleet_social_corp_gov::handle_get_wallet_divisions_info(ctx, session, args)
        }

        ("account", "GiveCashFromCorpAccount") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NoCorporation".into(),
                        message: "Character has no active corporation".into(),
                    }))
                }
            };
            let from_div = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let to_div = args.get(1).and_then(|a| a.as_i64()).unwrap_or(2) as u8;
            let amount_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
            if amount_isk <= 0.0 || !amount_isk.is_finite() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Transfer amount must be positive and finite".into(),
                }));
            }
            let transfer_amount = IskCents::from_f64_isk(amount_isk);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message:
                            "Transferring between corp wallet divisions requires Accountant role"
                                .into(),
                    });
                }
                corp.ensure_wallet_divisions();
                let from_pos = corp
                    .wallet_divisions
                    .iter()
                    .position(|d| d.division_id == from_div)
                    .ok_or_else(|| BridgeRpcError {
                        code: "DivisionNotFound".into(),
                        message: format!("Source wallet division {from_div} not found"),
                    })?;
                let to_pos = corp
                    .wallet_divisions
                    .iter()
                    .position(|d| d.division_id == to_div)
                    .ok_or_else(|| BridgeRpcError {
                        code: "DivisionNotFound".into(),
                        message: format!("Target wallet division {to_div} not found"),
                    })?;
                let source_bal = corp.wallet_divisions[from_pos].balance;
                let new_source =
                    source_bal
                        .checked_sub(transfer_amount)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientDivisionFunds".into(),
                            message: "Insufficient funds in source wallet division".into(),
                        })?;
                let new_target = corp.wallet_divisions[to_pos]
                    .balance
                    .checked_add(transfer_amount)
                    .unwrap_or(corp.wallet_divisions[to_pos].balance);
                corp.wallet_divisions[from_pos].balance = new_source;
                corp.wallet_divisions[to_pos].balance = new_target;
                if from_div == 1 {
                    corp.master_wallet = new_source;
                }
                if to_div == 1 {
                    corp.master_wallet = new_target;
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("fromDivisionID", PyRep::Integer(from_div as i64)),
                    ("toDivisionID", PyRep::Integer(to_div as i64)),
                    ("transferredISK", PyRep::Real(amount_isk)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => super::fleet_social_corp_gov::handle_corp_gov(ctx, session, service, method, args, kwargs),
    }
}

pub(super) fn handle_insert_vote_case(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let args = if args.first().and_then(|a| a.as_i64()).is_some() && args.get(1).and_then(|a| a.as_str()).is_some() {
        &args[1..]
    } else {
        args
    };
    let text = args.first().and_then(|a| a.as_str()).unwrap_or("CEO Election");
    let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Vote for new CEO");
    let vote_type = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    let opts_arg = args
        .get(3)
        .filter(|a| matches!(a, PyRep::List(_) | PyRep::Tuple(_) | PyRep::Rowset { .. }))
        .or_else(|| {
            args.get(4)
                .filter(|a| matches!(a, PyRep::List(_) | PyRep::Tuple(_) | PyRep::Rowset { .. }))
        });
    let (case_id, target_id) = match ctx
        .db
        .try_with_mut_snapshot(|snap| -> Result<(i64, i64), BridgeRpcError> {
            let mut options: Vec<(i64, String, i64)> = Vec::new();
            if let Some(opt_rep) = opts_arg {
                let items: Vec<PyRep> = match opt_rep {
                    PyRep::List(v) | PyRep::Tuple(v) => v.clone(),
                    PyRep::Rowset { rows, .. } => {
                        rows.iter().map(|r| PyRep::List(r.clone())).collect()
                    }
                    _ => Vec::new(),
                };
                for (idx, item) in items.iter().enumerate() {
                    let oid = (idx as i64) + 1;
                    match item {
                        PyRep::Integer(cand_id) => {
                            if vote_type == 0 && !snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(cand_id)) {
                                return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate must be a member of the corporation".into() });
                            }
                            let cname = snap.characters.get(cand_id).map(|c| c.name.clone()).unwrap_or_else(|| format!("Candidate {cand_id}"));
                            options.push((oid, cname, *cand_id));
                        }
                        PyRep::List(els) | PyRep::Tuple(els) => {
                            let (opt_id, otext, param) = if els.first().and_then(|x| x.as_str()).is_some() {
                                (oid, els.first().and_then(|x| x.as_str()).unwrap_or("Option").to_string(), els.get(1).and_then(|x| x.as_i64()).unwrap_or(0))
                            } else {
                                let id = els.first().and_then(|x| x.as_i64()).unwrap_or(oid);
                                let txt = els.get(1).and_then(|x| x.as_str()).unwrap_or("Option").to_string();
                                let p = els.get(2).and_then(|x| x.as_i64()).unwrap_or(id);
                                (id, txt, p)
                            };
                            if vote_type == 0 && param > 0 && !snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(&param)) {
                                return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate must be a member of the corporation".into() });
                            }
                            options.push((opt_id, otext, param));
                        }
                        PyRep::String(s) => options.push((oid, s.clone(), oid)),
                        _ => {}
                    }
                }
            }
            let target_id = args.get(3).and_then(|a| a.as_i64()).or_else(|| options.first().map(|o| o.2)).unwrap_or(char_id);
            if options.is_empty() {
                if vote_type == 0 {
                    if !snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(&target_id)) {
                        return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate must be a member of the corporation".into() });
                    }
                    let cname = snap.characters.get(&target_id).map(|c| c.name.clone()).unwrap_or_else(|| format!("Candidate {target_id}"));
                    options = vec![(1, format!("Vote for {cname}"), target_id), (2, "Reject".into(), 0)];
                } else {
                    options = vec![(1, "Yes / Accept".into(), target_id), (2, "No / Reject".into(), 0)];
                }
            }
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            snap.corp_vote_cases.insert(
                cid,
                eve_db::CorpVoteCaseRecord {
                    case_id: cid,
                    corporation_id: corp_id,
                    vote_type,
                    text: text.to_string(),
                    description: desc.to_string(),
                    parameter: target_id,
                    start_ts: now_unix,
                    end_ts: now_unix + 86_400,
                    executed: false,
                    options,
                },
            );
            Ok((cid, target_id))
        }) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnVoteCaseChanged",
        "corpid",
        vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)],
    );
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnSanctionedActionChanged",
        "corpid",
        vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)],
    );
    Some(Ok(PyRep::keyval([
        ("voteCaseID", PyRep::Integer(case_id)),
        ("voteType", PyRep::Integer(vote_type)),
        ("parameter", PyRep::Integer(target_id)),
    ])))
}
