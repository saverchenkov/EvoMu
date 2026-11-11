//! `social_corp_fleet` — Domain 10: Alliances, Corp Projects/Freelance Jobs & Deliveries (`SO-3`).
//! Wars, war allies, peace treaties and structure ownership live in `social_wars.rs`.

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, AllianceRecord, FleetMemberRecord, FleetRecord, FleetRole,
    FleetSquadRecord, FleetWingRecord, InventoryFlag, IskCents,
    PersistedAllianceMeta as AllianceMeta, PersistedAllianceWarState as AllianceWarState,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
#[path = "social_corp_projects.rs"]
pub mod social_corp_projects;
#[path = "social_wars.rs"]
pub mod social_wars;

fn with_alliance_war_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut AllianceWarState) -> R,
) -> R {
    ctx.db
        .with_mut_snapshot(|snap| f(&mut snap.alliance_war_state))
}

fn read_alliance_war_state<R>(ctx: &ServerContext, f: impl FnOnce(&AllianceWarState) -> R) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.alliance_war_state))
}

fn label_ids_from_mask(mask: u64) -> Vec<i64> {
    (0..63)
        .map(|b| 1i64 << b)
        .filter(|&bit| (mask & (bit as u64)) != 0)
        .collect()
}

fn label_mask_from_ids(ids: &[i64]) -> u64 {
    ids.iter().fold(0u64, |acc, &id| acc | (id as u64))
}

fn require_corp_director(ctx: &ServerContext, session: &SessionState) -> Result<i64, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let corp_id = session.corporation_id.unwrap_or(0);
    let now_unix = ctx.now_unix();
    let ok = ctx.db.with_snapshot(|snap| {
        let (is_ceo, roles) =
            snap.effective_corp_roles_at(corp_id, char_id, None, session.corp_role, now_unix);
        is_ceo || corp_roles::has_role(roles, corp_roles::ROLE_DIRECTOR)
    });
    if !ok {
        return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Director or CEO role is required".into() });
    }
    Ok(corp_id)
}

/// Returns the caller's alliance ID when the caller's corporation is that alliance's executor and caller is CEO/Director.
fn require_executor(ctx: &ServerContext, session: &SessionState) -> Result<i64, BridgeRpcError> {
    let aid = session.alliance_id.unwrap_or(0);
    let caller_corp = session.corporation_id.unwrap_or(0);
    let exec = ctx.db.with_snapshot(|snap| snap.alliances.get(&aid).map(|a| a.executor_corp_id));
    match exec {
        Some(e) if e == caller_corp => {
            let _ = require_corp_director(ctx, session)?;
            Ok(aid)
        }
        Some(_) => Err(BridgeRpcError { code: "NotAllianceExecutor".into(), message: "Only the executor corporation can manage the alliance".into() }),
        None => Err(BridgeRpcError { code: "NotInAlliance".into(), message: "Your corporation is not in an alliance".into() }),
    }
}

pub(crate) fn sync_all_alliance_timers(ctx: &ServerContext) {
    let now_unix = ctx.now_unix();
    let aids: Vec<i64> = read_alliance_war_state(ctx, |st| {
        st.alliances
            .iter()
            .filter_map(|(&aid, meta)| {
                let has_due_app = meta
                    .applications
                    .iter()
                    .any(|app| app.3 == "Accepted" && app.4.is_some_and(|eff| now_unix >= eff));
                let has_due_leave = meta.pending_leaves.values().any(|&eff| now_unix >= eff);
                (has_due_app || has_due_leave).then_some(aid)
            })
            .collect()
    });
    for aid in aids {
        let _ = sync_alliance_timers(ctx, aid);
    }
}

fn sync_alliance_timers(ctx: &ServerContext, aid: i64) -> bool {
    let now_unix = ctx.now_unix();
    let has_due = read_alliance_war_state(ctx, |st| {
        st.alliances.get(&aid).is_some_and(|meta| {
            meta.applications
                .iter()
                .any(|app| app.3 == "Accepted" && app.4.is_some_and(|eff| now_unix >= eff))
                || meta.pending_leaves.values().any(|&eff| now_unix >= eff)
        })
    });
    if !has_due {
        return false;
    }
    let (mut joined, mut left) = (Vec::new(), Vec::new());
    with_alliance_war_state(ctx, |st| {
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
    });
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
    for cid in &joined {
        let change = PyRep::Dict(vec![
            (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
            (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(*cid)])),
        ]);
        ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceMemberChanged", "allianceid", vec![PyRep::Integer(aid), PyRep::Integer(*cid), change]);
    }
    for cid in &left {
        let m_change = PyRep::Dict(vec![
            (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::Integer(aid), PyRep::None])),
            (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::Integer(*cid), PyRep::None])),
        ]);
        let m_args = vec![PyRep::Integer(aid), PyRep::Integer(*cid), m_change];
        ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceMemberChanged", "allianceid", m_args.clone());
        ctx.notify(NotificationTarget::Corporation(*cid), "OnAllianceMemberChanged", "corpid", m_args);
    }
    let now = ctx.now_unix();
    let members_to_refresh: Vec<u32> = ctx.db.with_snapshot(|snap| {
        joined.iter().chain(left.iter())
            .filter_map(|cid| snap.corporations.get(cid))
            .flat_map(|c| c.members.iter().map(|&m| m as u32))
            .collect()
    });
    for mid in members_to_refresh {
        let _ = crate::services::quasar_social_chat::on_character_local_chat_membership_refreshed(
            &ctx.quasar_gateway, &ctx.db, mid, now,
        );
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
    bound_obj_id: Option<i64>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetSvc", "InitFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if crate::services::fleet_social::fleet_social_notify::character_active_fleet_id(ctx, char_id).is_some() {
                return Some(Err(BridgeRpcError {
                    code: "AlreadyInFleet".into(),
                    message: "Character is already in a fleet".into(),
                }));
            }
            let fid = ctx.db.try_with_mut_snapshot(|snap| {
                for ch in snap.characters.values_mut() {
                    if let Some(stale_fid) = ch.fleet_id {
                        if !snap.fleets.contains_key(&stale_fid) {
                            ch.fleet_id = None;
                        }
                    }
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let mut wing_details = HashMap::new();
                wing_details.insert(1, FleetWingRecord { wing_id: 1, name: "Wing 1".into(), commander_id: None, squad_ids: vec![1] });
                let mut squad_details = HashMap::new();
                squad_details.insert(1, FleetSquadRecord { squad_id: 1, wing_id: 1, name: "Squad 1".into(), commander_id: Some(char_id) });
                let mut member_details = HashMap::new();
                member_details.insert(char_id, FleetMemberRecord {
                    character_id: char_id,
                    role: FleetRole::FleetCommander,
                    wing_id: Some(1),
                    squad_id: Some(1),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 2,
                    role_name: "Fleet Commander".into(),
                    join_time_unix: ctx.now_unix(),
                });
                snap.fleets.insert(id, FleetRecord {
                    fleet_id: id,
                    boss_id: char_id,
                    members: vec![char_id],
                    wings: vec![1],
                    squads: vec![1],
                    free_move: true,
                    is_registered: true,
                    wing_details,
                    squad_details,
                    member_details,
                    ..Default::default()
                });
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.fleet_id = Some(id);
                }
                Ok::<_, std::convert::Infallible>(id)
            }).unwrap();
            {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                ex.options.insert(fid, crate::services::fleet_social::fleet_social_notify::options_keyval(true, true, None));
                ex.wing_squad_hierarchy.insert(fid, (vec![1], vec![1]));
                ex.fleet_names.insert(fid, format!("Fleet {fid}"));
                ex.max_sizes.insert(fid, 256);
                ex.auto_join_squads.insert(fid, None);
            }
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
                if corp_id < 98_000_000 {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "NPC corporations cannot create alliances".into() });
                }
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if corp.ceo_id != char_id {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only the Corporation CEO can create an alliance".into() });
                }
                if corp.alliance_id.is_some() {
                    return Err(BridgeRpcError { code: "AlreadyInAlliance".into(), message: "Corporation is already in an alliance".into() });
                }
                let new_bal = corp
                    .debit_division(1, alliance_fee)
                    .ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Creating an alliance requires 1,000,000,000 ISK in the corporation master wallet".into(),
                    })?
                    .to_f64_isk();
                let next_jid = snap.corp_wallet_journals.values().flatten().map(|j| j.id).max().unwrap_or(500_000) + 1;
                snap.corp_wallet_journals.entry((corp_id, 1)).or_default().push(eve_db::snapshot_ext::CorpWalletJournalRecord {
                    id: next_jid,
                    date_unix: now_unix,
                    ref_type: "alliance_creation_fee".into(),
                    description: format!("Alliance registration fee for {name} [{short_name}]"),
                    amount: -1_000_000_000.0,
                    balance: new_bal,
                    first_party_id: Some(corp_id),
                    second_party_id: Some(1_000_125),
                    reason: Some(short_name.to_string()),
                });
                let corp = snap.corporations.get_mut(&corp_id).unwrap();
                let members_snapshot: Vec<i64> = corp.members.iter().copied().collect();
                snap.next_entity_id += 1;
                let aid = snap.allocate_alliance_id();
                if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                    corp.alliance_id = Some(aid);
                }
                for mid in &members_snapshot {
                    if let Some(ch) = snap.characters.get_mut(mid) {
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
                snap.alliance_lifecycle_meta.insert(
                    aid,
                    eve_db::snapshot_ext::AllianceLifecycleMeta {
                        creator_id: char_id,
                        creator_corporation_id: corp_id,
                        date_founded_unix: now_unix,
                        faction_id: None,
                    },
                );
                Ok((aid, members_snapshot))
            });
            match res {
                Ok((aid, _members_snapshot)) => {
                    session.alliance_id = Some(aid);
                    with_alliance_war_state(ctx, |st| {
                        let mut meta = AllianceMeta::default();
                        meta.description = description.to_string();
                        meta.url = url.to_string();
                        meta.emblem_entitlements = true;
                        meta.executor_votes.insert(corp_id, corp_id);
                        meta.employment_history.push((corp_id, now_unix, None));
                        st.alliances.insert(aid, meta);
                    });
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
            let rpc = crate::router::RpcArgs::positional(args);
            let aid = match rpc.req_positive_i64(0, "allianceID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let msg = match rpc.opt_str(1, "applicationText") {
                Ok(v) => v.unwrap_or(""),
                Err(e) => return Some(Err(e)),
            };
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
            with_alliance_war_state(ctx, |st| {
                let meta = st.alliances.entry(aid).or_default();
                meta.applications.retain(|a| a.0 != corp_id);
                meta.applications.push((corp_id, msg.to_string(), now_unix, "Pending".into(), None));
            });
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
            Some(handle_update_alliance_application(ctx, session, args, _kwargs))
        }

        ("corpRegistry", "GetAllianceApplications") | ("allianceRegistry", "GetAllianceApplications") => {
            Some(handle_get_alliance_applications(ctx, session, args))
        }

        ("corpRegistry", "DeleteAllianceApplication") => {
            Some(handle_delete_alliance_application(ctx, session, args))
        }

        ("allianceRegistry", "allianceID" | "shortName" | "executorCorpID" | "creatorCorpID" | "creatorCharID" | "allowWar") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = args.first().and_then(|a| a.as_i64()).or_else(|| bound_obj_id.filter(|&id| id > 0)).or(session.alliance_id).unwrap_or(0);
            let info = ctx.db.with_snapshot(|snap| {
                let a = snap.alliances.get(&aid)?;
                let meta = snap.alliance_lifecycle_meta.get(&aid);
                let creator_corp = meta.map(|m| m.creator_corporation_id).unwrap_or(a.executor_corp_id);
                let creator_char = meta.map(|m| m.creator_id).or_else(|| snap.corporations.get(&creator_corp).map(|c| c.ceo_id)).unwrap_or(0);
                Some((a.alliance_id, a.short_name.clone(), a.executor_corp_id, creator_corp, creator_char))
            });
            let Some((aid_val, short_name, exec_id, creator_corp_id, creator_char_id)) = info else {
                return Some(Err(BridgeRpcError { code: "AllianceNotFound".into(), message: format!("Alliance {aid} not found") }));
            };
            let rep = match method {
                "allianceID" => PyRep::Integer(aid_val),
                "shortName" => PyRep::String(short_name),
                "executorCorpID" => PyRep::Integer(exec_id),
                "creatorCorpID" => PyRep::Integer(creator_corp_id),
                "creatorCharID" => PyRep::Integer(creator_char_id),
                "allowWar" => PyRep::Bool(true),
                _ => PyRep::None,
            };
            Some(Ok(rep))
        }

        ("allianceRegistry", "GetAlliance") | ("allianceRegistry", "GetAlliancePublicInfo") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = args.first().and_then(|a| a.as_i64()).or_else(|| bound_obj_id.filter(|&id| id > 0)).or(session.alliance_id).unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let info = ctx.db.with_snapshot(|snap| {
                let a = snap.alliances.get(&aid)?;
                let meta = snap.alliance_lifecycle_meta.get(&aid);
                let creator_corp = meta.map(|m| m.creator_corporation_id).unwrap_or(a.executor_corp_id);
                let creator_char = meta.map(|m| m.creator_id).or_else(|| snap.corporations.get(&creator_corp).map(|c| c.ceo_id)).unwrap_or(0);
                let start_date = meta.map(|m| m.date_founded_unix as i64).unwrap_or(0);
                Some((a.alliance_id, a.name.clone(), a.short_name.clone(), a.executor_corp_id, a.member_corp_ids.len() as i64, creator_corp, creator_char, start_date))
            });
            let Some((id, name, short, exec, cnt, creator_corp, creator_char, start_date)) = info else {
                return Some(Err(BridgeRpcError { code: "AllianceNotFound".into(), message: format!("Alliance {aid} not found") }));
            };
            let (desc, url, logo, emblem, dictatorial) = read_alliance_war_state(ctx, |st| {
                st.alliances.get(&aid).map(|m| (m.description.clone(), m.url.clone(), m.logo, m.emblem_entitlements, m.dictatorial)).unwrap_or_default()
            });
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(id)), ("allianceName", PyRep::String(name)), ("shortName", PyRep::String(short)),
                ("executorCorpID", PyRep::Integer(exec)), ("creatorCorpID", PyRep::Integer(creator_corp)), ("creatorCharID", PyRep::Integer(creator_char)),
                ("startDate", PyRep::Integer(start_date)), ("memberCount", PyRep::Integer(cnt)), ("description", PyRep::String(desc)),
                ("url", PyRep::String(url)), ("shape1", PyRep::Integer(logo.0)), ("shape2", PyRep::Integer(logo.1)),
                ("shape3", PyRep::Integer(logo.2)), ("color", PyRep::Integer(logo.3)), ("emblemEntitlements", PyRep::Bool(emblem)),
                ("dictatorial", PyRep::Bool(dictatorial)), ("allowWar", PyRep::Bool(true)),
                ("deleted", PyRep::Bool(false)), ("warFactionID", PyRep::None),
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
            Some(handle_get_alliance_employment_record(ctx, session, args))
        }

        ("allianceRegistry", "GetRankedAlliances") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let max_len = args.first().and_then(|a| a.as_i64());
            let rows = ctx.db.with_snapshot(|snap| {
                let mut list: Vec<_> = snap.alliances.values().map(|a| {
                    let pilot_count: i64 = a.member_corp_ids.iter().map(|cid| {
                        snap.corporations.get(cid).map(|c| c.members.len().max(1) as i64).unwrap_or(1)
                    }).sum();
                    let meta = snap.alliance_lifecycle_meta.get(&a.alliance_id);
                    let creator_corp = meta.map(|m| m.creator_corporation_id).unwrap_or(a.executor_corp_id);
                    let creator_char = meta.map(|m| m.creator_id).or_else(|| snap.corporations.get(&creator_corp).map(|c| c.ceo_id)).unwrap_or(0);
                    let start_date = meta.map(|m| m.date_founded_unix as i64).unwrap_or(0);
                    (a, pilot_count, a.member_corp_ids.len() as i64, creator_corp, creator_char, start_date)
                }).collect();
                list.sort_by_key(|(a, pilots, corps, _, _, _)| (std::cmp::Reverse(*pilots), std::cmp::Reverse(*corps), a.alliance_id));
                if let Some(m) = max_len.filter(|&m| m > 0) {
                    list.truncate(m as usize);
                }
                list.into_iter().map(|(a, pilots, _, creator_corp, creator_char, start_date)| {
                    vec![
                        PyRep::Integer(a.alliance_id),
                        PyRep::String(a.name.clone()),
                        PyRep::String(a.short_name.clone()),
                        PyRep::Integer(a.executor_corp_id),
                        PyRep::Integer(pilots),
                        PyRep::Integer(creator_corp),
                        PyRep::Integer(creator_char),
                        PyRep::Integer(start_date),
                    ]
                }).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(
                vec!["allianceID", "allianceName", "shortName", "executorCorpID", "memberCount", "creatorCorpID", "creatorID", "startDate"],
                rows,
            )))
        }

        ("allianceRegistry", "GetDaysInAlliance") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
            let aid = session.alliance_id.unwrap_or(0);
            let _ = sync_alliance_timers(ctx, aid);
            let now_unix = ctx.now_unix();
            let start = read_alliance_war_state(ctx, |st| {
                st.alliances.get(&aid).and_then(|m| {
                    m.employment_history.iter().rev().find(|r| r.0 == corp_id && r.2.is_none()).map(|r| r.1)
                })
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
            let corp_id = match require_corp_director(ctx, session) { Ok(c) => c, Err(e) => return Some(Err(e)) };
            let aid = session.alliance_id.unwrap_or(0);
            let target_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let _ = sync_alliance_timers(ctx, aid);
            let member_corps: HashSet<i64> = ctx.db.with_snapshot(|snap| {
                snap.alliances.get(&aid).map(|a| a.member_corp_ids.clone()).unwrap_or_default()
            });
            if !member_corps.contains(&corp_id) || !member_corps.contains(&target_corp_id) {
                return Some(Err(BridgeRpcError { code: "NotInAlliance".into(), message: "Both corporations must be members of the alliance".into() }));
            }
            if read_alliance_war_state(ctx, |st| st.alliances.get(&aid).is_some_and(|m| m.dictatorial)) {
                return Some(Err(BridgeRpcError {
                    code: "AllianceIsDictatorial".into(),
                    message: format!("Alliance {aid} is dictatorial; executor support votes are disabled"),
                }));
            }
            let new_exec = with_alliance_war_state(ctx, |st| {
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
            });
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
            if let Err(e) = require_corp_director(ctx, session) {
                return Some(Err(e));
            }
            if has_active_war {
                return Some(Err(BridgeRpcError { code: "CannotLeaveAllianceDuringWar".into(), message: "Member corporations cannot leave an alliance during an active war".into() }));
            }
            let immediate = method == "KickMember";
            if !immediate {
                let eff = now_unix + 86_400;
                with_alliance_war_state(ctx, |st| {
                    st.alliances.entry(aid).or_default().pending_leaves.insert(target_corp, eff);
                });
                return Some(Ok(PyRep::keyval([
                    ("allianceID", PyRep::Integer(aid)),
                    ("corporationID", PyRep::Integer(target_corp)),
                    ("leaveEffectiveTime", PyRep::Integer(eff as i64)),
                    ("delaySeconds", PyRep::Integer(86_400)),
                    ("allianceClosed", PyRep::Bool(false)),
                ])));
            }
            with_alliance_war_state(ctx, |st| {
                st.alliances.entry(aid).or_default().pending_leaves.insert(target_corp, now_unix);
            });
            let closed = sync_alliance_timers(ctx, aid);
            if target_corp == caller_corp {
                session.alliance_id = None;
            }
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
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let rpc = crate::router::RpcArgs::positional(args);
            let contact_id = match rpc.req_positive_i64(0, "contactID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let standing = match rpc.req_finite_f64(1, "standing") {
                Ok(s) if (-10.0..=10.0).contains(&s) => s,
                _ => {
                    return Some(Err(BridgeRpcError { code: "InvalidStanding".into(), message: "Standing must be between -10 and +10".into() }));
                }
            };
            let label_mask = if matches!(args.get(2), Some(PyRep::Bool(_))) {
                args.get(4)
                    .and_then(|a| a.as_i64())
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0)
            } else {
                match rpc.opt_u64(2, "labelMask") {
                    Ok(v) => v.unwrap_or(0),
                    Err(e) => return Some(Err(e)),
                }
            };
            let prev = ctx.db.with_mut_snapshot(|snap| {
                let ctype = if snap.alliances.contains_key(&contact_id) {
                    "alliance"
                } else if snap.corporations.contains_key(&contact_id)
                    || ctx.sde.npc_corporations.contains_key(&contact_id)
                {
                    "corporation"
                } else {
                    "character"
                };
                let list = snap.alliance_contacts.entry(aid).or_default();
                let prev = list
                    .iter()
                    .find(|c| c.contact_id == contact_id)
                    .map(|c| (c.standing, label_mask_from_ids(&c.label_ids)));
                list.retain(|c| c.contact_id != contact_id);
                list.push(eve_db::SocialContactRecord {
                    contact_id,
                    contact_type: ctype.into(),
                    standing,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: label_ids_from_mask(label_mask),
                });
                list.sort_by_key(|c| c.contact_id);
                prev
            });
            let (old_st, old_mask) = prev.map_or((PyRep::None, PyRep::None), |(s, m)| (PyRep::Real(s), PyRep::Integer(m as i64)));
            let rel_change = PyRep::Dict(vec![
                (PyRep::String(("toID").into()), PyRep::Tuple(vec![prev.map_or(PyRep::None, |_| PyRep::Integer(contact_id)), PyRep::Integer(contact_id)])),
                (PyRep::String(("relationshipID").into()), PyRep::Tuple(vec![old_st, PyRep::Real(standing)])),
                (PyRep::String(("labelMask").into()), PyRep::Tuple(vec![old_mask, PyRep::Integer(label_mask as i64)])),
            ]);
            ctx.notify(NotificationTarget::Alliance(aid), "OnAllianceRelationshipChanged", "allianceid", vec![PyRep::Integer(aid), PyRep::Integer(contact_id), rel_change]);
            ctx.notify(NotificationTarget::Alliance(aid), "OnSetAllianceStanding", "allianceid", vec![PyRep::Integer(contact_id), PyRep::Real(standing)]);
            ctx.notify(
                NotificationTarget::Alliance(aid),
                "OnOrganizationContactsUpdated",
                "allianceid",
                vec![
                    PyRep::Integer(aid),
                    PyRep::List(vec![PyRep::keyval([
                        ("contactID", PyRep::Integer(contact_id)),
                        ("standing", PyRep::Real(standing)),
                        ("labelMask", PyRep::Integer(label_mask as i64)),
                    ])]),
                ],
            );
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
            let removed_contacts = ctx.db.with_mut_snapshot(|snap| {
                let mut out = Vec::new();
                if let Some(list) = snap.alliance_contacts.get_mut(&aid) {
                    for &cid in &ids {
                        let prev = list
                            .iter()
                            .find(|c| c.contact_id == cid)
                            .map(|c| (c.standing, label_mask_from_ids(&c.label_ids)));
                        list.retain(|c| c.contact_id != cid);
                        out.push((cid, prev));
                    }
                }
                out
            });
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
            let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.alliance_contacts
                    .get(&aid)
                    .map(|list| {
                        list.iter()
                            .map(|c| {
                                vec![
                                    PyRep::Integer(c.contact_id),
                                    PyRep::Real(c.standing),
                                    PyRep::Integer(label_mask_from_ids(&c.label_ids) as i64),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            rows.sort_by_key(|r| r[0].as_i64());
            Some(Ok(PyRep::rowset(vec!["contactID", "relationshipID", "labelMask"], rows)))
        }

        ("allianceRegistry", "CreateLabel") | ("allianceRegistry", "EditLabel") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let (lid, name, color) = ctx.db.with_mut_snapshot(|snap| {
                let labels = snap.alliance_contact_labels.entry(aid).or_default();
                let (lid, name, color) = if method == "EditLabel"
                    && args.first().and_then(|a| a.as_i64()).is_some()
                {
                    let id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                    let existing_name = labels
                        .iter()
                        .find(|(lid, _)| *lid == id)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_else(|| "Coalition".into());
                    let n = args
                        .get(1)
                        .and_then(|a| a.as_str())
                        .unwrap_or(&existing_name)
                        .to_string();
                    let c = args
                        .get(2)
                        .and_then(|a| a.as_i64())
                        .map(|v| v as u32)
                        .unwrap_or(0x00FF00);
                    (id, n, c)
                } else {
                    let n = args
                        .first()
                        .and_then(|a| a.as_str())
                        .unwrap_or("Coalition")
                        .to_string();
                    let c = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0x00FF00) as u32;
                    let next_bit = (0..30)
                        .map(|b| 1i64 << b)
                        .find(|id| !labels.iter().any(|(lid, _)| lid == id))
                        .unwrap_or(1);
                    (next_bit, n, c)
                };
                labels.retain(|(id, _)| *id != lid);
                labels.push((lid, name.clone()));
                labels.sort_by_key(|(id, _)| *id);
                (lid, name, color)
            });
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(lid)),
                ("name", PyRep::String(name)),
                ("color", PyRep::Integer(color as i64)),
            ])))
        }

        ("allianceRegistry", "GetLabels") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = session.alliance_id.unwrap_or(0);
            let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.alliance_contact_labels
                    .get(&aid)
                    .map(|labels| {
                        labels
                            .iter()
                            .map(|(lid, name)| {
                                vec![
                                    PyRep::Integer(*lid),
                                    PyRep::String(name.clone()),
                                    PyRep::Integer(0x00FF00),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            rows.sort_by_key(|r| r[0].as_i64());
            Some(Ok(PyRep::rowset(vec!["labelID", "name", "color"], rows)))
        }

        ("allianceRegistry", "DeleteLabel")
        | ("allianceRegistry", "AssignLabels")
        | ("allianceRegistry", "RemoveLabels") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let (lid, contact_ids): (i64, Vec<i64>) = match (args.first(), args.get(1)) {
                (Some(PyRep::List(items) | PyRep::Tuple(items)), _) => (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    items.iter().filter_map(|v| v.as_i64()).collect(),
                ),
                (Some(a0), Some(PyRep::List(items) | PyRep::Tuple(items))) => (
                    a0.as_i64().unwrap_or(1),
                    items.iter().filter_map(|v| v.as_i64()).collect(),
                ),
                _ => {
                    let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                    let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    if a0 >= 1_000_000 && a1 > 0 && a1 < 1_000_000 {
                        (a1, vec![a0])
                    } else {
                        (a0, vec![a1])
                    }
                }
            };
            ctx.db.with_mut_snapshot(|snap| {
                if method == "DeleteLabel" {
                    if let Some(labels) = snap.alliance_contact_labels.get_mut(&aid) {
                        labels.retain(|(id, _)| *id != lid);
                    }
                    if let Some(contacts) = snap.alliance_contacts.get_mut(&aid) {
                        for c in contacts {
                            let mask = label_mask_from_ids(&c.label_ids) & !(lid as u64);
                            c.label_ids = label_ids_from_mask(mask);
                        }
                    }
                } else {
                    let contacts = snap.alliance_contacts.entry(aid).or_default();
                    for cid in contact_ids {
                        if let Some(entry) = contacts.iter_mut().find(|c| c.contact_id == cid) {
                            let mut mask = label_mask_from_ids(&entry.label_ids);
                            if method == "AssignLabels" {
                                mask |= lid as u64;
                            } else {
                                mask &= !(lid as u64);
                            }
                            entry.label_ids = label_ids_from_mask(mask);
                        } else if method == "AssignLabels" {
                            contacts.push(eve_db::SocialContactRecord {
                                contact_id: cid,
                                contact_type: "character".into(),
                                standing: 0.0,
                                is_watched: false,
                                is_blocked: false,
                                label_ids: label_ids_from_mask(lid as u64),
                            });
                        }
                    }
                    contacts.sort_by_key(|c| c.contact_id);
                }
            });
            Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(lid))])))
        }

        ("allianceRegistry", "AddBulletin") | ("allianceRegistry", "UpdateBulletin") => {
            Some(handle_upsert_alliance_bulletin(ctx, session, method, args))
        }

        ("allianceRegistry", "GetBulletins") | ("allianceRegistry", "GetAllianceBulletins") => {
            Some(handle_get_alliance_bulletins(ctx, session))
        }

        ("allianceRegistry", "DeleteBulletin") => {
            Some(handle_delete_alliance_bulletin(ctx, session, args))
        }

        ("allianceRegistry", "UpdateAlliance") => {
            Some(handle_update_alliance(ctx, session, args, _kwargs))
        }

        // =====================================================================
        // Corp Deliveries (`feat:corporation.corp-deliveries`)
        // =====================================================================
        ("invbroker", "DeliverToCorpHangar") | ("invbroker", "DeliverToCorpMember") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(docked_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError::user_error("NotDocked")));
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
                Some(PyRep::List(xs) | PyRep::Tuple(xs)) => xs.iter().filter_map(|x| x.as_i64()).filter(|&id| id > 0).collect(),
                _ => Vec::new(),
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError { code: "ItemNotFound".into(), message: "Deliver requires at least one valid itemID".into() }));
            }
            let first_item_id = item_ids[0];
            let target_id_or_div = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                for &iid in &item_ids {
                    let Some(item) = snap.items.get(&iid) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {iid} not found") });
                    };
                    if snap.items.get(&item.location_id).is_some_and(|p| crate::services::inventory_fitting::fitting_containers_ships::is_container_group(ctx.sde.types.get(&p.type_id).map(|t| t.group_id).unwrap_or(0), p.type_id)) {
                        return Err(BridgeRpcError { code: "ItemInsideContainer".into(), message: format!("Item {iid} is inside a container and must be moved to item hangar before delivery") });
                    }
                    if snap.item_custom_names.get(&iid).is_some_and(|name| name.starts_with("courier_cargo:")) {
                        return Err(BridgeRpcError { code: "ItemCannotBeTransferred".into(), message: "Courier mission items cannot be transferred to corporation hangars or members".into() });
                    }
                }
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if !corp.members.contains(&char_id) {
                    return Err(BridgeRpcError { code: "NotInCorporation".into(), message: "Character is not a member of the corporation".into() });
                }
                if method == "DeliverToCorpMember" && target_id_or_div != char_id && !corp.members.contains(&target_id_or_div) {
                    return Err(BridgeRpcError { code: "TargetNotInCorporation".into(), message: format!("Recipient {target_id_or_div} is not in corporation {corp_id}") });
                }
                let (is_ceo, caller_roles) = snap.effective_corp_roles_at(
                    corp_id,
                    char_id,
                    Some(docked_id),
                    0,
                    ctx.now_unix(),
                );
                let has_trader = is_ceo
                    || corp_roles::has_role(caller_roles, corp_roles::ROLE_DIRECTOR)
                    || corp_roles::has_role(caller_roles, corp_roles::ROLE_ACCOUNTANT)
                    || corp_roles::has_role(caller_roles, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                    || corp_roles::has_role(caller_roles, corp_roles::ROLE_TRADER);
                let active_ship_id = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id));
                let office_folder_id = snap
                    .items
                    .values()
                    .find(|i| i.type_id == 27 && i.owner_id == corp_id && i.location_id == docked_id)
                    .map(|i| i.item_id);

                for &iid in &item_ids {
                    let Some(item) = snap.items.get(&iid) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {iid} not found") });
                    };
                    if snap.item_custom_names.get(&iid).is_some_and(|name| name.starts_with("courier_cargo:")) {
                        return Err(BridgeRpcError { code: "ItemCannotBeTransferred".into(), message: "Courier mission items cannot be transferred to corporation hangars or members".into() });
                    }
                    if Some(iid) == session.ship_id || Some(iid) == active_ship_id || snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, iid) {
                        return Err(BridgeRpcError { code: "CannotDeliverActiveShip".into(), message: "Cannot deliver the currently active ship".into() });
                    }
                    let flag_u16 = item.flag as u16;
                    if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
                        || item.flag == InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(iid)
                    {
                        return Err(BridgeRpcError { code: "ItemCannotBeTransferred".into(), message: "Fitted or locked items cannot be delivered".into() });
                    }
                    if item.owner_id != char_id && item.owner_id != corp_id {
                        return Err(BridgeRpcError { code: "ItemNotOwned".into(), message: "Cannot deliver an item not owned by caller or corporation".into() });
                    }
                    if item.location_id != docked_id && Some(item.location_id) != office_folder_id {
                        return Err(BridgeRpcError { code: "ItemNotAtLocation".into(), message: format!("Item {iid} is not located at docked facility {docked_id}") });
                    }
                    if item.owner_id == corp_id {
                        if !has_trader {
                            return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Delivering corporation-owned items requires Trader, Accountant, or Director role".into() });
                        }
                        if (116..=122).contains(&flag_u16) {
                            let req_take_role = 1u64 << (13 + (flag_u16 - 116));
                            if !is_ceo
                                && !corp_roles::has_role(caller_roles, corp_roles::ROLE_DIRECTOR)
                                && !corp_roles::has_role(caller_roles, corp_roles::ROLE_ACCOUNTANT)
                                && !corp_roles::has_role(caller_roles, req_take_role)
                            {
                                return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Delivering items from a corporation hangar division requires division take access, Accountant, or Director role".into() });
                            }
                        }
                    }
                }

                let mut last_res = (corp_id, InventoryFlag::Deliveries as i64);
                let mut personal_deliveries: Vec<(u32, i32)> = Vec::new();
                for &iid in &item_ids {
                    if let Some(item) = snap.items.get_mut(&iid) {
                        if method == "DeliverToCorpHangar" {
                            if item.owner_id == char_id && item.quantity > 0 {
                                personal_deliveries.push((item.type_id, item.quantity));
                            }
                            item.owner_id = corp_id;
                            let out_flag = match target_id_or_div { 1..=7 => 115 + target_id_or_div, 116..=122 => target_id_or_div, _ => InventoryFlag::Deliveries as i64 };
                            item.flag = InventoryFlag::from_u16(out_flag as u16);
                            item.version += 1;
                            last_res = (item.owner_id, out_flag);
                        } else {
                            item.owner_id = target_id_or_div;
                            item.flag = InventoryFlag::Deliveries;
                            item.version += 1;
                            last_res = (item.owner_id, item.flag as i64);
                        }
                    }
                }
                Ok((last_res, personal_deliveries))
            });
            match res {
                Ok(((new_owner, flag_id), personal_deliveries)) => {
                    for (tid, qty) in personal_deliveries {
                        social_corp_projects::record_server_project_activity(
                            ctx,
                            char_id,
                            &["DeliverItem"],
                            qty as u64,
                            Some(tid as i64),
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(first_item_id)),
                        ("ownerID", PyRep::Integer(new_owner)),
                        ("flagID", PyRep::Integer(flag_id)),
                        ("delivered", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("allianceRegistry", "UpdateLogo") => {
            let aid = match require_executor(ctx, session) {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };
            let s1 = args.first().and_then(|a| a.as_i64()).unwrap_or(10);
            let s2 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(20);
            let s3 = args.get(2).and_then(|a| a.as_i64()).unwrap_or(30);
            let c1 = args.get(3).and_then(|a| a.as_i64()).unwrap_or(5);
            let c2 = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
            let c3 = args.get(5).and_then(|a| a.as_i64()).unwrap_or(0);
            let (old_logo, old_extra) = ctx.db.with_mut_snapshot(|snap| {
                let st = snap.corp_governance.entry(aid).or_default();
                (
                    std::mem::replace(&mut st.logo, (s1, s2, s3, c1)),
                    std::mem::replace(&mut st.logo_extra_colors, (c2, c3)),
                )
            });
            ctx.notify(
                NotificationTarget::Alliance(aid),
                "OnAllianceLogoReady",
                "allianceid",
                vec![PyRep::Integer(aid), PyRep::Integer(128)],
            );
            let change = PyRep::Dict(vec![
                (PyRep::String(("shape1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.0), PyRep::Integer(s1)])),
                (PyRep::String(("shape2").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.1), PyRep::Integer(s2)])),
                (PyRep::String(("shape3").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.2), PyRep::Integer(s3)])),
                (PyRep::String(("color1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.3), PyRep::Integer(c1)])),
                (PyRep::String(("color2").into()), PyRep::Tuple(vec![PyRep::Integer(old_extra.0), PyRep::Integer(c2)])),
                (PyRep::String(("color3").into()), PyRep::Tuple(vec![PyRep::Integer(old_extra.1), PyRep::Integer(c3)])),
            ]);
            ctx.notify(
                NotificationTarget::Alliance(aid),
                "OnAllianceChanged",
                "allianceid",
                vec![PyRep::Integer(aid), change],
            );
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(aid)),
                ("shape1", PyRep::Integer(s1)),
                ("shape2", PyRep::Integer(s2)),
                ("shape3", PyRep::Integer(s3)),
                ("color1", PyRep::Integer(c1)),
                ("color2", PyRep::Integer(c2)),
                ("color3", PyRep::Integer(c3)),
                ("logoPath", PyRep::String(format!("res:/UI/Texture/AllianceLogos/{aid}_128.png"))),
            ])))
        }

        ("allianceRegistry", "DeclareWarAgainst") => {
            social_wars::handle_wars(ctx, session, "warRegistry", "DeclareWarAgainst", args, bound_obj_id)
        }
        ("allianceRegistry", "GetCapitalSystemInfo") => {
            Some(handle_get_capital_system_info(ctx, session, bound_obj_id))
        }
        ("allianceRegistry", "SetCapitalSystem") => {
            Some(handle_set_capital_system(ctx, session, args))
        }
        ("allianceRegistry", "CancelCapitalSystemTransition") => {
            Some(handle_cancel_capital_system_transition(ctx, session))
        }
        ("allianceRegistry", "GetPrimeTimeInfo") => {
            Some(handle_get_prime_time_info(ctx, session, bound_obj_id))
        }
        ("allianceRegistry", "SetPrimeHour") => {
            Some(handle_set_prime_hour(ctx, session, args))
        }
        ("allianceRegistry", "GetAllianceSovereigntyStructuresInfo") => {
            Some(handle_get_alliance_sov_structures_info(ctx, session, bound_obj_id))
        }
        ("allianceRegistry", "GetBillBalance") => {
            Some(handle_get_alliance_bill_balance(ctx, session, args, bound_obj_id))
        }
        ("allianceRegistry", "GetBills") | ("allianceRegistry", "GetBillsReceivable") => {
            Some(handle_get_alliance_bills(ctx, session, method == "GetBillsReceivable", bound_obj_id))
        }
        ("allianceRegistry", "PayBill") => {
            Some(handle_pay_alliance_bill(ctx, session, args, _kwargs))
        }
        ("allianceRegistry", "GMForceAcceptApplication") => {
            Some(handle_gm_force_accept_application(ctx, session, args, bound_obj_id))
        }
        ("allianceRegistry", "UpdateBulletinOrder") => {
            Some(handle_update_alliance_bulletin_order(ctx, session, args))
        }

        _ => social_wars::handle_wars(ctx, session, service, method, args, bound_obj_id),
    }
}

fn handle_update_alliance_application(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let applicant_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let is_rejected = args.get(2).is_some_and(|a| {
        a.as_i64() == Some(3)
            || a.as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("rejected") || s.eq_ignore_ascii_case("declined"))
    }) || args.get(1).and_then(|a| a.as_str()).is_some_and(|s| {
        s.eq_ignore_ascii_case("rejected") || s.eq_ignore_ascii_case("declined")
    }) || kwargs.get("state").is_some_and(|a| {
        a.as_i64() == Some(3)
            || a.as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("rejected") || s.eq_ignore_ascii_case("declined"))
    });
    let now_unix = ctx.now_unix();
    let eff_unix = now_unix + 86_400;
    let found = with_alliance_war_state(ctx, |st| {
        let meta = st.alliances.entry(aid).or_default();
        if let Some(app) = meta
            .applications
            .iter_mut()
            .find(|a| a.0 == applicant_corp_id && a.3 == "Pending")
        {
            if is_rejected {
                app.3 = "Rejected".into();
                app.4 = None;
            } else {
                app.3 = "Accepted".into();
                app.4 = Some(eff_unix);
            }
            true
        } else {
            false
        }
    });
    if !found {
        return Err(BridgeRpcError {
            code: "ApplicationNotFound".into(),
            message: format!("No pending application from corporation {applicant_corp_id}"),
        });
    }
    let _ = sync_alliance_timers(ctx, aid);
    let new_status = if is_rejected { "Rejected" } else { "Accepted" };
    let mut change_entries = vec![(
        PyRep::String("state".into()),
        PyRep::Tuple(vec![
            PyRep::String("Pending".into()),
            PyRep::String(new_status.into()),
        ]),
    )];
    if !is_rejected {
        change_entries.push((
            PyRep::String("joinEffectiveTime".into()),
            PyRep::Tuple(vec![PyRep::None, PyRep::Integer(eff_unix as i64)]),
        ));
    }
    let app_change = PyRep::Dict(change_entries);
    let app_args = vec![
        PyRep::Integer(aid),
        PyRep::Integer(applicant_corp_id),
        app_change,
    ];
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnAllianceApplicationChanged",
        "allianceid",
        app_args.clone(),
    );
    ctx.notify(
        NotificationTarget::Corporation(applicant_corp_id),
        "OnAllianceApplicationChanged",
        "corpid",
        app_args,
    );
    Ok(PyRep::keyval([
        ("allianceID", PyRep::Integer(aid)),
        ("corporationID", PyRep::Integer(applicant_corp_id)),
        ("status", PyRep::String(new_status.into())),
        (
            "joinEffectiveTime",
            if is_rejected {
                PyRep::None
            } else {
                PyRep::Integer(eff_unix as i64)
            },
        ),
        (
            "delaySeconds",
            PyRep::Integer(if is_rejected { 0 } else { 86_400 }),
        ),
    ]))
}

fn handle_get_alliance_applications(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let target_aid = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
    let caller_corp = session.corporation_id.filter(|&id| id > 0);
    let caller_alliance = session.alliance_id.filter(|&id| id > 0);
    if let Some(aid) = target_aid.or(caller_alliance) {
        let _ = sync_alliance_timers(ctx, aid);
    } else {
        sync_all_alliance_timers(ctx);
    }
    let rows: Vec<Vec<PyRep>> = read_alliance_war_state(ctx, |st| {
        let mut out = Vec::new();
        let mut alliance_ids: Vec<i64> = match target_aid {
            Some(aid) => vec![aid],
            None => st.alliances.keys().copied().collect(),
        };
        alliance_ids.sort_unstable();
        for aid in alliance_ids {
            let Some(m) = st.alliances.get(&aid) else {
                continue;
            };
            let is_own_alliance = caller_alliance == Some(aid);
            for (cid, msg, ts, status, eff) in &m.applications {
                let is_own_corp = caller_corp == Some(*cid);
                if !is_own_alliance && !is_own_corp {
                    continue;
                }
                out.push(vec![
                    PyRep::Integer(aid),
                    PyRep::Integer(*cid),
                    PyRep::String(msg.clone()),
                    PyRep::Integer(*ts as i64),
                    PyRep::String(status.clone()),
                    eff.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None),
                ]);
            }
        }
        out
    });
    Ok(PyRep::rowset(
        vec![
            "allianceID",
            "corporationID",
            "applicationText",
            "applicationDateTime",
            "status",
            "joinEffectiveTime",
        ],
        rows,
    ))
}

fn handle_delete_alliance_application(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let corp_id = require_corp_director(ctx, session)?;
    let aid = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.alliance_id)
        .unwrap_or(0);
    with_alliance_war_state(ctx, |st| {
        if let Some(m) = st.alliances.get_mut(&aid) {
            m.applications.retain(|a| a.0 != corp_id);
        }
    });
    Ok(PyRep::keyval([("deleted", PyRep::Bool(true))]))
}

fn handle_get_alliance_employment_record(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.alliance_id)
        .unwrap_or(0);
    let _ = sync_alliance_timers(ctx, aid);
    let rows: Vec<Vec<PyRep>> = read_alliance_war_state(ctx, |st| {
        st.alliances
            .get(&aid)
            .map(|m| {
                m.employment_history
                    .iter()
                    .map(|(cid, start, end)| {
                        vec![
                            PyRep::Integer(aid),
                            PyRep::Integer(*cid),
                            PyRep::Integer(*start as i64),
                            end.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None),
                        ]
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    Ok(PyRep::rowset(
        vec!["allianceID", "corporationID", "startDate", "endDate"],
        rows,
    ))
}

fn handle_upsert_alliance_bulletin(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let off = usize::from(method == "UpdateBulletin");
    let target = if off == 1 {
        args.first().and_then(|a| a.as_i64())
    } else {
        None
    };
    let title = args.get(off).and_then(|a| a.as_str()).unwrap_or("").to_string();
    let body = args.get(off + 1).and_then(|a| a.as_str()).unwrap_or("").to_string();
    if title.is_empty() {
        return Err(BridgeRpcError {
            code: "BulletinTitleRequired".into(),
            message: "A bulletin needs a title".into(),
        });
    }
    let now_unix = ctx.now_unix();
    let bid = with_alliance_war_state(ctx, |st| {
        let meta = st.alliances.entry(aid).or_default();
        match target {
            Some(id) => match meta.bulletins.iter_mut().find(|b| b.0 == id) {
                Some(b) => {
                    *b = (id, title.clone(), body.clone(), char_id, now_unix);
                    Ok(id)
                }
                None => Err(BridgeRpcError {
                    code: "BulletinNotFound".into(),
                    message: format!("Bulletin {id} not found"),
                }),
            },
            None => {
                let id = meta.bulletins.iter().map(|b| b.0).max().unwrap_or(0) + 1;
                meta.bulletins
                    .push((id, title.clone(), body.clone(), char_id, now_unix));
                Ok(id)
            }
        }
    })?;
    Ok(PyRep::keyval([
        ("bulletinID", PyRep::Integer(bid)),
        ("title", PyRep::String(title)),
        ("body", PyRep::String(body)),
    ]))
}

fn handle_get_alliance_bulletins(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = session.alliance_id.unwrap_or(0);
    let rows: Vec<Vec<PyRep>> = read_alliance_war_state(ctx, |st| {
        st.alliances
            .get(&aid)
            .map(|m| {
                m.bulletins
                    .iter()
                    .map(|(bid, title, body, author, ts)| {
                        vec![
                            PyRep::Integer(*bid),
                            PyRep::String(title.clone()),
                            PyRep::String(body.clone()),
                            PyRep::Integer(*author),
                            PyRep::Integer(*ts as i64),
                        ]
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    Ok(PyRep::rowset(
        vec![
            "bulletinID",
            "title",
            "body",
            "editCharacterID",
            "editDateTime",
        ],
        rows,
    ))
}

fn handle_delete_alliance_bulletin(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let bid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let removed = with_alliance_war_state(ctx, |st| {
        st.alliances
            .get_mut(&aid)
            .map(|m| {
                let before = m.bulletins.len();
                m.bulletins.retain(|b| b.0 != bid);
                before != m.bulletins.len()
            })
            .unwrap_or(false)
    });
    Ok(PyRep::Bool(removed))
}

fn handle_update_alliance(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let description = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
    let url = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
    let dict_opt = args
        .get(3)
        .and_then(|a| a.as_bool())
        .or_else(|| kwargs.get("dictatorial").and_then(|a| a.as_bool()));
    let (old_desc, old_url, dictatorial) = with_alliance_war_state(ctx, |st| {
        let meta = st.alliances.entry(aid).or_default();
        if let Some(d) = dict_opt {
            meta.dictatorial = d;
        }
        (
            std::mem::replace(&mut meta.description, description.clone()),
            std::mem::replace(&mut meta.url, url.clone()),
            meta.dictatorial,
        )
    });
    let change = PyRep::Dict(vec![
        (
            PyRep::String("description".into()),
            PyRep::Tuple(vec![
                PyRep::String(old_desc),
                PyRep::String(description.clone()),
            ]),
        ),
        (
            PyRep::String("url".into()),
            PyRep::Tuple(vec![PyRep::String(old_url), PyRep::String(url.clone())]),
        ),
    ]);
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnAllianceChanged",
        "allianceid",
        vec![PyRep::Integer(aid), change],
    );
    if let Some(allow_war) = args.get(2).and_then(|a| a.as_bool()) {
        crate::services::notify_emitters_character_services::emit_allow_war_updated_for_alliance(
            ctx, aid, allow_war,
        );
    }
    Ok(PyRep::keyval([
        ("allianceID", PyRep::Integer(aid)),
        ("description", PyRep::String(description)),
        ("url", PyRep::String(url)),
        ("dictatorial", PyRep::Bool(dictatorial)),
    ]))
}

fn read_alliance_sov_cfg(snap: &eve_db::WorldStateSnapshot, aid: i64, now_unix: u64) -> eve_db::AllianceSovConfig {
    let mut cfg = snap
        .cluster_and_runtime
        .alliance_sov_cfg
        .get(&aid)
        .cloned()
        .unwrap_or_default();
    if cfg.capital_valid_after_unix > 0 && now_unix >= cfg.capital_valid_after_unix {
        if let Some(pending) = cfg.pending_capital_system_id.take() {
            cfg.capital_system_id = Some(pending);
        }
        cfg.capital_valid_after_unix = 0;
    }
    if cfg.prime_valid_after_unix > 0 && now_unix >= cfg.prime_valid_after_unix {
        cfg.current_prime_hour = cfg.new_prime_hour;
        cfg.prime_valid_after_unix = 0;
    }
    cfg
}

fn write_alliance_sov_cfg(snap: &mut eve_db::WorldStateSnapshot, aid: i64, cfg: &eve_db::AllianceSovConfig) {
    snap.cluster_and_runtime
        .alliance_sov_cfg
        .insert(aid, cfg.clone());
}

fn resolve_alliance_id(session: &SessionState, bound_obj_id: Option<i64>) -> Result<i64, BridgeRpcError> {
    bound_obj_id
        .filter(|&id| id > 0)
        .or(session.alliance_id)
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "NotInAlliance".into(),
            message: "Character is not in an alliance".into(),
        })
}

fn handle_get_capital_system_info(
    ctx: &ServerContext,
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let now_unix = ctx.now_unix();
    let cfg = ctx.db.with_snapshot(|snap| {
        let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
        if cfg.capital_system_id.is_none() {
            let sov_sys = snap
                .system_states
                .iter()
                .filter(|(_, st)| st.sov_alliance_id == Some(aid))
                .map(|(&sys_id, _)| sys_id as i64)
                .min();
            cfg.capital_system_id = sov_sys.or_else(|| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.solar_system_id as i64)
                    .or(session.solar_system_id.map(i64::from))
            });
        }
        cfg
    });
    let valid_after = if cfg.capital_valid_after_unix > 0 {
        super::market_trading::unix_to_filetime(cfg.capital_valid_after_unix)
    } else {
        0
    };
    Ok(PyRep::keyval([
        (
            "currentCapitalSystem",
            cfg.capital_system_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "newCapitalSystem",
            cfg.pending_capital_system_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        ("newCapitalSystemValidAfter", PyRep::Integer(valid_after)),
    ]))
}

fn handle_set_capital_system(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let Some(sys_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "SetCapitalSystem requires solarSystemID".into(),
        });
    };
    if !ctx.sde.solar_systems.contains_key(&(sys_id as u32)) {
        return Err(BridgeRpcError {
            code: "InvalidSolarSystem".into(),
            message: format!("Unknown solar system {sys_id}"),
        });
    }
    let now_unix = ctx.now_unix();
    let old_cap = ctx.db.try_with_mut_snapshot(|snap| -> Result<Option<i64>, BridgeRpcError> {
        let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
        let prev = cfg.capital_system_id;
        cfg.pending_capital_system_id = Some(sys_id);
        cfg.capital_valid_after_unix = now_unix + 7 * 86_400;
        write_alliance_sov_cfg(snap, aid, &cfg);
        Ok(prev)
    })?;
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnUpdateCapitalInfo",
        "allianceid",
        Vec::new(),
    );
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnCapitalSystemChanged",
        "allianceid",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(aid),
            old_cap.map(PyRep::Integer).unwrap_or(PyRep::None),
            PyRep::Integer(sys_id),
        ])],
    );
    Ok(PyRep::Bool(true))
}

fn handle_cancel_capital_system_transition(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
        if cfg.pending_capital_system_id.is_none() {
            return Err(BridgeRpcError {
                code: "NoCapitalSystemTransition".into(),
                message: "No pending capital system transition to cancel".into(),
            });
        }
        cfg.pending_capital_system_id = None;
        cfg.capital_valid_after_unix = 0;
        write_alliance_sov_cfg(snap, aid, &cfg);
        Ok(())
    })?;
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnUpdateCapitalInfo",
        "allianceid",
        Vec::new(),
    );
    Ok(PyRep::Bool(true))
}

fn handle_get_prime_time_info(
    ctx: &ServerContext,
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let now_unix = ctx.now_unix();
    let cfg = ctx.db.with_snapshot(|snap| read_alliance_sov_cfg(snap, aid, now_unix));
    let valid_after = if cfg.prime_valid_after_unix > 0 {
        super::market_trading::unix_to_filetime(cfg.prime_valid_after_unix)
    } else {
        0
    };
    Ok(PyRep::keyval([
        ("currentPrimeHour", PyRep::Integer(cfg.current_prime_hour)),
        ("newPrimeHour", PyRep::Integer(cfg.new_prime_hour)),
        ("newPrimeHourValidAfter", PyRep::Integer(valid_after)),
    ]))
}

fn handle_set_prime_hour(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let Some(hour) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "SetPrimeHour requires hour".into(),
        });
    };
    if !(0..=23).contains(&hour) {
        return Err(BridgeRpcError {
            code: "InvalidPrimeHour".into(),
            message: format!("Prime hour must be 0..=23, got {hour}"),
        });
    }
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
        cfg.new_prime_hour = hour;
        cfg.prime_valid_after_unix = now_unix + 48 * 3600;
        write_alliance_sov_cfg(snap, aid, &cfg);
        Ok(())
    })?;
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnUpdateCapitalInfo",
        "allianceid",
        Vec::new(),
    );
    Ok(PyRep::Bool(true))
}

fn handle_get_alliance_sov_structures_info(
    ctx: &ServerContext,
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let now_unix = ctx.now_unix();
    let (structures_list, campaigns_list, scores_list) = ctx.db.with_snapshot(|snap| {
        let member_corps: HashSet<i64> = snap
            .alliances
            .get(&aid)
            .map(|a| a.member_corp_ids.clone())
            .unwrap_or_default();
        let mut sov_struct_ids = HashSet::new();
        let mut sov_structs: Vec<&eve_db::UpwellStructureRecord> = snap
            .structures
            .values()
            .filter(|s| {
                let owned_by_alliance = member_corps.contains(&s.owner_corp_id)
                    || snap
                        .system_states
                        .get(&s.solar_system_id)
                        .is_some_and(|st| st.sov_alliance_id == Some(aid));
                let is_sov_type = matches!(s.type_id, 32226 | 32458 | 81242)
                    || snap.sov_runtime.sov_hubs.contains_key(&s.structure_id);
                owned_by_alliance && is_sov_type
            })
            .collect();
        sov_structs.sort_by_key(|s| s.structure_id);

        let mut structures_out = Vec::with_capacity(sov_structs.len());
        for s in sov_structs {
            sov_struct_ids.insert(s.structure_id);
            let (vuln_start, vuln_end) = snap
                .sov_runtime
                .adm_vulnerability_windows
                .get(&s.structure_id)
                .map(|&(st, en)| {
                    (
                        super::market_trading::unix_to_filetime(st),
                        super::market_trading::unix_to_filetime(en),
                    )
                })
                .unwrap_or((0, 0));
            let adm = snap
                .system_states
                .get(&s.solar_system_id)
                .map(|st| {
                    1.0 + f64::from(st.military_index.min(5)) * 0.4
                        + f64::from(st.industrial_index.min(5)) * 0.4
                        + f64::from(st.strategic_index.min(5)) * 0.2
                })
                .unwrap_or(1.0);
            structures_out.push(PyRep::keyval([
                ("itemID", PyRep::Integer(s.structure_id)),
                ("typeID", PyRep::Integer(i64::from(s.type_id))),
                ("solarSystemID", PyRep::Integer(i64::from(s.solar_system_id))),
                ("allianceID", PyRep::Integer(aid)),
                ("vulnerabilityOccupancyLevel", PyRep::Real(adm)),
                ("vulnerableStartTime", PyRep::Integer(vuln_start)),
                ("vulnerableEndTime", PyRep::Integer(vuln_end)),
            ]));
        }

        let mut seen_campaigns = std::collections::BTreeMap::<i64, (u32, i64)>::new();
        for &(hub_id, sys_id, cap_aid) in snap.sov_runtime.command_nodes.values() {
            if sov_struct_ids.contains(&hub_id)
                || snap
                    .system_states
                    .get(&sys_id)
                    .is_some_and(|st| st.sov_alliance_id == Some(aid))
            {
                seen_campaigns.entry(hub_id).or_insert((sys_id, cap_aid));
            }
        }
        let mut campaigns_out = Vec::with_capacity(seen_campaigns.len());
        let mut scores_out = Vec::new();
        for (hub_id, (sys_id, _cap_aid)) in seen_campaigns {
            campaigns_out.push(PyRep::keyval([
                ("campaignID", PyRep::Integer(hub_id)),
                ("sourceItemID", PyRep::Integer(hub_id)),
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("eventType", PyRep::Integer(8)),
                (
                    "startTime",
                    PyRep::Integer(super::market_trading::unix_to_filetime(now_unix)),
                ),
            ]));
        }
        let mut score_hub_ids: Vec<i64> = snap
            .sov_runtime
            .command_node_scores
            .keys()
            .copied()
            .filter(|hub_id| {
                sov_struct_ids.contains(hub_id)
                    || snap
                        .structures
                        .get(hub_id)
                        .is_some_and(|s| member_corps.contains(&s.owner_corp_id))
            })
            .collect();
        score_hub_ids.sort_unstable();
        for hub_id in score_hub_ids {
            if let Some(entries) = snap.sov_runtime.command_node_scores.get(&hub_id) {
                for &(team_id, score) in entries {
                    scores_out.push(PyRep::keyval([
                        ("sourceItemID", PyRep::Integer(hub_id)),
                        ("teamID", PyRep::Integer(team_id)),
                        ("score", PyRep::Real(score)),
                    ]));
                }
            }
        }
        (structures_out, campaigns_out, scores_out)
    });
    Ok(PyRep::Tuple(vec![
        PyRep::List(structures_list),
        PyRep::List(campaigns_list),
        PyRep::List(scores_list),
    ]))
}

fn handle_get_alliance_bill_balance(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let bill_id_opt = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
    ctx.db.with_snapshot(|snap| {
        if let Some(bill_id) = bill_id_opt {
            let Some(note) = snap
                .owner_notes
                .get(&super::wallet_services_ext::bill_note_id(bill_id))
            else {
                return Err(BridgeRpcError {
                    code: "BillNotFound".into(),
                    message: format!("Bill {bill_id} not found"),
                });
            };
            let bill: super::wallet_services_ext::CorpBillRecord =
                serde_json::from_str(&note.note).map_err(|_| BridgeRpcError {
                    code: "BillNotFound".into(),
                    message: "Corrupt bill record".into(),
                })?;
            if bill.debtor_id != aid && bill.creditor_id != aid {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Bill does not belong to caller alliance".into(),
                });
            }
            let bal = if bill.paid {
                0.0
            } else {
                eve_db::IskCents(bill.amount_cents).to_wire_f64()
            };
            Ok(PyRep::Real(bal))
        } else {
            let total_cents: i64 = snap
                .owner_notes
                .values()
                .filter(|n| n.character_id == -71)
                .filter_map(|n| {
                    serde_json::from_str::<super::wallet_services_ext::CorpBillRecord>(&n.note)
                        .ok()
                })
                .filter(|b| !b.paid && b.debtor_id == aid)
                .map(|b| b.amount_cents)
                .sum();
            Ok(PyRep::Real(eve_db::IskCents(total_cents).to_wire_f64()))
        }
    })
}

fn handle_get_alliance_bills(
    ctx: &ServerContext,
    session: &SessionState,
    receivable: bool,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let rows = ctx.db.with_snapshot(|snap| {
        let mut bills: Vec<super::wallet_services_ext::CorpBillRecord> = snap
            .owner_notes
            .values()
            .filter(|n| n.character_id == -71)
            .filter_map(|n| {
                serde_json::from_str::<super::wallet_services_ext::CorpBillRecord>(&n.note).ok()
            })
            .filter(|b| {
                if receivable {
                    b.creditor_id == aid
                } else {
                    !b.paid && b.debtor_id == aid
                }
            })
            .collect();
        bills.sort_by_key(|b| b.bill_id);
        bills
            .iter()
            .map(super::wallet_serverops::bill_to_row)
            .collect::<Vec<_>>()
    });
    Ok(PyRep::rowset(super::wallet_serverops::bill_columns(), rows))
}

fn handle_pay_alliance_bill(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let Some(bill_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "PayBill requires billID".into(),
        });
    };
    let from_account_key = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("fromAccountKey").and_then(|v| v.as_i64()))
        .unwrap_or(1000)
        .clamp(1000, 1006) as u16;
    let div_id = (from_account_key - 999) as u8;
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let nid = super::wallet_services_ext::bill_note_id(bill_id);
        let Some(note) = snap.owner_notes.get(&nid) else {
            return Err(BridgeRpcError {
                code: "BillNotFound".into(),
                message: format!("Bill {bill_id} not found"),
            });
        };
        let mut bill: super::wallet_services_ext::CorpBillRecord =
            serde_json::from_str(&note.note).map_err(|_| BridgeRpcError {
                code: "BillNotFound".into(),
                message: "Corrupt bill record".into(),
            })?;
        if bill.paid {
            return Err(BridgeRpcError {
                code: "BillAlreadyPaid".into(),
                message: "Bill is already paid".into(),
            });
        }
        if bill.debtor_id != aid {
            return Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Bill does not belong to caller alliance".into(),
            });
        }
        super::market_trading::market_orders_corp_auth::require_corp_accountant_spend_role(
            snap,
            corp_id,
            char_id,
            from_account_key,
            now_unix,
            "CorpRoleRequired",
        )?;
        let amt = eve_db::IskCents(bill.amount_cents);
        let corp = snap
            .corporations
            .get_mut(&corp_id)
            .ok_or_else(|| BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: "Executor corporation not found".into(),
            })?;
        let bal_after = corp.debit_division(div_id, amt).ok_or_else(|| BridgeRpcError {
            code: "NotEnoughMoney".into(),
            message: "Insufficient corporation division ISK to pay alliance bill".into(),
        })?;
        snap.append_journal(
            corp_id,
            "BillPayment",
            amt,
            bal_after,
            format!("Paid alliance bill {bill_id}"),
        );
        let cred_corp = snap
            .alliances
            .get(&bill.creditor_id)
            .map(|a| a.executor_corp_id)
            .unwrap_or(bill.creditor_id);
        if let Some(cred) = snap.corporations.get_mut(&cred_corp) {
            cred.credit_division(1, amt);
        }
        bill.paid = true;
        bill.paid_by_division = Some(from_account_key);
        super::wallet_services_ext::save_json_note(
            snap,
            nid,
            -71,
            bill_id,
            format!("bill:{bill_id}"),
            &bill,
            now_unix,
        );
        Ok(())
    })?;
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnCorpWalletBillsChanged",
        "allianceid",
        Vec::new(),
    );
    Ok(PyRep::Bool(true))
}

fn handle_gm_force_accept_application(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "GMForceAcceptApplication requires GM privileges".into(),
        });
    }
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let Some(applicant_corp_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "GMForceAcceptApplication requires corporationID".into(),
        });
    };
    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        if !snap.alliances.contains_key(&aid) {
            return Err(BridgeRpcError {
                code: "AllianceNotFound".into(),
                message: format!("Alliance {aid} not found"),
            });
        }
        if !snap.corporations.contains_key(&applicant_corp_id) {
            return Err(BridgeRpcError {
                code: "CorpNotFound".into(),
                message: format!("Corporation {applicant_corp_id} not found"),
            });
        }
        let meta = snap.alliance_war_state.alliances.entry(aid).or_default();
        let Some(app) = meta
            .applications
            .iter_mut()
            .find(|a| a.0 == applicant_corp_id && a.3 == "Pending")
        else {
            return Err(BridgeRpcError {
                code: "ApplicationNotFound".into(),
                message: format!("No pending application from corporation {applicant_corp_id}"),
            });
        };
        app.3 = "Accepted".into();
        app.4 = Some(now_unix);
        if !meta
            .employment_history
            .iter()
            .any(|e| e.0 == applicant_corp_id && e.2.is_none())
        {
            meta.employment_history.push((applicant_corp_id, now_unix, None));
        }
        if let Some(al) = snap.alliances.get_mut(&aid) {
            al.member_corp_ids.insert(applicant_corp_id);
        }
        let mut members = Vec::new();
        if let Some(corp) = snap.corporations.get_mut(&applicant_corp_id) {
            corp.alliance_id = Some(aid);
            members.extend(corp.members.iter().copied());
        }
        for mid in members {
            if let Some(ch) = snap.characters.get_mut(&mid) {
                ch.alliance_id = Some(aid);
            }
        }
        Ok(())
    })?;
    let app_change = PyRep::Dict(vec![(
        PyRep::String("state".into()),
        PyRep::Tuple(vec![
            PyRep::String("Pending".into()),
            PyRep::String("Accepted".into()),
        ]),
    )]);
    let app_args = vec![
        PyRep::Integer(aid),
        PyRep::Integer(applicant_corp_id),
        app_change,
    ];
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnAllianceApplicationChanged",
        "allianceid",
        app_args.clone(),
    );
    ctx.notify(
        NotificationTarget::Corporation(applicant_corp_id),
        "OnAllianceApplicationChanged",
        "corpid",
        app_args,
    );
    let member_change = PyRep::Dict(vec![(
        PyRep::String("corporationID".into()),
        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(applicant_corp_id)]),
    )]);
    ctx.notify(
        NotificationTarget::Alliance(aid),
        "OnAllianceMemberChanged",
        "allianceid",
        vec![
            PyRep::Integer(aid),
            PyRep::Integer(applicant_corp_id),
            member_change,
        ],
    );
    Ok(PyRep::Bool(true))
}

fn handle_update_alliance_bulletin_order(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let order_pairs: Vec<(i64, i64)> = match args.first() {
        Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| match item {
                PyRep::List(p) | PyRep::Tuple(p) => Some((
                    p.first()?.as_i64()?,
                    p.get(1).and_then(|x| x.as_i64()).unwrap_or(idx as i64),
                )),
                PyRep::Integer(bid) => Some((*bid, idx as i64)),
                _ => None,
            })
            .collect(),
        _ => args
            .iter()
            .enumerate()
            .filter_map(|(idx, x)| x.as_i64().map(|bid| (bid, idx as i64)))
            .collect(),
    };
    if order_pairs.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "UpdateBulletinOrder requires a non-empty order list".into(),
        });
    }
    with_alliance_war_state(ctx, |st| {
        let meta = st.alliances.entry(aid).or_default();
        meta.bulletins.sort_by_key(|b| {
            order_pairs
                .iter()
                .find(|&&(id, _)| id == b.0)
                .map(|&(_, ord)| ord)
                .unwrap_or(i64::MAX)
        });
    });
    Ok(PyRep::Bool(true))
}

#[cfg(test)]
#[path = "social_corp_fleet_tests.rs"]
mod social_corp_fleet_tests;

#[cfg(test)]
#[path = "social_corp_fleet_edges_tests.rs"]
mod social_corp_fleet_edges_tests;
#[cfg(test)]
#[path = "social_corp_fleet_edges_b4_tests.rs"]
mod social_corp_fleet_edges_b4_tests;
