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
mod social_wars;

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

fn sync_alliance_contacts_esi(ctx: &ServerContext, aid: i64) {
    let (contacts, labels) = ctx.db.with_snapshot(|snap| {
        (
            snap.alliance_contacts.get(&aid).cloned().unwrap_or_default(),
            snap.alliance_contact_labels
                .get(&aid)
                .cloned()
                .unwrap_or_default(),
        )
    });
    if let Ok(mut esi_c) = ctx.esi_state.alliance_contacts.write() {
        esi_c.insert(
            aid,
            contacts
                .into_iter()
                .map(|c| crate::esi::runtime_state::EsiContactEntry {
                    contact_id: c.contact_id,
                    contact_type: c.contact_type,
                    standing: c.standing,
                    is_watched: c.is_watched,
                    is_blocked: c.is_blocked,
                    label_ids: c.label_ids,
                })
                .collect(),
        );
    }
    if let Ok(mut esi_l) = ctx.esi_state.alliance_contact_labels.write() {
        esi_l.insert(aid, labels);
    }
}
fn require_corp_director(ctx: &ServerContext, session: &SessionState) -> Result<i64, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let corp_id = session.corporation_id.unwrap_or(0);
    let ok = ctx.db.with_snapshot(|snap| {
        if let Some(c) = snap.corporations.get(&corp_id) {
            c.ceo_id == char_id
                || corp_roles::has_role(c.member_roles.get(&char_id).copied().unwrap_or(0), corp_roles::ROLE_DIRECTOR)
        } else {
            corp_roles::has_role(session.corp_role, corp_roles::ROLE_DIRECTOR)
        }
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
    let aids: Vec<i64> = read_alliance_war_state(ctx, |st| st.alliances.keys().copied().collect());
    for aid in aids {
        let _ = sync_alliance_timers(ctx, aid);
    }
}

fn sync_alliance_timers(ctx: &ServerContext, aid: i64) -> bool {
    let now_unix = ctx.now_unix();
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
                corp.ensure_wallet_divisions();
                if corp.wallet_divisions[0].balance < alliance_fee {
                    return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: "Creating an alliance requires 1,000,000,000 ISK in the corporation master wallet".into() });
                }
                corp.wallet_divisions[0].balance = corp.wallet_divisions[0].balance.checked_sub(alliance_fee).unwrap();
                corp.master_wallet = corp.wallet_divisions[0].balance;
                let new_bal = corp.wallet_divisions[0].balance.to_f64_isk();
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
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let applicant_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let eff_unix = now_unix + 86_400;
            let found = with_alliance_war_state(ctx, |st| {
                let meta = st.alliances.entry(aid).or_default();
                if let Some(app) = meta.applications.iter_mut().find(|a| a.0 == applicant_corp_id && a.3 == "Pending") {
                    app.3 = "Accepted".into();
                    app.4 = Some(eff_unix);
                    true
                } else {
                    false
                }
            });
            if !found {
                return Some(Err(BridgeRpcError { code: "ApplicationNotFound".into(), message: format!("No pending application from corporation {applicant_corp_id}") }));
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
            Some(Ok(PyRep::rowset(
                vec!["allianceID", "corporationID", "applicationText", "applicationDateTime", "status", "joinEffectiveTime"],
                rows,
            )))
        }

        ("corpRegistry", "DeleteAllianceApplication") => {
            let corp_id = match require_corp_director(ctx, session) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            with_alliance_war_state(ctx, |st| {
                if let Some(m) = st.alliances.get_mut(&aid) {
                    m.applications.retain(|a| a.0 != corp_id);
                }
            });
            Some(Ok(PyRep::keyval([("deleted", PyRep::Bool(true))])))
        }

        ("allianceRegistry", "Bind") | ("allianceRegistry", "SetSessionCheck") => {
            Some(Ok(PyRep::None))
        }

        ("allianceRegistry", "allianceID" | "shortName" | "description" | "url" | "executorCorpID" | "creatorCorpID" | "creatorCharID" | "dictatorial" | "allowWar") => {
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
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
            let (desc, url, _, _) = read_alliance_war_state(ctx, |st| {
                st.alliances.get(&aid).map(|m| (m.description.clone(), m.url.clone(), m.logo, m.emblem_entitlements)).unwrap_or_default()
            });
            let rep = match method {
                "allianceID" => PyRep::Integer(aid_val),
                "shortName" => PyRep::String(short_name),
                "description" => PyRep::String(desc),
                "url" => PyRep::String(url),
                "executorCorpID" => PyRep::Integer(exec_id),
                "creatorCorpID" => PyRep::Integer(creator_corp_id),
                "creatorCharID" => PyRep::Integer(creator_char_id),
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
            let (desc, url, logo, emblem) = read_alliance_war_state(ctx, |st| {
                st.alliances.get(&aid).map(|m| (m.description.clone(), m.url.clone(), m.logo, m.emblem_entitlements)).unwrap_or_default()
            });
            Some(Ok(PyRep::keyval([
                ("allianceID", PyRep::Integer(id)), ("allianceName", PyRep::String(name)), ("shortName", PyRep::String(short)),
                ("executorCorpID", PyRep::Integer(exec)), ("creatorCorpID", PyRep::Integer(creator_corp)), ("creatorCharID", PyRep::Integer(creator_char)),
                ("startDate", PyRep::Integer(start_date)), ("memberCount", PyRep::Integer(cnt)), ("description", PyRep::String(desc)),
                ("url", PyRep::String(url)), ("shape1", PyRep::Integer(logo.0)), ("shape2", PyRep::Integer(logo.1)),
                ("shape3", PyRep::Integer(logo.2)), ("color", PyRep::Integer(logo.3)), ("emblemEntitlements", PyRep::Bool(emblem)),
                ("dictatorial", PyRep::Bool(false)), ("allowWar", PyRep::Bool(true)),
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
            let rows: Vec<Vec<PyRep>> = read_alliance_war_state(ctx, |st| {
                st.alliances.get(&aid).map(|m| {
                    m.employment_history.iter().map(|(cid, start, end)| {
                        vec![PyRep::Integer(aid), PyRep::Integer(*cid), PyRep::Integer(*start as i64), end.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None)]
                    }).collect()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["allianceID", "corporationID", "startDate", "endDate"], rows)))
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
            let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(standing) = args.get(1).and_then(|a| a.as_f64()).filter(|s| (-10.0..=10.0).contains(s)) else {
                return Some(Err(BridgeRpcError { code: "InvalidStanding".into(), message: "Standing must be between -10 and +10".into() }));
            };
            let label_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            let prev = ctx.db.with_mut_snapshot(|snap| {
                let list = snap.alliance_contacts.entry(aid).or_default();
                let prev = list
                    .iter()
                    .find(|c| c.contact_id == contact_id)
                    .map(|c| (c.standing, label_mask_from_ids(&c.label_ids)));
                list.retain(|c| c.contact_id != contact_id);
                list.push(eve_db::SocialContactRecord {
                    contact_id,
                    contact_type: "character".into(),
                    standing,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: label_ids_from_mask(label_mask),
                });
                list.sort_by_key(|c| c.contact_id);
                prev
            });
            sync_alliance_contacts_esi(ctx, aid);
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
            sync_alliance_contacts_esi(ctx, aid);
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
            sync_alliance_contacts_esi(ctx, aid);
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
            let (lid, contact_ids): (i64, Vec<i64>) = match args.first() {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
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
            sync_alliance_contacts_esi(ctx, aid);
            Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(lid))])))
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
            let bid_res = with_alliance_war_state(ctx, |st| {
                let meta = st.alliances.entry(aid).or_default();
                match target {
                    Some(id) => match meta.bulletins.iter_mut().find(|b| b.0 == id) {
                        Some(b) => {
                            *b = (id, title.clone(), body.clone(), char_id, now_unix);
                            Ok(id)
                        }
                        None => Err(BridgeRpcError { code: "BulletinNotFound".into(), message: format!("Bulletin {id} not found") }),
                    },
                    None => {
                        let id = meta.bulletins.iter().map(|b| b.0).max().unwrap_or(0) + 1;
                        meta.bulletins.push((id, title.clone(), body.clone(), char_id, now_unix));
                        Ok(id)
                    }
                }
            });
            let bid = match bid_res {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([("bulletinID", PyRep::Integer(bid)), ("title", PyRep::String(title)), ("body", PyRep::String(body))])))
        }

        ("allianceRegistry", "GetBulletins") | ("allianceRegistry", "GetAllianceBulletins") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = session.alliance_id.unwrap_or(0);
            let rows: Vec<Vec<PyRep>> = read_alliance_war_state(ctx, |st| {
                st.alliances.get(&aid).map(|m| {
                    m.bulletins.iter().map(|(bid, title, body, author, ts)| {
                        vec![PyRep::Integer(*bid), PyRep::String(title.clone()), PyRep::String(body.clone()), PyRep::Integer(*author), PyRep::Integer(*ts as i64)]
                    }).collect()
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["bulletinID", "title", "body", "editCharacterID", "editDateTime"], rows)))
        }

        ("allianceRegistry", "DeleteBulletin") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let bid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let removed = with_alliance_war_state(ctx, |st| {
                st.alliances.get_mut(&aid).map(|m| {
                    let before = m.bulletins.len();
                    m.bulletins.retain(|b| b.0 != bid);
                    before != m.bulletins.len()
                }).unwrap_or(false)
            });
            Some(Ok(PyRep::Bool(removed)))
        }

        ("allianceRegistry", "UpdateAlliance") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let aid = match require_executor(ctx, session) { Ok(a) => a, Err(e) => return Some(Err(e)) };
            let description = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            let url = args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let (old_desc, old_url) = with_alliance_war_state(ctx, |st| {
                let meta = st.alliances.entry(aid).or_default();
                (
                    std::mem::replace(&mut meta.description, description.clone()),
                    std::mem::replace(&mut meta.url, url.clone()),
                )
            });
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
                    let Some(_) = snap.items.get(&iid) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {iid} not found") });
                    };
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
                let caller_roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                let is_ceo = corp.ceo_id == char_id;
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
                    if Some(iid) == session.ship_id || Some(iid) == active_ship_id {
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
                for &iid in &item_ids {
                    if let Some(item) = snap.items.get_mut(&iid) {
                        if method == "DeliverToCorpHangar" {
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
                Ok(last_res)
            });
            match res {
                Ok((new_owner, flag_id)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(first_item_id)),
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

        _ => social_corp_projects::handle_corp_projects_rpc(ctx, session, service, method, args)
            .or_else(|| social_wars::handle_wars(ctx, session, service, method, args, bound_obj_id)),
    }
}

