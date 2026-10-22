//! Corporation Core Services (`SO-3`): `corpRegistry`, `corpmgr`, `corporationSvc`, `corpStationMgr`, `account`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{corp_roles, CorpApplicationRecord, IskCents, MailRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug, Default)]
pub(super) struct CorpGovState {
    pub(super) role_audit_log: Vec<(i64, i64, u64, u64, u64)>, // (target_id, issuer_id, old_roles, new_roles, timestamp)
    pub(super) pending_role_grants: HashMap<i64, (u64, u64)>,  // char_id -> (pending_roles, effective_unix)
    pub(super) pending_auto_kicks: HashMap<i64, u64>,          // char_id -> queued_unix
    pub(super) titles: HashMap<i64, (String, u64)>,            // title_id -> (title_name, roles_mask)
    pub(super) member_titles: HashMap<i64, u64>,               // char_id -> title_bitmask
    pub(super) shares: HashMap<i64, i64>,                      // owner_id -> share_count
    pub(super) company_shares: i64,
    pub(super) bulletins: Vec<(i64, String, String, i64, u64)>, // (bulletin_id, title, body, author_id, timestamp)
    pub(super) recruitment_ads: Vec<(i64, String, String, u64, i64)>, // (ad_id, title, desc, mask, recruiter_id)
    pub(super) contacts: HashMap<i64, (f64, u64)>,             // contact_id -> (standing, label_mask)
    pub(super) labels: HashMap<i64, (String, u32)>,            // label_id -> (name, color)
    pub(super) welcome_mail: (bool, String, String),           // (enabled, subject, body)
    pub(super) restrict_corp_mails: bool,
    pub(super) accept_structures: bool,
    pub(super) reinforce_hour: u8,
    pub(super) friendly_fire_legal: bool,
    pub(super) pending_friendly_fire: Option<(bool, u64)>,
    pub(super) member_limit: Option<usize>,
    #[allow(dead_code)]
    pub(super) offices: HashMap<i64, i64>, // station_id -> office_folder_id
    pub(super) logo: (i64, i64, i64, i64),
    pub(super) medals: Vec<(i64, String, String, i64, u64)>,   // (medal_id, title, desc, creator_id, created_unix)
    pub(super) medal_recipients: Vec<(i64, i64, i64, String, i64)>, // (medal_id, recipient_id, issuer_id, reason, status)
    pub(super) vote_cases: Vec<(i64, String, String, i64, i64, u64, bool)>, // (case_id, text, desc, vote_type, target_id, end_unix, acted)
    pub(super) votes: HashMap<(i64, i64), (i64, i64)>,         // (case_id, char_id) -> (option_id, weight)
}

pub(super) static CORP_GOV: LazyLock<Mutex<HashMap<i64, CorpGovState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) static EMP_HISTORY: LazyLock<Mutex<HashMap<i64, Vec<(i64, u64, Option<u64>)>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn get_character_medals_and_decorations(char_id: i64) -> (Vec<PyRep>, Vec<PyRep>) {
    let g = CORP_GOV.lock().unwrap();
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
    if let Some(lim) = CORP_GOV
        .lock()
        .unwrap()
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

pub(super) fn record_corp_join(char_id: i64, corp_id: i64, now_unix: u64) {
    let mut emp = EMP_HISTORY.lock().unwrap();
    let list = emp.entry(char_id).or_default();
    if let Some(last) = list.last_mut() {
        if last.2.is_none() {
            last.2 = Some(now_unix);
        }
    }
    list.push((corp_id, now_unix, None));
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

        ("corpmgr", "GetPublicInfo")
        | ("corporationSvc", "GetCorpInfo")
        | ("corpRegistry", "GetCorporation")
        | ("corpRegistry", "GetInfoWindowDataForChar")
        | ("corpmgr", "GetCorporations") => {
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
                    (
                        c.corporation_id,
                        c.name.clone(),
                        c.ticker.clone(),
                        c.ceo_id,
                        c.members.len() as i64,
                        c.tax_rate,
                        c.alliance_id,
                        c.hq_station_id.unwrap_or(60_003_760),
                        c.description.clone(),
                        c.url.clone(),
                    )
                })
            }).or_else(|| {
                // DERIVED: SDE NPC corporations fallback for starter/NPC corps
                ctx.sde.npc_corporations.get(&corp_id).map(|c| {
                    (
                        c.corporation_id,
                        c.name.clone(),
                        c.ticker_name.clone(),
                        0i64,
                        10_000i64,
                        c.tax_rate,
                        None,
                        c.station_id.unwrap_or(60_003_760),
                        format!("NPC Corporation {}", c.name),
                        String::new(),
                    )
                })
            });

            let Some((cid, name, ticker, ceo_id, m_count, tax, alliance_id, hq_station, desc, url)) =
                corp_info
            else {
                return Some(Err(BridgeRpcError {
                    code: "CorporationNotFound".into(),
                    message: format!("Corporation {corp_id} not found"),
                }));
            };
            let (s1, s2, s3, c1) = CORP_GOV
                .lock()
                .unwrap()
                .get(&cid)
                .map(|g| g.logo)
                .unwrap_or((1, 2, 3, 10));

            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(cid)),
                ("corporationName", PyRep::String(name)),
                ("ticker", PyRep::String(ticker.clone())),
                ("tickerName", PyRep::String(ticker)),
                ("ceoID", PyRep::Integer(ceo_id)),
                ("memberCount", PyRep::Integer(m_count)),
                ("taxRate", PyRep::Real(tax)),
                ("allianceID", alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("stationID", PyRep::Integer(hq_station)),
                ("description", PyRep::String(desc)),
                ("url", PyRep::String(url)),
                ("shares", PyRep::Integer(1000)),
                ("shape1", PyRep::Integer(s1)),
                ("shape2", PyRep::Integer(s2)),
                ("shape3", PyRep::Integer(s3)),
                ("color1", PyRep::Integer(c1)),
                ("deleted", PyRep::Integer(0)),
            ])))
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
                (
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(3).and_then(|a| a.as_f64()),
                )
            } else {
                (
                    args.first().and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                    args.get(2).and_then(|a| a.as_f64()),
                )
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
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("description", PyRep::String(desc)),
                        ("url", PyRep::String(url)),
                        ("taxRate", PyRep::Real(tax)),
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
            let old_logo = {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                std::mem::replace(&mut state.logo, (s1, s2, s3, c1))
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorpLogoReady", "corpid", vec![PyRep::Integer(corp_id)]);
            let change = PyRep::Dict(vec![
                (PyRep::String(("shape1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.0), PyRep::Integer(s1)])),
                (PyRep::String(("shape2").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.1), PyRep::Integer(s2)])),
                (PyRep::String(("shape3").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.2), PyRep::Integer(s3)])),
                (PyRep::String(("color1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.3), PyRep::Integer(c1)])),
            ]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("shape1", PyRep::Integer(s1)),
                ("shape2", PyRep::Integer(s2)),
                ("shape3", PyRep::Integer(s3)),
                ("color1", PyRep::Integer(c1)),
            ])))
        }

        ("corpRegistry", "UpdateDivisionNames")
        | ("corpmgr", "UpdateDivisionNames")
        | ("corporationSvc", "UpdateDivisionNames") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(corp_id) = session.corporation_id else {
                return Some(Err(BridgeRpcError { code: "NoCorporation".into(), message: "Character has no active corporation".into() }));
            };
            let div_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Division");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update division names".into() });
                }
                if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                    Ok(std::mem::replace(&mut div.name, new_name.to_string()))
                } else {
                    Err(BridgeRpcError { code: "DivisionNotFound".into(), message: format!("Division {div_id} not found") })
                }
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

        ("corpRegistry", "GetMembers")
        | ("corpmgr", "GetMembers")
        | ("corporationSvc", "GetMembers")
        | ("corpRegistry", "GetMember")
        | ("corpRegistry", "GetMembersPaged")
        | ("corpRegistry", "GetMembersByIds")
        | ("corpRegistry", "GetMemberIDsByQuery")
        | ("corpRegistry", "GetMemberTrackingInfo")
        | ("corpRegistry", "GetMemberTrackingInfoSimple") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&id| id >= 98_000_000 || id == 1_000_009)
                .or(session.corporation_id)
                .unwrap_or(0);
            let now_unix = ctx.now_unix();
            let gov_map = CORP_GOV.lock().unwrap();
            let gov_opt = gov_map.get(&corp_id);

            let rows = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Vec::new();
                };
                corp.members
                    .iter()
                    .filter_map(|&mid| {
                        snap.characters.get(&mid).map(|ch| {
                            let mut role_mask = corp.member_roles.get(&mid).copied().unwrap_or(0);
                            if let Some(g) = gov_opt {
                                if let Some(&(pending_mask, eff_unix)) = g.pending_role_grants.get(&mid) {
                                    if now_unix >= eff_unix {
                                        role_mask |= pending_mask;
                                    }
                                }
                                if let Some(&tmask) = g.member_titles.get(&mid) {
                                    for (&tid, &(_, troles)) in &g.titles {
                                        if (tmask & (1u64 << (tid.rem_euclid(16)))) != 0 {
                                            role_mask |= troles;
                                        }
                                    }
                                }
                            }
                            let title = if mid == corp.ceo_id { "CEO" } else { "Member" };
                            let ship_type_id = snap
                                .items
                                .get(&ch.active_ship_id)
                                .map(|i| i.type_id as i64)
                                .unwrap_or(606);
                            vec![
                                PyRep::Integer(ch.character_id),
                                PyRep::String(ch.name.clone()),
                                PyRep::Integer(role_mask as i64),
                                PyRep::String(title.into()),
                                PyRep::Integer(corp_id),
                                PyRep::Integer(ch.solar_system_id as i64),
                                PyRep::Integer(ship_type_id),
                                PyRep::Integer(now_unix as i64),
                            ]
                        })
                    })
                    .collect()
            });

            Some(Ok(PyRep::rowset(
                vec![
                    "characterID",
                    "name",
                    "role",
                    "title",
                    "corporationID",
                    "locationID",
                    "shipTypeID",
                    "logonDateTime",
                ],
                rows,
            )))
        }

        ("corpRegistry", "UpdateMember") | ("corpRegistry", "UpdateMembers") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let db_corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .filter(|cid| snap.corporations.contains_key(cid))
            });
            if let Some(cid) = db_corp_id {
                session.corporation_id = Some(cid);
            }
            let corp_id = match session.corporation_id {
                Some(c) => c,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NoCorporation".into(),
                        message: "Character has no active corporation".into(),
                    }))
                }
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "UpdateMember requires target characterID".into(),
                }));
            };
            let new_roles = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            let title_mask_opt = args.get(2).and_then(|a| a.as_i64()).map(|v| v as u64);
            let _delay_hangar_grant = matches!(args.get(3), Some(PyRep::Bool(true)));
            let now_unix = ctx.now_unix();

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
                        message: "Only CEO or Director can update corporation roles".into(),
                    });
                }
                if !corp.members.contains(&target_char_id) {
                    return Err(BridgeRpcError {
                        code: "TargetNotInCorp".into(),
                        message: format!("Character {target_char_id} is not a member of corporation {corp_id}"),
                    });
                }
                let old_roles = corp.member_roles.get(&target_char_id).copied().unwrap_or(0);
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                state
                    .role_audit_log
                    .push((target_char_id, char_id, old_roles, new_roles, now_unix));
                if let Some(tm) = title_mask_opt {
                    state.member_titles.insert(target_char_id, tm);
                }
                const HANGAR_DIV_MASK: u64 = ((1u64 << 14) - 1) << 13;
                let granted_hangar = (new_roles & !old_roles) & HANGAR_DIV_MASK;
                if granted_hangar != 0 {
                    // 24h delay on hangar division role grant; revocations and non-hangar role grants are immediate
                    state
                        .pending_role_grants
                        .insert(target_char_id, (new_roles, now_unix + 86_400));
                    corp.member_roles
                        .insert(target_char_id, new_roles & !granted_hangar);
                } else {
                    state.pending_role_grants.remove(&target_char_id);
                    corp.member_roles.insert(target_char_id, new_roles);
                }
                Ok(old_roles)
            });

            match res {
                Ok(old_roles) => {
                    let change = PyRep::Dict(vec![(
                        PyRep::String(("roles").into()),
                        PyRep::Tuple(vec![PyRep::Integer(old_roles as i64), PyRep::Integer(new_roles as i64)]),
                    )]);
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorporationMemberChanged",
                        "corpid",
                        vec![PyRep::Integer(corp_id), PyRep::Integer(target_char_id), change],
                    );
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(target_char_id)),
                        ("roles", PyRep::Integer(new_roles as i64)),
                        ("delayed24h", PyRep::Bool(new_roles > old_roles)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "AuditMember") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let target_id = args.first().and_then(|a| a.as_i64());
            let rows: Vec<Vec<PyRep>> = CORP_GOV
                .lock()
                .unwrap()
                .get(&corp_id)
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
                .unwrap_or_default();
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
            if matches!(method, "GetMyApplications" | "GetMyOldApplications") {
                let rows = ctx.db.with_snapshot(|snap| {
                    let mut out = Vec::new();
                    for corp in snap.corporations.values() {
                        for app in corp.applications.values() {
                            if app.character_id == char_id {
                                out.push(vec![
                                    PyRep::Integer(app.application_id),
                                    PyRep::Integer(app.corporation_id),
                                    PyRep::Integer(app.character_id),
                                    PyRep::String(app.message.clone()),
                                    PyRep::String(app.status.clone()),
                                    PyRep::Integer(app.created_unix as i64),
                                ]);
                            }
                        }
                    }
                    out
                });
                return Some(Ok(PyRep::rowset(
                    vec![
                        "applicationID",
                        "corporationID",
                        "characterID",
                        "message",
                        "status",
                        "createdDate",
                    ],
                    rows,
                )));
            }
            let corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.corporation_id)
                .unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Viewing applications requires Personnel Manager or Director role"
                            .into(),
                    });
                }
                let rows: Vec<Vec<PyRep>> = corp
                    .applications
                    .values()
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
                Ok(rows) => Some(Ok(PyRep::rowset(
                    vec!["applicationID", "characterID", "message", "status", "createdDate"],
                    rows,
                ))),
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
                let welcome = CORP_GOV
                    .lock()
                    .unwrap()
                    .get(&corp_id)
                    .map(|g| g.welcome_mail.clone())
                    .unwrap_or((false, String::new(), String::new()));
                if welcome.0 && !welcome.1.is_empty() {
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.mails.push(MailRecord {
                        message_id: mid,
                        sender_id: ceo_id,
                        recipient_id: applicant_id,
                        subject: welcome.1,
                        body: welcome.2,
                    });
                    snap.mail_timestamps.insert(mid, now_unix);
                    snap.mail_assigned_labels.insert((applicant_id, mid), vec![1]);
                }

                Ok((corp_id, applicant_id, old_corp_id, "Accepted".to_string(), app_msg, app_ts))
            });

            match res {
                Ok((corp_id, applicant_id, old_corp_id, status_str, app_msg, app_ts)) => {
                    if status_str == "Accepted" {
                        record_corp_join(applicant_id, corp_id, now_unix);
                        let m_change = PyRep::Dict(vec![(
                            PyRep::String(("corporationID").into()),
                            PyRep::Tuple(vec![PyRep::Integer(old_corp_id), PyRep::Integer(corp_id)]),
                        )]);
                        ctx.notify(
                            NotificationTarget::Corporation(corp_id),
                            "OnCorporationMemberChanged",
                            "corpid",
                            vec![PyRep::Integer(corp_id), PyRep::Integer(applicant_id), m_change],
                        );
                    }
                    let app_kv = PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("characterID", PyRep::Integer(applicant_id)),
                        ("applicationText", PyRep::String(app_msg)),
                        ("status", PyRep::String(status_str.clone())),
                        ("applicationDateTime", PyRep::Integer(app_ts as i64)),
                    ]);
                    let app_args = vec![
                        PyRep::Integer(corp_id),
                        PyRep::Integer(applicant_id),
                        PyRep::Integer(app_id),
                        app_kv,
                    ];
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
                    if let Some(ch) = snap.characters.get_mut(&target_id) {
                        ch.corporation_id = corp_id;
                        ch.alliance_id = alliance_id;
                    }
                }
                Ok(app_id)
            });
            match res {
                Ok(app_id) => {
                    if accept_now {
                        record_corp_join(target_id, corp_id, now_unix);
                    }
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let target_id = match args.first() {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.first().and_then(|v| v.as_i64()).unwrap_or(char_id)
                }
                _ => char_id,
            };
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if target_id != char_id
                    && char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Kicking a member requires Personnel Manager or Director role"
                            .into(),
                    });
                }
                if target_id == corp.ceo_id && corp.members.len() > 1 {
                    return Err(BridgeRpcError {
                        code: "CeoCannotLeave".into(),
                        message: "CEO cannot leave while other members remain in the corporation"
                            .into(),
                    });
                }
                let has_active_war = snap.wars.values().any(|w| {
                    w.active
                        && (w.aggressor_corp_id == corp_id
                            || w.defender_corp_id == corp_id
                            || corp.alliance_id.is_some_and(|aid| {
                                w.aggressor_corp_id == aid || w.defender_corp_id == aid
                            }))
                });
                if corp.members.len() == 1 && corp.members.contains(&target_id) && has_active_war {
                    return Err(BridgeRpcError {
                        code: "ActiveWarsPreventClosure".into(),
                        message: "Corporation cannot close while active wars remain".into(),
                    });
                }
                let target_roles = corp.member_roles.get(&target_id).copied().unwrap_or(0);
                let in_space = snap
                    .characters
                    .get(&target_id)
                    .is_some_and(|ch| ch.station_id.is_none() && ch.structure_id.is_none());
                if target_id != char_id && (target_roles != 0 || in_space) {
                    // Queue deferred auto-kick
                    CORP_GOV
                        .lock()
                        .unwrap()
                        .entry(corp_id)
                        .or_default()
                        .pending_auto_kicks
                        .insert(target_id, now_unix);
                    return Ok((false, true, false));
                }
                let corp = snap.corporations.get_mut(&corp_id).unwrap();
                corp.members.remove(&target_id);
                corp.member_roles.remove(&target_id);
                let closed = corp.members.is_empty();
                if closed {
                    snap.corporations.remove(&corp_id);
                }
                if let Some(ch) = snap.characters.get_mut(&target_id) {
                    ch.corporation_id = 1_000_009;
                    ch.alliance_id = None;
                }
                Ok((true, false, closed))
            });
            match res {
                Ok((removed, queued, closed)) => {
                    if target_id == char_id && removed {
                        session.corporation_id = Some(1_000_009);
                        session.alliance_id = None;
                        session.corp_role = 0;
                    }
                    if removed {
                        record_corp_join(target_id, 1_000_009, now_unix);
                        let m_change = PyRep::Dict(vec![(
                            PyRep::String(("corporationID").into()),
                            PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(1_000_009)]),
                        )]);
                        let m_args = vec![
                            PyRep::Integer(corp_id),
                            PyRep::Integer(target_id),
                            m_change,
                        ];
                        ctx.notify(
                            NotificationTarget::Corporation(corp_id),
                            "OnCorporationMemberChanged",
                            "corpid",
                            m_args.clone(),
                        );
                        ctx.notify(
                            NotificationTarget::Character(target_id),
                            "OnCorporationMemberChanged",
                            "charid",
                            m_args,
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(target_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("removed", PyRep::Bool(removed)),
                        ("queuedAutoKick", PyRep::Bool(queued)),
                        ("corporationClosed", PyRep::Bool(closed)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "GetPendingAutoKicks") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let pending: Vec<(i64, u64)> = CORP_GOV
                .lock()
                .unwrap()
                .get(&corp_id)
                .map(|g| g.pending_auto_kicks.iter().map(|(&k, &v)| (k, v)).collect())
                .unwrap_or_default();
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
