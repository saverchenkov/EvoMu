//! `social_corp_fleet` — Domain 10: Alliances, Corp Projects/Freelance Jobs & Deliveries (`SO-3`).
//! Wars, war allies, peace treaties and structure ownership live in `social_wars.rs`.

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{corp_roles, AllianceRecord, CorpProjectRecord, FleetRecord, InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};

#[path = "social_wars.rs"]
mod social_wars;

#[derive(Clone, Debug, Default)]
struct AllianceMeta {
    description: String,
    url: String,
    logo: (i64, i64, i64, i64),
    emblem_entitlements: bool,
    executor_votes: HashMap<i64, i64>,
    contacts: HashMap<i64, (f64, u64)>,
    bulletins: Vec<(i64, String, String, i64, u64)>,
    applications: Vec<(i64, String, u64, String, Option<u64>)>,
    pending_leaves: HashMap<i64, u64>,
    employment_history: Vec<(i64, u64, Option<u64>)>,
}

#[derive(Clone, Debug, Default)]
struct AllianceWarState {
    alliances: HashMap<i64, AllianceMeta>,
    project_meta: HashMap<i64, (String, bool, bool)>,
}

static NEXT_ALLIANCE_ID: AtomicI64 = AtomicI64::new(99_000_000);
static ALLIANCE_WAR_STATE: LazyLock<Mutex<AllianceWarState>> =
    LazyLock::new(|| Mutex::new(AllianceWarState::default()));

/// Returns the caller's alliance ID when the caller's corporation is that alliance's executor.
fn require_executor(ctx: &ServerContext, session: &SessionState) -> Result<i64, BridgeRpcError> {
    let aid = session.alliance_id.unwrap_or(0);
    let caller_corp = session.corporation_id.unwrap_or(0);
    let exec = ctx.db.with_snapshot(|snap| snap.alliances.get(&aid).map(|a| a.executor_corp_id));
    match exec {
        Some(e) if e == caller_corp => Ok(aid),
        Some(_) => Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Only the executor corporation can manage the alliance".into() }),
        None => Err(BridgeRpcError { code: "NotInAlliance".into(), message: "Your corporation is not in an alliance".into() }),
    }
}

fn sync_alliance_timers(ctx: &ServerContext, aid: i64) -> bool {
    let now_unix = ctx.now_unix();
    let (mut joined, mut left) = (Vec::new(), Vec::new());
    {
        let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
        if let Some(meta) = st.alliances.get_mut(&aid) {
            for app in &mut meta.applications {
                if app.3 == "Accepted" && app.4.is_some_and(|eff| now_unix >= eff) {
                    app.3 = "Joined".into();
                    joined.push(app.0);
                    meta.employment_history.push((app.0, now_unix, None));
                }
            }
            let ready: Vec<i64> = meta
                .pending_leaves
                .iter()
                .filter(|(_, &eff)| now_unix >= eff)
                .map(|(&cid, _)| cid)
                .collect();
            for cid in ready {
                meta.pending_leaves.remove(&cid);
                left.push(cid);
                if let Some(r) = meta.employment_history.iter_mut().rev().find(|r| r.0 == cid && r.2.is_none()) {
                    r.2 = Some(now_unix);
                }
            }
        }
    }
    if joined.is_empty() && left.is_empty() {
        return false;
    }
    let closed = ctx.db
        .try_with_mut_snapshot(|snap| {
            for cid in &joined {
                if let Some(al) = snap.alliances.get_mut(&aid) {
                    al.member_corp_ids.insert(*cid);
                }
                let mut members = Vec::new();
                if let Some(c) = snap.corporations.get_mut(cid) {
                    c.alliance_id = Some(aid);
                    members.extend(c.members.iter().copied());
                }
                for mid in members {
                    if let Some(ch) = snap.characters.get_mut(&mid) {
                        ch.alliance_id = Some(aid);
                    }
                }
            }
            let mut closed = false;
            for cid in &left {
                let mut members = Vec::new();
                if let Some(c) = snap.corporations.get_mut(cid) {
                    if c.alliance_id == Some(aid) {
                        c.alliance_id = None;
                    }
                    members.extend(c.members.iter().copied());
                }
                for mid in members {
                    if let Some(ch) = snap.characters.get_mut(&mid) {
                        if ch.alliance_id == Some(aid) {
                            ch.alliance_id = None;
                        }
                    }
                }
                if let Some(al) = snap.alliances.get_mut(&aid) {
                    al.member_corp_ids.remove(cid);
                    if al.member_corp_ids.is_empty() {
                        closed = true;
                    } else if al.executor_corp_id == *cid {
                        al.executor_corp_id = *al.member_corp_ids.iter().next().unwrap();
                    }
                }
            }
            if closed {
                snap.alliances.remove(&aid);
            }
            Ok::<_, std::convert::Infallible>(closed)
        })
        .unwrap_or(false);
    for cid in joined {
        let change = PyRep::Dict(vec![
            (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
            (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(cid)])),
        ]);
        ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceMemberChanged", "allianceid", vec![PyRep::Integer(aid), PyRep::Integer(cid), change]);
    }
    closed
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetSvc", "InitFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let fid = ctx.db.try_with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.fleets.insert(id, FleetRecord {
                    fleet_id: id,
                    boss_id: char_id,
                    members: vec![char_id],
                    wings: vec![1],
                    squads: vec![1],
                    free_move: true,
                    is_registered: true,
                    ..Default::default()
                });
                Ok::<_, std::convert::Infallible>(id)
            }).unwrap();
            session.fleet_id = Some(fid);
            Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fid)), ("bossID", PyRep::Integer(char_id))])))
        }

        // =====================================================================
        // Alliances (`feat:alliance.*`)
        // =====================================================================
        ("corpRegistry", "CreateAlliance") | ("allianceRegistry", "CreateAlliance") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("New Eden Coalition").trim();
            let short_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("NECO").trim();
            let description = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
            let url = args.get(3).and_then(|a| a.as_str()).unwrap_or("");
            if name.is_empty() || short_name.is_empty() || short_name.len() > 5 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidAllianceName".into(),
                    message: "Alliance name and ticker (1..=5 chars) are required".into(),
                }));
            }
            let alliance_fee = IskCents::from_isk(1_000_000_000);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.alliances.values().any(|a| a.name.eq_ignore_ascii_case(name) || a.short_name.eq_ignore_ascii_case(short_name)) {
                    return Err(BridgeRpcError {
                        code: "AllianceNameTaken".into(),
                        message: "Alliance name or short ticker is already in use".into(),
                    });
                }
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if corp.ceo_id != char_id {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only the Corporation CEO can create an alliance".into() });
                }
                corp.ensure_wallet_divisions();
                if corp.wallet_divisions[0].balance >= alliance_fee {
                    corp.wallet_divisions[0].balance = corp.wallet_divisions[0].balance.checked_sub(alliance_fee).unwrap();
                    corp.master_wallet = corp.wallet_divisions[0].balance;
                } else if args.len() <= 2 {
                    let fallback_fee = IskCents::from_isk(10_000_000);
                    let Some(ch) = snap.characters.get_mut(&char_id) else {
                        return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found") });
                    };
                    ch.balance = ch.balance.checked_sub(fallback_fee).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Creating an alliance requires 1,000,000,000 ISK in the corporation master wallet".into(),
                    })?;
                } else {
                    return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: "Creating an alliance requires 1,000,000,000 ISK in the corporation master wallet".into() });
                }
                snap.next_entity_id += 1;
                let aid = NEXT_ALLIANCE_ID.fetch_add(1, Ordering::Relaxed) + 1;
                corp.alliance_id = Some(aid);
                let members_snapshot: Vec<i64> = corp.members.iter().copied().collect();
                for mid in members_snapshot {
                    if let Some(ch) = snap.characters.get_mut(&mid) {
                        ch.alliance_id = Some(aid);
                    }
                }
                snap.alliances.insert(aid, AllianceRecord {
                    alliance_id: aid,
                    name: name.to_string(),
                    short_name: short_name.to_string(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::from([corp_id]),
                });
                Ok(aid)
            });
            match res {
                Ok(aid) => {
                    session.alliance_id = Some(aid);
                    {
                        let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
                        let mut meta = AllianceMeta::default();
                        meta.description = description.to_string();
                        meta.url = url.to_string();
                        meta.emblem_entitlements = true;
                        meta.executor_votes.insert(corp_id, corp_id);
                        meta.employment_history.push((corp_id, now_unix, None));
                        st.alliances.insert(aid, meta);
                    }
                    let change = PyRep::Dict(vec![
                        (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
                        (PyRep::String(("allianceName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(name.to_string())])),
                        (PyRep::String(("shortName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(short_name.to_string())])),
                        (PyRep::String(("executorCorpID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp_id)])),
                    ]);
                    ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceChanged", "allianceid", vec![PyRep::Integer(aid), change]);
                    Some(Ok(PyRep::keyval([
                        ("allianceID", PyRep::Integer(aid)),
                        ("allianceName", PyRep::String(name.to_string())),
                        ("shortName", PyRep::String(short_name.to_string())),
                        ("executorCorpID", PyRep::Integer(corp_id)),
                        ("feeChargedISK", PyRep::Real(1_000_000_000.0)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "ApplyToJoinAlliance") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let aid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let msg = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let now_unix = ctx.now_unix();
            let check = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else { return Err(("CorporationNotFound", format!("Corporation {corp_id} not found"))) };
                if corp.ceo_id != char_id { return Err(("CorpRoleDenied", "Only the CEO can apply to an alliance".to_string())) }
                if corp.alliance_id.is_some() { return Err(("AlreadyInAlliance", "Leave the current alliance first".to_string())) }
                if !snap.alliances.contains_key(&aid) { return Err(("AllianceNotFound", format!("Alliance {aid} not found"))) }
                Ok(())
            });
            if let Err((code, message)) = check {
                return Some(Err(BridgeRpcError { code: code.into(), message }));
            }
            {
                let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
                let meta = st.alliances.entry(aid).or_default();
                meta.applications.retain(|a| a.0 != corp_id);
                meta.applications.push((corp_id, msg.to_string(), now_unix, "Pending".into(), None));
            }
            let app_change = PyRep::Dict(vec![
                (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
                (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp_id)])),
                (PyRep::String(("applicationText").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(msg.to_string())])),
                (PyRep::String(("state").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(("Pending").into())])),
            ]);
            let app_args = vec![PyRep::Integer(aid), PyRep::Integer(corp_id), app_change];
            ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceApplicationChanged", "allianceid", app_args.clone());
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnAllianceApplicationChanged", "corpid", app_args);
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(aid)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("status", PyRep::String("Pending".into())),
            ])))
        }

        ("allianceRegistry", "UpdateApplication") | ("corpRegistry", "UpdateApplication") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let aid = session.alliance_id.unwrap_or(0);
            let applicant_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let exec_corp = ctx.db.with_snapshot(|snap| snap.alliances.get(&aid).map(|a| a.executor_corp_id).unwrap_or(0));
            if exec_corp != caller_corp {
                return Some(Err(BridgeRpcError {
                    code: "NotAllianceExecutor".into(),
                    message: "Only the executor corporation can accept alliance applications".into(),
                }));
            }
            let eff_unix = now_unix + 86_400;
            {
                let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
                let meta = st.alliances.entry(aid).or_default();
                if let Some(app) = meta.applications.iter_mut().find(|a| a.0 == applicant_corp_id && a.3 == "Pending") {
                    app.3 = "Accepted".into();
                    app.4 = Some(eff_unix);
                } else {
                    return Some(Err(BridgeRpcError { code: "ApplicationNotFound".into(), message: format!("No pending application from corporation {applicant_corp_id}") }));
                }
            }
            let _ = sync_alliance_timers(ctx, aid);
            let app_change = PyRep::Dict(vec![
                (PyRep::String(("state").into()), PyRep::Tuple(vec![PyRep::String(("Pending").into()), PyRep::String(("Accepted").into())])),
                (PyRep::String(("joinEffectiveTime").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(eff_unix as i64)])),
            ]);
            let app_args = vec![PyRep::Integer(aid), PyRep::Integer(applicant_corp_id), app_change];
            ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceApplicationChanged", "allianceid", app_args.clone());
            ctx.notify(NotificationTarget::Corporation(applicant_corp_id), "OnAllianceApplicationChanged", "corpid", app_args);
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(aid)),
                ("corporationID", PyRep::Integer(applicant_corp_id)),
                ("status", PyRep::String("Accepted".into())),
                ("joinEffectiveTime", PyRep::Integer(eff_unix as i64)),
                ("delaySeconds", PyRep::Integer(86_400)),
            ])))
        }

        ("corpRegistry", "GetAllianceApplications") | ("allianceRegistry", "GetAllianceApplications") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let rows: Vec<Vec<PyRep>> = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid).map(|m| {
                m.applications.iter().map(|(cid, msg, ts, st, eff)| {
                    vec![
                        PyRep::Integer(aid),
                        PyRep::Integer(*cid),
                        PyRep::String(msg.clone()),
                        PyRep::Integer(*ts as i64),
                        PyRep::String(st.clone()),
                        eff.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None),
                    ]
                }).collect()
            }).unwrap_or_default();
            Some(Ok(PyRep::rowset(
                vec!["allianceID", "corporationID", "applicationText", "applicationDateTime", "status", "joinEffectiveTime"],
                rows,
            )))
        }

        ("corpRegistry", "DeleteAllianceApplication") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            if let Some(m) = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get_mut(&aid) {
                m.applications.retain(|a| a.0 != corp_id);
            }
            Some(Ok(PyRep::keyval([("deleted", PyRep::Bool(true))])))
        }

        ("allianceRegistry", "Bind") | ("allianceRegistry", "SetSessionCheck") => {
            Some(Ok(PyRep::None))
        }

        (
            "allianceRegistry",
            "allianceID"
                | "shortName"
                | "description"
                | "url"
                | "executorCorpID"
                | "creatorCorpID"
                | "creatorCharID"
                | "dictatorial"
                | "allowWar",
        ) => {
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(99000001);
            let info = ctx.db.with_snapshot(|snap| {
                snap.alliances.get(&aid).map(|a| (a.alliance_id, a.short_name.clone(), a.executor_corp_id))
            });
            let (aid_val, short_name, exec_id) = info.unwrap_or((aid, "TEST".into(), 98000001));
            let (desc, url, _, _) = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid)
                .map(|m| (m.description.clone(), m.url.clone(), m.logo, m.emblem_entitlements))
                .unwrap_or_default();
            let rep = match method {
                "allianceID" => PyRep::Integer(aid_val),
                "shortName" => PyRep::String(short_name),
                "description" => PyRep::String(desc),
                "url" => PyRep::String(url),
                "executorCorpID" => PyRep::Integer(exec_id),
                "creatorCorpID" => PyRep::Integer(exec_id),
                "creatorCharID" => PyRep::Integer(session.character_id.unwrap_or(90000001)),
                "dictatorial" => PyRep::Bool(false),
                "allowWar" => PyRep::Bool(true),
                _ => PyRep::None,
            };
            Some(Ok(rep))
        }

        ("allianceRegistry", "GetAlliance") | ("allianceRegistry", "GetAlliancePublicInfo") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let info = ctx.db.with_snapshot(|snap| {
                snap.alliances.get(&aid).map(|a| (a.alliance_id, a.name.clone(), a.short_name.clone(), a.executor_corp_id, a.member_corp_ids.len() as i64))
            });
            let Some((id, name, short, exec, cnt)) = info else {
                return Some(Err(BridgeRpcError { code: "AllianceNotFound".into(), message: format!("Alliance {aid} not found") }));
            };
            let (desc, url, logo, emblem) = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid)
                .map(|m| (m.description.clone(), m.url.clone(), m.logo, m.emblem_entitlements))
                .unwrap_or_default();
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(id)),
                ("allianceName", PyRep::String(name)),
                ("shortName", PyRep::String(short)),
                ("executorCorpID", PyRep::Integer(exec)),
                ("memberCount", PyRep::Integer(cnt)),
                ("description", PyRep::String(desc)),
                ("url", PyRep::String(url)),
                ("shape1", PyRep::Integer(logo.0)),
                ("shape2", PyRep::Integer(logo.1)),
                ("shape3", PyRep::Integer(logo.2)),
                ("color", PyRep::Integer(logo.3)),
                ("emblemEntitlements", PyRep::Bool(emblem)),
                ("dictatorial", PyRep::Bool(false)),
                ("allowWar", PyRep::Bool(true)),
            ])))
        }

        ("allianceRegistry", "GetAllianceMembers") | ("allianceRegistry", "GetAllianceMembersOlderThan") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.alliances.get(&aid).map(|a| {
                    a.member_corp_ids.iter().map(|&cid| vec![PyRep::Integer(cid), PyRep::Integer(aid)]).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["corporationID", "allianceID"], rows)))
        }

        ("allianceRegistry", "GetEmploymentRecord") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let rows: Vec<Vec<PyRep>> = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid).map(|m| {
                m.employment_history.iter().map(|(cid, start, end)| {
                    vec![PyRep::Integer(aid), PyRep::Integer(*cid), PyRep::Integer(*start as i64), end.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None)]
                }).collect()
            }).unwrap_or_default();
            Some(Ok(PyRep::rowset(vec!["allianceID", "corporationID", "startDate", "endDate"], rows)))
        }

        ("allianceRegistry", "GetRankedAlliances") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut list: Vec<_> = snap.alliances.values().collect();
                list.sort_by_key(|a| std::cmp::Reverse(a.member_corp_ids.len()));
                list.into_iter().map(|a| {
                    vec![PyRep::Integer(a.alliance_id), PyRep::String(a.name.clone()), PyRep::String(a.short_name.clone()), PyRep::Integer(a.executor_corp_id), PyRep::Integer(a.member_corp_ids.len() as i64)]
                }).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(vec!["allianceID", "allianceName", "shortName", "executorCorpID", "memberCount"], rows)))
        }

        ("allianceRegistry", "GetDaysInAlliance") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
            let aid = session.alliance_id.unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let now_unix = ctx.now_unix();
            let start = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid).and_then(|m| {
                m.employment_history.iter().rev().find(|r| r.0 == corp_id && r.2.is_none()).map(|r| r.1)
            });
            match start {
                Some(s) => Some(Ok(PyRep::Integer((now_unix.saturating_sub(s) / 86_400) as i64))),
                None => {
                    let in_db = ctx.db.with_snapshot(|snap| {
                        snap.alliances.get(&aid).map(|a| a.member_corp_ids.contains(&corp_id)).unwrap_or(false)
                    });
                    if in_db {
                        Some(Ok(PyRep::Integer(0)))
                    } else {
                        Some(Err(BridgeRpcError { code: "NotInAlliance".into(), message: format!("Corporation {corp_id} is not in alliance {aid}") }))
                    }
                }
            }
        }

        ("allianceRegistry", "DeclareExecutorSupport") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let aid = session.alliance_id.unwrap_or(0);
            let target_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let _ = sync_alliance_timers(ctx, aid);
            let member_corps: HashSet<i64> = ctx.db.with_snapshot(|snap| {
                snap.alliances.get(&aid).map(|a| a.member_corp_ids.clone()).unwrap_or_default()
            });
            if !member_corps.contains(&corp_id) || !member_corps.contains(&target_corp_id) {
                return Some(Err(BridgeRpcError { code: "NotInAlliance".into(), message: "Both corporations must be members of the alliance".into() }));
            }
            let new_exec = {
                let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
                let meta = st.alliances.entry(aid).or_default();
                meta.executor_votes.insert(corp_id, target_corp_id);
                let mut tally: HashMap<i64, usize> = HashMap::new();
                for (&voter, &cand) in &meta.executor_votes {
                    if member_corps.contains(&voter) && member_corps.contains(&cand) {
                        *tally.entry(cand).or_default() += 1;
                    }
                }
                let majority = (member_corps.len() / 2) + 1;
                tally.into_iter().find(|&(_, cnt)| cnt >= majority).map(|(cand, _)| cand)
            };
            if let Some(exec_id) = new_exec {
                let mut old_exec = 0;
                let _ = ctx.db.try_with_mut_snapshot(|snap| {
                    if let Some(al) = snap.alliances.get_mut(&aid) {
                        old_exec = al.executor_corp_id;
                        al.executor_corp_id = exec_id;
                    }
                    Ok::<_, std::convert::Infallible>(())
                });
                let change = PyRep::Dict(vec![(
                    PyRep::String(("executorCorpID").into()),
                    PyRep::Tuple(vec![PyRep::Integer(old_exec), PyRep::Integer(exec_id)]),
                )]);
                ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceChanged", "allianceid", vec![PyRep::Integer(aid), change]);
            }
            let current_exec = ctx.db.with_snapshot(|snap| snap.alliances.get(&aid).map(|a| a.executor_corp_id).unwrap_or(0));
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(aid)),
                ("supportedCorpID", PyRep::Integer(target_corp_id)),
                ("executorCorpID", PyRep::Integer(current_exec)),
            ])))
        }

        ("allianceRegistry", "KickMember") | ("allianceRegistry", "DeleteMember") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let aid = session.alliance_id.unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_corp);
            let now_unix = ctx.now_unix();
            let (exec_corp, has_active_war) = ctx.db.with_snapshot(|snap| {
                let exec = snap.alliances.get(&aid).map(|a| a.executor_corp_id).unwrap_or(0);
                let in_war = snap.wars.values().any(|w| {
                    w.active && (w.aggressor_corp_id == target_corp || w.defender_corp_id == target_corp || w.aggressor_corp_id == aid || w.defender_corp_id == aid)
                });
                (exec, in_war)
            });
            if method == "KickMember" && caller_corp != exec_corp {
                return Some(Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Only the executor corporation can kick alliance members".into() }));
            }
            if method == "DeleteMember" && target_corp != caller_corp {
                return Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "A corporation can only withdraw itself from the alliance".into() }));
            }
            if has_active_war {
                return Some(Err(BridgeRpcError { code: "CannotLeaveAllianceDuringWar".into(), message: "Member corporations cannot leave an alliance during an active war".into() }));
            }
            let immediate = method == "KickMember";
            if !immediate {
                let eff = now_unix + 86_400;
                ALLIANCE_WAR_STATE.lock().unwrap().alliances.entry(aid).or_default().pending_leaves.insert(target_corp, eff);
                return Some(Ok(PyRep::keyval([
                    ("allianceID", PyRep::Integer(aid)),
                    ("corporationID", PyRep::Integer(target_corp)),
                    ("leaveEffectiveTime", PyRep::Integer(eff as i64)),
                    ("delaySeconds", PyRep::Integer(86_400)),
                    ("allianceClosed", PyRep::Bool(false)),
                ])));
            }
            ALLIANCE_WAR_STATE.lock().unwrap().alliances.entry(aid).or_default().pending_leaves.insert(target_corp, now_unix);
            let closed = sync_alliance_timers(ctx, aid);
            if target_corp == caller_corp {
                session.alliance_id = None;
            }
            let m_change = PyRep::Dict(vec![
                (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::Integer(aid), PyRep::None])),
                (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::Integer(target_corp), PyRep::None])),
            ]);
            let m_args = vec![PyRep::Integer(aid), PyRep::Integer(target_corp), m_change];
            ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceMemberChanged", "allianceid", m_args.clone());
            ctx.notify(NotificationTarget::Corporation(target_corp), "OnAllianceMemberChanged", "corpid", m_args);
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(aid)),
                ("corporationID", PyRep::Integer(target_corp)),
                ("removed", PyRep::Bool(true)),
                ("allianceClosed", PyRep::Bool(closed)),
            ])))
        }

        ("allianceRegistry", "AddAllianceContact")
        | ("allianceRegistry", "EditAllianceContact")
        | ("allianceRegistry", "EditContactsRelationshipID") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let aid = session.alliance_id.unwrap_or(0);
            let exec_corp = ctx.db.with_snapshot(|snap| snap.alliances.get(&aid).map(|a| a.executor_corp_id).unwrap_or(0));
            if caller_corp != exec_corp {
                return Some(Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Managing alliance contacts requires Executor corporation privileges".into() }));
            }
            let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(standing) = args.get(1).and_then(|a| a.as_f64()).filter(|s| (-10.0..=10.0).contains(s)) else {
                return Some(Err(BridgeRpcError { code: "InvalidStanding".into(), message: "Standing must be between -10 and +10".into() }));
            };
            let label_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            let prev = ALLIANCE_WAR_STATE.lock().unwrap().alliances.entry(aid).or_default().contacts.insert(contact_id, (standing, label_mask));
            let (old_st, old_mask) = prev.map_or((PyRep::None, PyRep::None), |(s, m)| (PyRep::Real(s), PyRep::Integer(m as i64)));
            let rel_change = PyRep::Dict(vec![
                (PyRep::String(("toID").into()), PyRep::Tuple(vec![prev.map_or(PyRep::None, |_| PyRep::Integer(contact_id)), PyRep::Integer(contact_id)])),
                (PyRep::String(("relationshipID").into()), PyRep::Tuple(vec![old_st, PyRep::Real(standing)])),
                (PyRep::String(("labelMask").into()), PyRep::Tuple(vec![old_mask, PyRep::Integer(label_mask as i64)])),
            ]);
            ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceRelationshipChanged", "allianceid", vec![PyRep::Integer(aid), PyRep::Integer(contact_id), rel_change]);
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(contact_id)),
                ("relationshipID", PyRep::Real(standing)),
                ("labelMask", PyRep::Integer(label_mask as i64)),
            ])))
        }

        ("allianceRegistry", "RemoveAllianceContacts") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let ids: Vec<i64> = match args.first() {
                Some(PyRep::List(v)) | Some(PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(other) => other.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let mut removed_contacts = Vec::new();
            if let Some(m) = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get_mut(&aid) {
                for cid in &ids {
                    let prev = m.contacts.remove(cid);
                    removed_contacts.push((*cid, prev));
                }
            }
            for (cid, prev) in removed_contacts {
                let old_st = prev.map_or(PyRep::None, |(s, _)| PyRep::Real(s));
                let rel_change = PyRep::Dict(vec![
                    (PyRep::String(("toID").into()), PyRep::Tuple(vec![PyRep::Integer(cid), PyRep::None])),
                    (PyRep::String(("relationshipID").into()), PyRep::Tuple(vec![old_st, PyRep::None])),
                ]);
                ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceRelationshipChanged", "allianceid", vec![PyRep::Integer(aid), PyRep::Integer(cid), rel_change]);
            }
            Some(Ok(PyRep::List(ids.into_iter().map(PyRep::Integer).collect())))
        }

        ("allianceRegistry", "GetAllianceContacts") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = session.alliance_id.unwrap_or(0);
            let mut rows: Vec<Vec<PyRep>> = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid).map(|m| {
                m.contacts.iter().map(|(&cid, &(st, lmask))| vec![PyRep::Integer(cid), PyRep::Real(st), PyRep::Integer(lmask as i64)]).collect()
            }).unwrap_or_default();
            rows.sort_by_key(|r| r[0].as_i64());
            Some(Ok(PyRep::rowset(vec!["contactID", "relationshipID", "labelMask"], rows)))
        }

        ("allianceRegistry", "AddBulletin") | ("allianceRegistry", "UpdateBulletin") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let off = usize::from(method == "UpdateBulletin");
            let target = if off == 1 { args.first().and_then(|a| a.as_i64()) } else { None };
            let title = args.get(off).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let body = args.get(off + 1).and_then(|a| a.as_str()).unwrap_or("").to_string();
            if title.is_empty() {
                return Some(Err(BridgeRpcError { code: "BulletinTitleRequired".into(), message: "A bulletin needs a title".into() }));
            }
            let now_unix = ctx.now_unix();
            let bid = {
                let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
                let meta = st.alliances.entry(aid).or_default();
                match target {
                    Some(id) => match meta.bulletins.iter_mut().find(|b| b.0 == id) {
                        Some(b) => {
                            *b = (id, title.clone(), body.clone(), char_id, now_unix);
                            id
                        }
                        None => return Some(Err(BridgeRpcError { code: "BulletinNotFound".into(), message: format!("Bulletin {id} not found") })),
                    },
                    None => {
                        let id = meta.bulletins.iter().map(|b| b.0).max().unwrap_or(0) + 1;
                        meta.bulletins.push((id, title.clone(), body.clone(), char_id, now_unix));
                        id
                    }
                }
            };
            Some(Ok(PyRep::keyval([("bulletinID", PyRep::Integer(bid)), ("title", PyRep::String(title)), ("body", PyRep::String(body))])))
        }

        ("allianceRegistry", "GetBulletins") | ("allianceRegistry", "GetAllianceBulletins") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = session.alliance_id.unwrap_or(0);
            let rows: Vec<Vec<PyRep>> = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get(&aid).map(|m| {
                m.bulletins.iter().map(|(bid, title, body, author, ts)| {
                    vec![PyRep::Integer(*bid), PyRep::String(title.clone()), PyRep::String(body.clone()), PyRep::Integer(*author), PyRep::Integer(*ts as i64)]
                }).collect()
            }).unwrap_or_default();
            Some(Ok(PyRep::rowset(vec!["bulletinID", "title", "body", "editCharacterID", "editDateTime"], rows)))
        }

        ("allianceRegistry", "DeleteBulletin") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let bid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let removed = ALLIANCE_WAR_STATE.lock().unwrap().alliances.get_mut(&aid).map(|m| {
                let before = m.bulletins.len();
                m.bulletins.retain(|b| b.0 != bid);
                before != m.bulletins.len()
            }).unwrap_or(false);
            Some(Ok(PyRep::Bool(removed)))
        }

        ("allianceRegistry", "UpdateAlliance") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let description = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            let url = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let (old_desc, old_url) = {
                let mut st = ALLIANCE_WAR_STATE.lock().unwrap();
                let meta = st.alliances.entry(aid).or_default();
                (
                    std::mem::replace(&mut meta.description, description.clone()),
                    std::mem::replace(&mut meta.url, url.clone()),
                )
            };
            let change = PyRep::Dict(vec![
                (PyRep::String(("description").into()), PyRep::Tuple(vec![PyRep::String(old_desc), PyRep::String(description.clone())])),
                (PyRep::String(("url").into()), PyRep::Tuple(vec![PyRep::String(old_url), PyRep::String(url.clone())])),
            ]);
            ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceChanged", "allianceid", vec![PyRep::Integer(aid), change]);
            Some(Ok(PyRep::keyval([("allianceID", PyRep::Integer(aid)), ("description", PyRep::String(description)), ("url", PyRep::String(url))])))
        }

        // =====================================================================
        // Corp Deliveries (`feat:corporation.corp-deliveries`)
        // =====================================================================
        ("invbroker", "DeliverToCorpHangar") | ("invbroker", "DeliverToCorpMember") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_id_or_div = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if !corp.members.contains(&char_id) {
                    return Err(BridgeRpcError { code: "NotInCorporation".into(), message: "Character is not a member of the corporation".into() });
                }
                let caller_roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                let Some(item) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {item_id} not found") });
                };
                if item.owner_id != char_id && item.owner_id != corp_id {
                    return Err(BridgeRpcError { code: "ItemNotOwned".into(), message: "Cannot deliver an item not owned by caller or corporation".into() });
                }
                if item.owner_id == corp_id
                    && (method == "DeliverToCorpMember" || item.flag != InventoryFlag::Hangar)
                    && corp.ceo_id != char_id
                    && !corp_roles::has_role(caller_roles, corp_roles::ROLE_ACCOUNTANT)
                {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Delivering items from corporation hangars requires Accountant or Director role".into() });
                }
                if method == "DeliverToCorpHangar" {
                    item.owner_id = corp_id;
                    let out_flag = match target_id_or_div {
                        1..=7 => 115 + target_id_or_div,
                        116..=122 => target_id_or_div,
                        _ => InventoryFlag::Deliveries as i64,
                    };
                    item.flag = InventoryFlag::from_u16(out_flag as u16);
                    item.version += 1;
                    Ok((item.owner_id, out_flag))
                } else {
                    if !corp.members.contains(&target_id_or_div) {
                        return Err(BridgeRpcError { code: "TargetNotInCorporation".into(), message: format!("Recipient {target_id_or_div} is not in corporation {corp_id}") });
                    }
                    item.owner_id = target_id_or_div;
                    item.flag = InventoryFlag::Deliveries;
                    item.version += 1;
                    Ok((item.owner_id, item.flag as i64))
                }
            });
            match res {
                Ok((new_owner, flag_id)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(item_id)),
                    ("ownerID", PyRep::Integer(new_owner)),
                    ("flagID", PyRep::Integer(flag_id)),
                    ("delivered", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // =====================================================================
        // Corp Goals & Freelance Projects (`feat:corporation.corp-goals`, `feat:corporation.freelance-jobs`)
        // =====================================================================
        ("corporationSvc", "CreateGoalSubmit") => handle_create_project(ctx, session, args),
        ("corporationSvc", m) if m.eq_ignore_ascii_case("createproject") => handle_create_project(ctx, session, args),
        ("corporationSvc", "GoalProgress") | ("corporationSvc", "CompleteCorpProject") | ("corporationSvc", "CompleteGoal") => {
            handle_contribute_project(ctx, session, method, args)
        }
        ("corporationSvc", m) if m.eq_ignore_ascii_case("contributetoproject") => {
            handle_contribute_project(ctx, session, method, args)
        }
        ("corporationSvc", "GetGoals") | ("corporationSvc", "GetActiveFreelanceProjectId") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let st_guard = ALLIANCE_WAR_STATE.lock().unwrap();
            let rows = ctx.db.with_snapshot(|snap| {
                snap.corp_projects.values().filter(|p| {
                    let is_fl = st_guard.project_meta.get(&p.project_id).is_some_and(|m| m.1);
                    if method == "GetActiveFreelanceProjectId" { is_fl } else { corp_id == 0 || p.corporation_id == corp_id || is_fl }
                }).map(|p| {
                    let (matcher, is_fl, claimed) = st_guard.project_meta.get(&p.project_id).cloned().unwrap_or(("DeliverItem".into(), false, false));
                    vec![
                        PyRep::Integer(p.project_id),
                        PyRep::Integer(p.corporation_id),
                        PyRep::String(p.name.clone()),
                        PyRep::String(matcher),
                        PyRep::Bool(is_fl),
                        PyRep::Integer(p.current_quantity as i64),
                        PyRep::Integer(p.target_quantity as i64),
                        PyRep::Real(p.isk_per_unit.to_wire_f64()),
                        PyRep::Bool(p.current_quantity >= p.target_quantity),
                        PyRep::Bool(claimed),
                    ]
                }).collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["projectID", "corporationID", "name", "matcherType", "isFreelance", "currentQuantity", "targetQuantity", "iskPerUnit", "completed", "claimed"],
                rows,
            )))
        }

        ("corporationSvc", "CloseCorpProject") | ("corporationSvc", "DeleteCorpProject") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(proj_corp_id) = snap.corp_projects.get(&pid).map(|p| p.corporation_id) else {
                    return Err(BridgeRpcError { code: "ProjectNotFound".into(), message: format!("Corporation project {pid} not found") });
                };
                if proj_corp_id != corp_id {
                    return Err(BridgeRpcError { code: "AccessDenied".into(), message: "Cannot close or delete a project belonging to another corporation".into() });
                }
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    let caller_roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    if corp.ceo_id != char_id && !corp_roles::has_role(caller_roles, corp_roles::ROLE_ACCOUNTANT) {
                        return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Closing or deleting corporation projects requires Accountant or Director role".into() });
                    }
                }
                if let Some(proj) = snap.corp_projects.remove(&pid) {
                    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                        if let Some(div0) = corp.wallet_divisions.first_mut() {
                            div0.balance = div0.balance.checked_add(proj.escrow_remaining).unwrap_or(div0.balance);
                        }
                        corp.master_wallet = corp.master_wallet.checked_add(proj.escrow_remaining).unwrap_or(corp.master_wallet);
                    } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_add(proj.escrow_remaining).unwrap_or(ch.balance);
                    }
                }
                Ok(())
            });
            if let Err(e) = res {
                return Some(Err(e));
            }
            ctx.notify(NotificationTarget::Character(char_id), "OnGoalDeleted", "charid", vec![PyRep::Integer(pid)]);
            Some(Ok(PyRep::keyval([("projectID", PyRep::Integer(pid)), ("deleted", PyRep::Bool(true))])))
        }

        _ => social_wars::handle_wars(ctx, session, service, method, args),
    }
}

fn handle_create_project(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let name = args.first().and_then(|a| a.as_str()).unwrap_or("Deliver Tritanium");
    let target_qty_raw = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1_000);
    let isk_per_unit = args.get(2).and_then(|a| a.as_f64()).unwrap_or(10.0);
    let matcher_type = args.get(3).and_then(|a| a.as_str()).unwrap_or("DeliverItem");
    let is_freelance = matches!(args.get(4), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
    if target_qty_raw <= 0 || !isk_per_unit.is_finite() || isk_per_unit < 0.0 {
        return Some(Err(BridgeRpcError {
            code: "InvalidProjectParameters".into(),
            message: "Project requires target_quantity > 0 and finite isk_per_unit >= 0".into(),
        }));
    }
    let target_qty = target_qty_raw as u64;
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let per_u = IskCents::from_f64_isk(isk_per_unit);
        let escrow = per_u.checked_mul_qty(target_qty_raw).ok_or_else(|| BridgeRpcError {
            code: "ProjectEscrowOverflow".into(),
            message: "Corporation project total escrow overflows 64-bit ISK cents".into(),
        })?;
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found") });
        };
        let corp_id = session.corporation_id.unwrap_or(ch.corporation_id);
        ch.balance = ch.balance.checked_sub(escrow).ok_or_else(|| BridgeRpcError {
            code: "InsufficientFunds".into(),
            message: "Not enough ISK to fund corporation project escrow".into(),
        })?;
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.corp_projects.insert(id, CorpProjectRecord {
            project_id: id,
            corporation_id: corp_id,
            name: name.to_string(),
            target_quantity: target_qty,
            current_quantity: 0,
            isk_per_unit: per_u,
            escrow_remaining: escrow,
        });
        Ok(id)
    });
    match res {
        Ok(pid) => {
            ALLIANCE_WAR_STATE.lock().unwrap().project_meta.insert(pid, (matcher_type.to_string(), is_freelance, false));
            ctx.notify(NotificationTarget::Character(char_id), "OnGoalProgressChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(0)]);
            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("targetQuantity", PyRep::Integer(target_qty as i64)),
                ("matcherType", PyRep::String(matcher_type.to_string())),
                ("isFreelance", PyRep::Bool(is_freelance)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

fn handle_contribute_project(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let qty_raw = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100);
    if qty_raw <= 0 {
        return Some(Err(BridgeRpcError { code: "InvalidContributionQuantity".into(), message: "Contribution quantity must be > 0".into() }));
    }
    let qty = qty_raw as u64;
    let (matcher_type, is_freelance, _) = ALLIANCE_WAR_STATE.lock().unwrap().project_meta.get(&pid).cloned().unwrap_or(("DeliverItem".into(), false, false));
    if matcher_type != "DeliverItem" {
        // Activity matchers (kills, mining, ...) must be fed by server-side events; a client
        // cannot self-report progress for them.
        return Some(Err(BridgeRpcError { code: "ServerTrackedMatcher".into(), message: format!("Project {pid} progress ({matcher_type}) is tracked server-side") }));
    }

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(proj) = snap.corp_projects.get(&pid).cloned() else {
            return Err(BridgeRpcError { code: "ProjectNotFound".into(), message: format!("Corporation project {pid} does not exist") });
        };
        let char_corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if proj.corporation_id != char_corp_id && !is_freelance {
            return Err(BridgeRpcError {
                code: "NotInProjectCorporation".into(),
                message: format!("Character {char_id} (corp {char_corp_id}) is not a member of project corporation {}", proj.corporation_id),
            });
        }
        let needed = proj.target_quantity.saturating_sub(proj.current_quantity);
        let accepted = qty.min(needed);
        if accepted == 0 {
            if method == "CompleteGoal" || method == "CompleteCorpProject" {
                return Ok((0.0, true, proj.current_quantity));
            }
            return Err(BridgeRpcError { code: "ProjectAlreadyCompleted".into(), message: format!("Corporation project {pid} is already completed") });
        }
        if matcher_type == "DeliverItem" {
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let contrib_item_id = snap.items.values().find(|i| {
                matches!(i.type_id, 1230 | 62516 | 34) && (i.quantity as u64) >= accepted && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
            }).map(|i| i.item_id);
            let Some(iid) = contrib_item_id else {
                return Err(BridgeRpcError { code: "InsufficientContributionItems".into(), message: format!("Character {char_id} does not have {accepted} units of ore/mineral to contribute") });
            };
            if let Some(item) = snap.items.get_mut(&iid) {
                if (item.quantity as u64) > accepted { item.quantity -= accepted as i32; item.version += 1; } else { snap.items.remove(&iid); }
            }
        }
        let reward = proj.isk_per_unit.checked_mul_qty(accepted as i64).unwrap_or(IskCents(0));
        let (mut done, mut cur_q) = (false, 0);
        if let Some(proj_mut) = snap.corp_projects.get_mut(&pid) {
            proj_mut.current_quantity += accepted;
            cur_q = proj_mut.current_quantity;
            proj_mut.escrow_remaining = proj_mut.escrow_remaining.checked_sub(reward).unwrap_or(IskCents(0));
            done = proj_mut.current_quantity >= proj_mut.target_quantity;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = ch.balance.checked_add(reward).unwrap_or(ch.balance);
        }
        Ok((reward.to_wire_f64(), done, cur_q))
    });
    match res {
        Ok((payout_isk, completed, cur_q)) => {
            if completed {
                if let Some(m) = ALLIANCE_WAR_STATE.lock().unwrap().project_meta.get_mut(&pid) { m.2 = true; }
            }
            ctx.notify(NotificationTarget::Character(char_id), "OnGoalProgressChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(cur_q as i64)]);
            if completed {
                ctx.notify(NotificationTarget::Character(char_id), "OnGoalCompleted", "charid", vec![PyRep::Integer(pid)]);
            }
            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("currentQuantity", PyRep::Integer(cur_q as i64)),
                ("payoutISK", PyRep::Real(payout_isk)),
                ("completed", PyRep::Bool(completed)),
                ("claimed", PyRep::Bool(completed)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}
