//! Corporation Core Services (`SO-3`): `corpRegistry`, `corpmgr`, `corporationSvc`, `corpStationMgr`, `account`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
pub use eve_db::CorpGovState;
use eve_db::{corp_roles, CorpApplicationRecord, IskCents, MailRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};
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

pub(crate) fn promote_due_pending_corp_grants(ctx: &ServerContext, corp_id: i64) {
    if corp_id <= 0 { return; }
    let now_unix = ctx.now_unix();
    let has_due = ctx.db.with_snapshot(|snap| {
        snap.corp_governance.get(&corp_id).is_some_and(|g| {
            g.pending_role_grants.values().any(|&(_, eff)| now_unix >= eff)
                || g.pending_title_role_grants.values().any(|&(_, eff)| now_unix >= eff)
                || g.pending_scoped_role_grants.values().any(|&(_, _, _, eff)| now_unix >= eff)
                || g.pending_friendly_fire.is_some_and(|(_, eff)| now_unix >= eff)
        })
    });
    if !has_due { return; }
    let dec = |m: u64| -> Vec<String> { crate::esi::corp_governance_routes::decode_corp_roles(m).into_iter().map(String::from).collect() };
    let matured_changes: Vec<(i64, u64, u64, u64, u64, u64, u64, u64, u64)> = ctx.db.with_mut_snapshot(|snap| {
        let (mut promoted, mut promoted_scoped) = (Vec::<(i64, u64)>::new(), Vec::<(i64, u64, u64, u64)>::new());
        if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
            g.pending_role_grants.retain(|&mid, &mut (p_mask, eff)| { if now_unix >= eff { promoted.push((mid, p_mask)); false } else { true } });
            g.pending_title_role_grants.retain(|&mid, &mut (t_mask, eff)| { if now_unix >= eff { promoted.push((mid, t_mask)); false } else { true } });
            g.pending_scoped_role_grants.retain(|&mid, &mut (hq, base, other, eff)| { if now_unix >= eff { promoted_scoped.push((mid, hq, base, other)); false } else { true } });
            if let Some((ff_val, _)) = g.pending_friendly_fire.filter(|&(_, eff)| now_unix >= eff) {
                g.friendly_fire_legal = ff_val;
                g.pending_friendly_fire = None;
            }
        }
        let mut touched: HashMap<i64, (u64, u64, u64, u64)> = HashMap::new();
        for mid in promoted.iter().map(|&(m, _)| m).chain(promoted_scoped.iter().map(|&(m, _, _, _)| m)) {
            touched.entry(mid).or_insert_with(|| {
                let r0 = snap.corporations.get(&corp_id).and_then(|c| c.member_roles.get(&mid).copied()).unwrap_or(0);
                let ra0 = snap.corp_role_assignments.get(&(corp_id, mid)).cloned().unwrap_or_default();
                (r0, ra0.roles_at_hq, ra0.roles_at_base, ra0.roles_at_other)
            });
        }
        for (mid, p_mask) in promoted {
            let new_eff = snap.corporations.get_mut(&corp_id).map_or(p_mask, |corp| {
                let r = corp.member_roles.entry(mid).or_insert(0);
                *r |= p_mask;
                *r
            });
            let ra = snap.corp_role_assignments.entry((corp_id, mid)).or_insert_with(|| {
                eve_db::snapshot_ext::CorpMemberRoleAssignment { character_id: mid, ..Default::default() }
            });
            ra.roles |= new_eff;
            snap.char_scoped_roles.entry(mid).or_default().roles = dec(ra.roles);
        }
        for (mid, hq, base, other) in promoted_scoped {
            let ra = snap.corp_role_assignments.entry((corp_id, mid)).or_insert_with(|| {
                eve_db::snapshot_ext::CorpMemberRoleAssignment { character_id: mid, ..Default::default() }
            });
            ra.roles_at_hq |= hq;
            ra.roles_at_base |= base;
            ra.roles_at_other |= other;
            let sc = snap.char_scoped_roles.entry(mid).or_default();
            sc.roles_at_hq = dec(ra.roles_at_hq);
            sc.roles_at_base = dec(ra.roles_at_base);
            sc.roles_at_other = dec(ra.roles_at_other);
        }
        touched.into_iter().map(|(mid, (r0, hq0, base0, other0))| {
            let r1 = snap.corporations.get(&corp_id).and_then(|c| c.member_roles.get(&mid).copied()).unwrap_or(r0);
            let ra1 = snap.corp_role_assignments.get(&(corp_id, mid)).cloned().unwrap_or_default();
            (mid, r0, r1, hq0, ra1.roles_at_hq, base0, ra1.roles_at_base, other0, ra1.roles_at_other)
        }).collect()
    });
    if !matured_changes.is_empty() {
        let corp_ally = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).and_then(|c| c.alliance_id));
        for (mid, r0, r1, hq0, hq1, base0, base1, other0, other1) in matured_changes {
            if r0 != r1 {
                super::fleet_social_persistence::with_social_state(ctx, |s| { s.forced_session_corp_sync.insert(mid, (corp_id, corp_ally, r1)); });
            }
            super::fleet_social_corp_tail::emit_corp_role_session_changed(ctx, corp_id, mid, r0, r1, hq0, hq1, base0, base1, other0, other1);
        }
    }
}

/// Promotes all matured 24-hour pending corporation role grants and friendly-fire
/// toggles across every corporation (`#736`).
pub(crate) fn promote_all_due_corp_grants(ctx: &ServerContext) {
    let now_unix = ctx.now_unix();
    let due_corps: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.corp_governance
            .iter()
            .filter(|(_, g)| {
                g.pending_role_grants.values().any(|&(_, eff)| now_unix >= eff)
                    || g.pending_title_role_grants.values().any(|&(_, eff)| now_unix >= eff)
                    || g.pending_scoped_role_grants
                        .values()
                        .any(|&(_, _, _, eff)| now_unix >= eff)
                    || g.pending_friendly_fire.is_some_and(|(_, eff)| now_unix >= eff)
            })
            .map(|(&cid, _)| cid)
            .collect()
    });
    for corp_id in due_corps {
        promote_due_pending_corp_grants(ctx, corp_id);
    }
}

pub(crate) fn get_character_medals_and_decorations(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> (Vec<PyRep>, Vec<PyRep>) {
    let (mut medals, mut decorations) = (Vec::new(), Vec::new());
    for (&corp_id, state) in &snap.corp_governance {
        for (mid, rid, iid, reason, status) in &state.medal_recipients {
            if *rid == char_id {
                let title = state.medals.iter().find(|m| m.0 == *mid).map_or_else(|| "Medal".into(), |m| m.1.clone());
                let kv = PyRep::keyval([
                    ("medalID", PyRep::Integer(*mid)), ("wearerID", PyRep::Integer(*rid)), ("issuerID", PyRep::Integer(*iid)),
                    ("corporationID", PyRep::Integer(corp_id)), ("title", PyRep::String(title)), ("reason", PyRep::String(reason.clone())), ("status", PyRep::Integer(*status)),
                ]);
                decorations.push(kv.clone());
                if *status > 1 { medals.push(kv); }
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
    if let Some(lim) = snap.corp_governance.get(&corp_id).and_then(|g| g.member_limit) {
        return lim;
    }
    let bonus: usize = snap.skills.get(&ceo_id).map_or(20, |skills| {
        skills.values().map(|s| {
            let per_lvl = ctx.sde.types.get(&s.type_id).and_then(|t| t.default_attributes.get(&191).copied()).unwrap_or(0.0);
            (per_lvl * s.level as f64) as usize
        }).sum()
    });
    bonus.max(1)
}

pub(crate) fn record_corp_join_in_snap_pub(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    old_corp_id: i64,
    corp_id: i64,
    now_unix: u64,
) -> Vec<(i64, i64, u32, i64)> {
    record_corp_join_in_snap(snap, char_id, old_corp_id, corp_id, now_unix)
}

pub(super) fn record_corp_join_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    old_corp_id: i64,
    corp_id: i64,
    now_unix: u64,
) -> Vec<(i64, i64, u32, i64)> {
    let mut released = Vec::new();
    if old_corp_id > 0 && old_corp_id != corp_id {
        if let Some(g) = snap.corp_governance.get_mut(&old_corp_id) {
            g.pending_role_grants.remove(&char_id);
            g.pending_title_role_grants.remove(&char_id);
            g.pending_scoped_role_grants.remove(&char_id);
            g.pending_auto_kicks.remove(&char_id);
            g.member_titles.remove(&char_id);
            g.member_divisions.remove(&char_id);
            g.member_bases.remove(&char_id);
            g.member_account_keys.remove(&char_id);
            g.role_drop_timestamps.remove(&char_id);
        }
        snap.corp_role_assignments.remove(&(old_corp_id, char_id));
        snap.char_scoped_roles.remove(&char_id);
        snap.corp_member_titles.remove(&(old_corp_id, char_id));
        released = crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(snap);
    }
    let list = snap.employment_history.entry(char_id).or_default();
    if list.is_empty() {
        let cdate = snap.characters.get(&char_id).map(|c| c.create_date_unix).unwrap_or(0);
        if old_corp_id > 0 {
            list.push((old_corp_id, cdate, Some(now_unix)));
        }
    } else if let Some(last) = list.last_mut() {
        if last.2.is_none() {
            last.2 = Some(now_unix);
        }
    }
    list.push((corp_id, now_unix, None));
    released
}

const MEMBER_COLS: [&str; 24] = [
    "characterID", "name", "role", "title", "corporationID", "locationID", "shipTypeID", "logonDateTime",
    "roles", "grantableRoles", "rolesAtHQ", "grantableRolesAtHQ", "rolesAtBase", "grantableRolesAtBase",
    "rolesAtOther", "grantableRolesAtOther", "titleMask", "baseID", "startDateTime", "accountKey",
    "divisionID", "squadronID", "blockRoles", "blocksRoles",
];

struct CorpMemberRowSnapshot {
    character_id: i64,
    name: String,
    role_mask: u64,
    title: String,
    corporation_id: i64,
    location_id: i64,
    ship_type_id: i64,
    logon_date_time: i64,
    grantable_roles: u64,
    roles_at_hq: u64,
    grantable_roles_at_hq: u64,
    roles_at_base: u64,
    grantable_roles_at_base: u64,
    roles_at_other: u64,
    grantable_roles_at_other: u64,
    title_mask: u64,
    base_id: Option<i64>,
    start_date_time: i64,
    account_key: i64,
    division_id: i64,
    squadron_id: i64,
}

pub(crate) fn require_corp_caller(
    ctx: &ServerContext,
    session: &mut SessionState,
    target_corp_arg: Option<i64>,
) -> Result<(i64, i64, bool), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.corporation_id.is_none() || session.corporation_id == Some(0) {
        if let Some(cid) = ctx.db.with_snapshot(|s| {
            s.characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .filter(|id| s.corporations.contains_key(id) || ctx.sde.npc_corporations.contains_key(id))
        }) {
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
    promote_due_pending_corp_grants(ctx, caller_corp);
    let now_unix = ctx.now_unix();
    let has_director = ctx.db.with_snapshot(|s| {
        if !s.corporations.contains_key(&caller_corp) {
            return false;
        }
        let (is_ceo, roles) = s.effective_corp_roles_at(caller_corp, char_id, None, 0, now_unix);
        is_ceo || (roles & (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER) != 0)
    });
    Ok((caller_corp, char_id, has_director))
}

fn get_corp_member_snapshot(
    ctx: &ServerContext,
    corp_id: i64,
    viewer_id: i64,
    has_director: bool,
) -> Vec<CorpMemberRowSnapshot> {
    promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let gov_opt = snap.corp_governance.get(&corp_id);
        let Some(corp) = snap.corporations.get(&corp_id) else { return Vec::new(); };
        let mut mids: Vec<i64> = corp.members.iter().copied().collect();
        mids.sort_unstable();
        mids.into_iter().filter_map(|mid| {
            snap.characters.get(&mid).map(|ch| {
                let (_, role_mask) = snap.effective_corp_roles_at(corp_id, mid, None, 0, now_unix);
                let ra = snap.corp_role_assignments.get(&(corp_id, mid)).cloned().unwrap_or_default();
                let tmask = gov_opt.and_then(|g| g.member_titles.get(&mid).copied()).unwrap_or(0);
                let title = gov_opt
                    .and_then(|g| {
                        let mut names = Vec::new();
                        for idx in 1..=16i64 {
                            if (tmask & (1u64 << (idx - 1))) != 0 || tmask == idx as u64 {
                                if let Some((n, _)) = g.titles.get(&idx) {
                                    names.push(n.clone());
                                }
                            }
                        }
                        (!names.is_empty()).then(|| names.join(", "))
                    })
                    .unwrap_or_else(|| if mid == corp.ceo_id { "CEO".into() } else { String::new() });
                let (loc_id, ship_id, logon_ts) = if has_director || mid == viewer_id {
                    let s_id = snap.items.get(&ch.active_ship_id).map(|i| i.type_id as i64).unwrap_or(606);
                    let l_ts = snap
                        .character_login_stats
                        .get(&mid)
                        .map(|s| s.last_login_unix)
                        .filter(|&t| t > 0)
                        .unwrap_or(now_unix);
                    (ch.solar_system_id as i64, s_id, crate::sp6_lifecycle::unix_to_filetime(l_ts))
                } else {
                    (0, 0, 0)
                };
                let start_dt = snap
                    .employment_history
                    .get(&mid)
                    .and_then(|h| h.iter().rev().find(|e| e.0 == corp_id).map(|e| e.1))
                    .unwrap_or(ch.create_date_unix);
                let base_id = gov_opt
                    .and_then(|g| g.member_bases.get(&mid).copied())
                    .or(corp.hq_station_id)
                    .or(Some(ch.home_station_id));
                let acct_key = gov_opt.and_then(|g| g.member_account_keys.get(&mid).copied()).unwrap_or(1000);
                let (div_id, sq_id) = gov_opt.and_then(|g| g.member_divisions.get(&mid).copied()).unwrap_or((0, 0));
                CorpMemberRowSnapshot {
                    character_id: ch.character_id,
                    name: ch.name.clone(),
                    role_mask,
                    title,
                    corporation_id: corp_id,
                    location_id: loc_id,
                    ship_type_id: ship_id,
                    logon_date_time: logon_ts,
                    grantable_roles: ra.grantable_roles,
                    roles_at_hq: ra.roles_at_hq,
                    grantable_roles_at_hq: ra.grantable_roles_at_hq,
                    roles_at_base: ra.roles_at_base,
                    grantable_roles_at_base: ra.grantable_roles_at_base,
                    roles_at_other: ra.roles_at_other,
                    grantable_roles_at_other: ra.grantable_roles_at_other,
                    title_mask: tmask,
                    base_id,
                    start_date_time: crate::sp6_lifecycle::unix_to_filetime(start_dt),
                    account_key: acct_key,
                    division_id: div_id,
                    squadron_id: sq_id,
                }
            })
        }).collect()
    })
}

fn member_row_rep(m: &CorpMemberRowSnapshot) -> Vec<PyRep> {
    vec![
        PyRep::Integer(m.character_id), PyRep::String(m.name.clone()), PyRep::Integer(m.role_mask as i64),
        PyRep::String(m.title.clone()), PyRep::Integer(m.corporation_id), PyRep::Integer(m.location_id),
        PyRep::Integer(m.ship_type_id), PyRep::Integer(m.logon_date_time),
        PyRep::Integer(m.role_mask as i64), PyRep::Integer(m.grantable_roles as i64),
        PyRep::Integer(m.roles_at_hq as i64), PyRep::Integer(m.grantable_roles_at_hq as i64),
        PyRep::Integer(m.roles_at_base as i64), PyRep::Integer(m.grantable_roles_at_base as i64),
        PyRep::Integer(m.roles_at_other as i64), PyRep::Integer(m.grantable_roles_at_other as i64),
        PyRep::Integer(m.title_mask as i64), m.base_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        PyRep::Integer(m.start_date_time), PyRep::Integer(m.account_key),
        PyRep::Integer(m.division_id), PyRep::Integer(m.squadron_id),
        PyRep::Integer(0), PyRep::Integer(0),
    ]
}

fn handle_get_members_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id));
    let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
    let rows: Vec<Vec<PyRep>> = members.iter().map(member_row_rep).collect();
    Some(Ok(PyRep::rowset(MEMBER_COLS.to_vec(), rows)))
}

fn handle_get_members_paged(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (target_cid, page, page_size) = if let Some(cid) = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id)) {
        (Some(cid), args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as usize, args.get(2).and_then(|a| a.as_i64()).unwrap_or(50).max(1) as usize)
    } else {
        (None, args.first().and_then(|a| a.as_i64()).unwrap_or(1).max(1) as usize, args.get(1).and_then(|a| a.as_i64()).unwrap_or(50).max(1) as usize)
    };
    let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
    let total_count = members.len();
    let offset = (page.saturating_sub(1)) * page_size;
    let rows: Vec<Vec<PyRep>> = members.iter().skip(offset).take(page_size).map(member_row_rep).collect();
    let crowset = DBRowDescriptor::new([
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
    ])
    .build_crowset(rows);
    Some(Ok(PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("eve.common.script.util.pagedCollection.PagedResultSet".into()),
            PyRep::Tuple(vec![
                crowset,
                PyRep::Integer(total_count as i64),
                PyRep::Integer(page.saturating_sub(1) as i64),
                PyRep::Integer(page_size as i64),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    }))
}

fn handle_get_members_by_ids(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id));
    let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let target_ids: HashSet<i64> = match args.get(1).or_else(|| args.first()) {
        Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => HashSet::new(),
    };
    let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
    let rows: Vec<Vec<PyRep>> = members.iter().filter(|m| target_ids.contains(&m.character_id)).map(member_row_rep).collect();
    Some(Ok(PyRep::rowset(MEMBER_COLS.to_vec(), rows)))
}

fn eval_member_query_clause(
    ctx: &ServerContext,
    corp_id: i64,
    member: &CorpMemberRowSnapshot,
    prop: &str,
    op: &PyRep,
    val: &PyRep,
) -> bool {
    let char_id = member.character_id;
    let effective_roles = member.role_mask as i64;
    let field_val = ctx.db.with_snapshot(|snap| {
        let ra = snap.corp_role_assignments.get(&(corp_id, char_id));
        let gov = snap.corp_governance.get(&corp_id);
        let ch = snap.characters.get(&char_id);
        match prop {
            "roles" | "corpRole" => effective_roles,
            "rolesAtHQ" => ra.map_or(0, |r| r.roles_at_hq as i64),
            "rolesAtBase" => ra.map_or(0, |r| r.roles_at_base as i64),
            "rolesAtOther" => ra.map_or(0, |r| r.roles_at_other as i64),
            "grantableRoles" => ra.map_or(0, |r| r.grantable_roles as i64),
            "grantableRolesAtHQ" => ra.map_or(0, |r| r.grantable_roles_at_hq as i64),
            "grantableRolesAtBase" => ra.map_or(0, |r| r.grantable_roles_at_base as i64),
            "grantableRolesAtOther" => ra.map_or(0, |r| r.grantable_roles_at_other as i64),
            "titleMask" => gov.and_then(|g| g.member_titles.get(&char_id).copied()).unwrap_or(0) as i64,
            "baseID" => gov.and_then(|g| g.member_bases.get(&char_id).copied()).unwrap_or(0),
            "characterID" | "charID" => char_id,
            "startDateTime" | "startDate" => {
                snap.employment_history
                    .get(&char_id)
                    .and_then(|h| h.iter().rfind(|(cid, _, _)| *cid == corp_id))
                    .map_or_else(|| ch.map_or(0, |c| c.create_date_unix as i64), |&(_, start, _)| start as i64)
            }
            _ => 0,
        }
    });
    if matches!(prop, "name" | "characterName") {
        let needle = val.as_str().unwrap_or("").to_ascii_lowercase();
        let hay = member.name.to_ascii_lowercase();
        return match op.as_i64().or_else(|| op.as_str().map(|_| 0)) {
            Some(6) => hay != needle,
            _ if op.as_str() == Some("!=") => hay != needle,
            _ if op.as_str() == Some("==") || op.as_str() == Some("=") => hay == needle,
            _ => hay.contains(&needle),
        };
    }
    let rhs = val.as_i64().unwrap_or(0);
    if let Some(code) = op.as_i64() {
        match code {
            1 => field_val == rhs,
            2 => field_val > rhs,
            3 => field_val >= rhs,
            4 => field_val < rhs,
            5 => field_val <= rhs,
            6 => field_val != rhs,
            7 => (field_val & rhs) == rhs,
            8 => (field_val & rhs) == 0,
            _ => field_val == rhs,
        }
    } else if let Some(s) = op.as_str() {
        match s {
            "=" | "==" => field_val == rhs,
            "!=" | "<>" => field_val != rhs,
            ">" => field_val > rhs,
            ">=" => field_val >= rhs,
            "<" => field_val < rhs,
            "<=" => field_val <= rhs,
            "&" => (field_val & rhs) == rhs,
            "!&" => (field_val & rhs) == 0,
            _ => field_val == rhs,
        }
    } else {
        field_val == rhs
    }
}

fn handle_get_member_ids_by_query(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id));
    let (corp_id, viewer_id, has_director) = match require_corp_caller(ctx, session, target_cid) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let query_arg = if target_cid.is_some() { args.get(1) } else { args.first() }
        .or_else(|| kwargs.get("query"));
    let members = get_corp_member_snapshot(ctx, corp_id, viewer_id, has_director);
    let ids: Vec<PyRep> = match query_arg {
        Some(PyRep::List(clauses)) | Some(PyRep::Tuple(clauses)) if !clauses.is_empty() => {
            members.into_iter().filter(|m| {
                let mut acc = true;
                let mut initialized = false;
                for clause in clauses {
                    let items = match clause {
                        PyRep::List(v) | PyRep::Tuple(v) => v,
                        _ => continue,
                    };
                    if items.len() == 3 {
                        let prop = items[0].as_str().unwrap_or("");
                        acc = eval_member_query_clause(ctx, corp_id, m, prop, &items[1], &items[2]);
                        initialized = true;
                    } else if items.len() >= 4 {
                        let join_op = items[0].as_i64().unwrap_or(1);
                        let prop = items[1].as_str().unwrap_or("");
                        let res = eval_member_query_clause(ctx, corp_id, m, prop, &items[2], &items[3]);
                        if !initialized {
                            acc = res;
                            initialized = true;
                        } else if join_op == 2 || items[0].as_str().is_some_and(|s| s.eq_ignore_ascii_case("OR")) {
                            acc = acc || res;
                        } else {
                            acc = acc && res;
                        }
                    }
                }
                acc
            }).map(|m| PyRep::Integer(m.character_id)).collect()
        }
        _ => {
            let query = args.iter().find_map(|a| match a { PyRep::String(s) => Some(s.to_lowercase()), _ => None })
                .or_else(|| kwargs.get("query").and_then(|v| match v { PyRep::String(s) => Some(s.to_lowercase()), _ => None }))
                .unwrap_or_default();
            members.into_iter()
                .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query) || m.character_id.to_string().contains(&query))
                .map(|m| PyRep::Integer(m.character_id)).collect()
        }
    };
    Some(Ok(PyRep::List(ids)))
}

fn normalize_audit_timestamp_to_unix(raw: i64) -> Option<u64> {
    if raw <= 0 {
        return None;
    }
    if raw >= 116_444_736_000_000_000 {
        Some(((raw - 116_444_736_000_000_000) / 10_000_000) as u64)
    } else {
        Some(raw as u64)
    }
}

fn handle_audit_member(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let corp_id = session.corporation_id.unwrap_or(0);
    promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    let (is_ceo, roles) = ctx.db.with_snapshot(|snap| {
        snap.effective_corp_roles_at(corp_id, char_id, None, session.corp_role, now_unix)
    });
    if !is_ceo && !eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR) {
        return Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only Directors and CEO can audit member roles and permissions".into() }));
    }
    let target_id = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("characterID").and_then(|a| a.as_i64()));
    let from_ts = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("fromDate").and_then(|a| a.as_i64())).and_then(normalize_audit_timestamp_to_unix);
    let to_ts = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("toDate").and_then(|a| a.as_i64())).and_then(normalize_audit_timestamp_to_unix);
    let limit = args.get(3).and_then(|a| a.as_i64()).or_else(|| kwargs.get("rowsPerPage").and_then(|a| a.as_i64())).and_then(|n| (n > 0).then_some(n as usize));
    let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
        snap.corp_governance.get(&corp_id).map(|g| {
            g.role_audit_log.iter()
                .filter(|&&(tid, _, _, _, ts)| {
                    target_id.is_none_or(|t| t == tid)
                        && from_ts.is_none_or(|f| ts >= f)
                        && to_ts.is_none_or(|t| ts <= t)
                })
                .map(|&(tid, issuer_id, old_r, new_r, ts)| {
                    vec![PyRep::Integer(tid), PyRep::Integer(issuer_id), PyRep::Integer(old_r as i64), PyRep::Integer(new_r as i64), PyRep::Integer(ts as i64)]
                }).collect()
        }).unwrap_or_default()
    });
    if let Some(max_rows) = limit {
        rows.truncate(max_rows);
    }
    Some(Ok(PyRep::rowset(vec!["characterID", "issuerID", "oldRoles", "newRoles", "changeTime"], rows)))
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

        ("corporationSvc", "GetCorpInfo") => {
            crate::services::corp_registry_ops::handle_get_corp_market_info(ctx, session, args)
        }

        ("corpmgr", "GetPublicInfo")
        | ("corpRegistry", "GetCorporation")
        | ("corpmgr", "GetCorporations") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
            let corp_info = ctx.db.with_snapshot(|snap| {
                let allow_war = snap
                    .fleet_and_social_ops
                    .corp_allow_war
                    .get(&corp_id)
                    .copied()
                    .unwrap_or(true);
                snap.corporations.get(&corp_id).map(|c| {
                    let h_divs = snap.corp_hangar_divisions.get(&corp_id).cloned().unwrap_or_default();
                    let w_divs: Vec<(u8, String)> = c.wallet_divisions.iter().map(|d| (d.division_id, d.name.clone())).collect();
                    let m_limit = compute_corp_member_limit(ctx, snap, c.corporation_id, c.ceo_id) as i64;
                    (c.corporation_id, c.name.clone(), c.ticker.clone(), (c.ceo_id > 0).then_some(c.ceo_id), c.members.len() as i64, m_limit, c.tax_rate, c.alliance_id.filter(|&id| id > 0), c.hq_station_id.filter(|&id| id > 0), c.description.clone(), c.url.clone(), h_divs, w_divs, allow_war)
                })
            }).or_else(|| {
                // DERIVED: SDE NPC corporations fallback for starter/NPC corps; always supply a
                // valid ownerID for ceoID (SDE ceo_id or corporation_id) because V24.01 infosvc
                // passes corpinfo.ceoID directly into cfg.eveowners.Get().
                ctx.sde.npc_corporations.get(&corp_id).map(|c| {
                    let ceo_id = c.ceo_id.filter(|&id| id > 0).or(Some(c.corporation_id));
                    (c.corporation_id, c.name.clone(), c.ticker_name.clone(), ceo_id, 10_000i64, 100_000i64, c.tax_rate, None, c.station_id.filter(|&id| id > 0), format!("NPC Corporation {}", c.name), String::new(), HashMap::new(), Vec::new(), false)
                })
            });

            let Some((cid, name, ticker, ceo_id, m_count, m_limit, tax, alliance_id, hq_station, desc, url, h_divs, w_divs, allow_war)) = corp_info else {
                return Some(Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") }));
            };
            let ((s1, s2, s3, c1), (c2, c3), apps_enabled, friendly_fire) = read_corp_gov(ctx, |g| {
                g.get(&cid)
                    .map(|s| (s.logo, s.logo_extra_colors, s.applications_enabled.unwrap_or(true), s.friendly_fire_legal))
                    .unwrap_or(((1, 2, 3, 10), (0, 0), true, false))
            });
            let creator_id = ceo_id.unwrap_or(cid);
            let default_h = ["1st Division", "2nd Division", "3rd Division", "4th Division", "5th Division", "6th Division", "7th Division"];
            let default_w = ["Master Wallet", "2nd Wallet", "3rd Wallet", "4th Wallet", "5th Wallet", "6th Wallet", "7th Wallet"];
            let mut entries = vec![
                ("corporationID".to_string(), PyRep::Integer(cid)),
                ("corporationName".into(), PyRep::String(name)),
                ("ticker".into(), PyRep::String(ticker.clone())),
                ("tickerName".into(), PyRep::String(ticker)),
                ("ceoID".into(), ceo_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("creatorID".into(), PyRep::Integer(creator_id)),
                ("memberCount".into(), PyRep::Integer(m_count)),
                ("memberLimit".into(), PyRep::Integer(m_limit)),
                ("taxRate".into(), PyRep::Real(tax)),
                ("iskTaxRate".into(), PyRep::Real(tax)),
                ("loyaltyPointTaxRate".into(), PyRep::Real(tax)),
                ("allianceID".into(), alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("stationID".into(), hq_station.map(PyRep::Integer).unwrap_or(PyRep::None)),
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
                ("allowWar".into(), PyRep::Bool(allow_war)),
                ("friendlyFire".into(), PyRep::Bool(friendly_fire)),
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
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
            let info = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).map(|c| (c.corporation_id, c.ceo_id, c.alliance_id))
            }).or_else(|| {
                ctx.sde.npc_corporations.get(&corp_id).map(|c| {
                    (
                        c.corporation_id,
                        c.ceo_id.filter(|&id| id > 0).unwrap_or(c.corporation_id),
                        None,
                    )
                })
            });
            let (cid, ceo, aid) = info.unwrap_or((corp_id, 0, None));
            let rep = match method {
                "allianceID" => aid.filter(|&id| id > 0).map(PyRep::Integer).unwrap_or(PyRep::None),
                "ceoID" => (ceo > 0).then_some(PyRep::Integer(ceo)).unwrap_or(PyRep::None),
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
            let cid = ctx.db.with_snapshot(|snap| snap.characters.get(&target_id).map(|c| c.corporation_id).unwrap_or(0));
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
            let (desc, url, tax_rate_opt, allow_war_opt) = if args.first().and_then(|a| a.as_i64()).is_some() {
                (args.get(1).and_then(|a| a.as_str()).unwrap_or(""), args.get(2).and_then(|a| a.as_str()).unwrap_or(""), args.get(3).and_then(|a| a.as_f64()), args.get(4).and_then(|a| a.as_bool()))
            } else {
                (args.first().and_then(|a| a.as_str()).unwrap_or(""), args.get(1).and_then(|a| a.as_str()).unwrap_or(""), args.get(2).and_then(|a| a.as_f64()), args.get(3).and_then(|a| a.as_bool()))
            };

            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let old_allow_war = snap
                    .fleet_and_social_ops
                    .corp_allow_war
                    .get(&corp_id)
                    .copied()
                    .unwrap_or(true);
                let (is_ceo, caller_role) =
                    snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if !is_ceo && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update corporation details".into() });
                }
                let (old_desc, old_url, old_tax) = (corp.description.clone(), corp.url.clone(), corp.tax_rate);
                corp.description = desc.to_string();
                corp.url = url.to_string();
                if let Some(t) = tax_rate_opt { corp.tax_rate = t.clamp(0.0, 1.0); }
                let (updated_desc, updated_url, updated_tax) = (corp.description.clone(), corp.url.clone(), corp.tax_rate);
                let allow_war_changed = allow_war_opt.and_then(|new_aw| {
                    snap.fleet_and_social_ops
                        .corp_allow_war
                        .insert(corp_id, new_aw);
                    (new_aw != old_allow_war).then_some((old_allow_war, new_aw))
                });
                Ok((old_desc, old_url, old_tax, updated_desc, updated_url, updated_tax, allow_war_changed))
            });

            match res {
                Ok((old_desc, old_url, old_tax, desc, url, tax, allow_war_changed)) => {
                    let mut change_entries = vec![
                        (PyRep::String(("description").into()), PyRep::Tuple(vec![PyRep::String(old_desc), PyRep::String(desc.clone())])),
                        (PyRep::String(("url").into()), PyRep::Tuple(vec![PyRep::String(old_url), PyRep::String(url.clone())])),
                        (PyRep::String(("taxRate").into()), PyRep::Tuple(vec![PyRep::Real(old_tax), PyRep::Real(tax)])),
                    ];
                    if let Some((old_aw, new_aw)) = allow_war_changed {
                        change_entries.push((PyRep::String("allowWar".into()), PyRep::Tuple(vec![PyRep::Bool(old_aw), PyRep::Bool(new_aw)])));
                    }
                    let change = PyRep::Dict(change_entries);
                    ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
                    if let Some((_, new_aw)) = allow_war_changed {
                        crate::services::notify_emitters_skills_trade_godma::emit_allow_war_updated(ctx, corp_id, new_aw);
                    }
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
            let c2 = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
            let c3 = args.get(5).and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let is_ceo_or_dir = ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return false;
                }
                let (is_ceo, roles) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                is_ceo || corp_roles::has_role(roles, corp_roles::ROLE_DIRECTOR)
            });
            if !is_ceo_or_dir {
                return Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update the corporation logo".into() }));
            }
            let (old_logo, old_extra) = with_corp_gov(ctx, |g| {
                let state = g.entry(corp_id).or_default();
                (
                    std::mem::replace(&mut state.logo, (s1, s2, s3, c1)),
                    std::mem::replace(&mut state.logo_extra_colors, (c2, c3)),
                )
            });
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorpLogoReady", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(128)]);
            let change = PyRep::Dict(vec![
                (PyRep::String(("shape1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.0), PyRep::Integer(s1)])),
                (PyRep::String(("shape2").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.1), PyRep::Integer(s2)])),
                (PyRep::String(("shape3").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.2), PyRep::Integer(s3)])),
                (PyRep::String(("color1").into()), PyRep::Tuple(vec![PyRep::Integer(old_logo.3), PyRep::Integer(c1)])),
                (PyRep::String(("color2").into()), PyRep::Tuple(vec![PyRep::Integer(old_extra.0), PyRep::Integer(c2)])),
                (PyRep::String(("color3").into()), PyRep::Tuple(vec![PyRep::Integer(old_extra.1), PyRep::Integer(c3)])),
            ]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)), ("shape1", PyRep::Integer(s1)),
                ("shape2", PyRep::Integer(s2)), ("shape3", PyRep::Integer(s3)),
                ("color1", PyRep::Integer(c1)), ("color2", PyRep::Integer(c2)), ("color3", PyRep::Integer(c3)),
            ])))
        }

        ("corpRegistry", "UpdateDivisionNames")
        | ("corpmgr", "UpdateDivisionNames")
        | ("corporationSvc", "UpdateDivisionNames") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(corp_id) = session.corporation_id else {
                return Some(Err(BridgeRpcError { code: "NoCorporation".into(), message: "Character has no active corporation".into() }));
            };
            let dict_payload = args.first().and_then(|a| match a { PyRep::Dict(d) => Some(d.as_slice()), _ => None });
            if args.len() >= 14 || args.first().and_then(|a| a.as_str()).is_some() || dict_payload.is_some() {
                let default_h = ["1st Division", "2nd Division", "3rd Division", "4th Division", "5th Division", "6th Division", "7th Division"];
                let dict_str = |k: &str| dict_payload.and_then(|entries| entries.iter().find(|(ek, _)| ek.as_str() == Some(k)).and_then(|(_, ev)| ev.as_str()));
                let now_unix = ctx.now_unix();
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let (is_ceo, caller_role) =
                        snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                    let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                        return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                    };
                    if !is_ceo && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                        return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only CEO or Director can update division names".into() });
                    }
                    corp.ensure_wallet_divisions();
                    let mut diff = Vec::new();
                    let h_divs = snap.corp_hangar_divisions.entry(corp_id).or_default();
                    for i in 0..7usize {
                        let d_id = (i + 1) as u8;
                        let h_key = format!("division{d_id}");
                        if let Some(h_name) = args.get(i).and_then(|a| a.as_str()).or_else(|| dict_str(&h_key)) {
                            let old_h = h_divs.get(&d_id).cloned().unwrap_or_else(|| default_h[i].to_string());
                            if old_h != h_name {
                                diff.push((PyRep::String(h_key), PyRep::Tuple(vec![PyRep::String(old_h), PyRep::String(h_name.to_string())])));
                            }
                            h_divs.insert(d_id, h_name.to_string());
                        }
                    }
                    for i in 0..7usize {
                        let d_id = (i + 1) as u8;
                        let w_key = format!("walletDivision{d_id}");
                        if let Some(w_name) = args.get(7 + i).and_then(|a| a.as_str()).or_else(|| dict_str(&w_key)) {
                            if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == d_id) {
                                let old_w = std::mem::replace(&mut div.name, w_name.to_string());
                                if old_w != w_name {
                                    diff.push((PyRep::String(w_key), PyRep::Tuple(vec![PyRep::String(old_w), PyRep::String(w_name.to_string())])));
                                }
                            }
                        }
                    }
                    Ok(diff)
                });
                return match res {
                    Ok(diff) => {
                        let first_h = args.first().and_then(|a| a.as_str()).or_else(|| dict_str("division1")).unwrap_or("1st Division").to_string();
                        if !diff.is_empty() {
                            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Dict(diff)]);
                        }
                        Some(Ok(PyRep::keyval([("divisionID", PyRep::Integer(1)), ("name", PyRep::String(first_h))])))
                    }
                    Err(e) => Some(Err(e)),
                };
            }
            let raw_div_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let div_id = match raw_div_id {
                1..=7 => raw_div_id as u8,
                115..=121 => (raw_div_id - 114) as u8,
                1000..=1006 => (raw_div_id - 999) as u8,
                _ => raw_div_id as u8,
            };
            let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Division");
            let wallet_name = args.get(2).and_then(|a| a.as_str()).unwrap_or(new_name);

            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (is_ceo, caller_role) =
                    snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if !is_ceo && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
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

        ("corpRegistry", "GetMember") if !args.first().and_then(|a| a.as_i64()).is_some_and(|id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id)) => {
            crate::services::corp_registry_ops::handle_get_member(ctx, session, args)
        }
        ("corpRegistry", "GetMemberTrackingInfo")
        | ("corpRegistry", "GetMemberTrackingInfoSimple") => {
            crate::services::corp_registry_ops::handle_get_member_tracking_info(ctx, session, method, args)
        }

        ("corpRegistry", "GetMembers")
        | ("corpmgr", "GetMembers")
        | ("corporationSvc", "GetMembers") => {
            handle_get_members_rpc(ctx, session, args)
        }
        ("corpRegistry", "GetMember") => {
            handle_get_members_rpc(ctx, session, args)
        }

        ("corpRegistry", "GetMembersPaged") => {
            handle_get_members_paged(ctx, session, args)
        }

        ("corpRegistry", "GetMembersByIds") => {
            handle_get_members_by_ids(ctx, session, args)
        }

        ("corpRegistry", "GetMemberIDsByQuery") => {
            handle_get_member_ids_by_query(ctx, session, args, kwargs)
        }

        ("corpRegistry", "UpdateMember") | ("corpRegistry", "UpdateMembers") => {
            super::fleet_social_persistence::handle_update_members(ctx, session, method, args)
        }

        ("corpmgr", "AuditMember") => {
            handle_audit_member(ctx, session, args, kwargs)
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
            if ctx.sde.npc_corporations.contains_key(&corp_id) || (1_000_000..2_000_000).contains(&corp_id) {
                return Some(Err(BridgeRpcError {
                    code: "CannotApplyToNpcCorporation".into(),
                    message: "Players cannot submit membership applications to NPC corporations".into(),
                }));
            }
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
        | ("corporationSvc", "GetApplications") => {
            super::fleet_social_corp_tail_dispatch::handle_get_corp_applications_rpc(ctx, session, args, false)
        }

        ("corpRegistry", "GetMyApplications") => {
            super::fleet_social_corp_tail_dispatch::handle_get_my_applications_rpc(ctx, session, false)
        }

        ("corpRegistry", "GetOldApplications") => {
            super::fleet_social_corp_tail_dispatch::handle_get_corp_applications_rpc(ctx, session, args, true)
        }

        ("corpRegistry", "GetMyOldApplications") => {
            super::fleet_social_corp_tail_dispatch::handle_get_my_applications_rpc(ctx, session, true)
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
            if let Some(app_cid) = ctx.db.with_snapshot(|s| {
                s.corporations.values().find(|c| c.applications.contains_key(&app_id)).map(|c| c.corporation_id).or(session.corporation_id)
            }).filter(|&id| id > 0) {
                promote_due_pending_corp_grants(ctx, app_cid);
            }

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
                let (applicant_id, app_msg, app_ts, app_status) = corp
                    .applications
                    .get(&app_id)
                    .map(|a| (a.character_id, a.message.clone(), a.created_unix, a.status.clone()))
                    .ok_or_else(|| BridgeRpcError { code: "ApplicationNotFound".into(), message: format!("Application {app_id} not found") })?;
                if !matches!(app_status.as_str(), "Pending" | "Open" | "Applied" | "Invited" | "Offered" | "Renegotiated") {
                    return Err(BridgeRpcError { code: "ApplicationAlreadyClosed".into(), message: format!("Application {app_id} is already closed ('{app_status}')") });
                }
                let old_corp_id = snap.characters.get(&applicant_id).map(|c| c.corporation_id).unwrap_or(0);
                let is_invitation = app_status.eq_ignore_ascii_case("invited");
                let (is_ceo, eff_roles) =
                    crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                        snap, corp_id, char_id, 0, now_unix,
                    );
                let has_personnel_role = is_ceo
                    || (eff_roles & (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_DIRECTOR)) != 0;

                if action.eq_ignore_ascii_case("withdrawn") {
                    if char_id != applicant_id && !(is_invitation && has_personnel_role) {
                        return Err(BridgeRpcError { code: "AccessDenied".into(), message: "Only the applicant can withdraw their application".into() });
                    }
                    let corp_mut = snap.corporations.get_mut(&corp_id).unwrap();
                    if let Some(app) = corp_mut.applications.get_mut(&app_id) {
                        app.status = "Withdrawn".into();
                    }
                    return Ok((corp_id, applicant_id, old_corp_id, "Withdrawn".to_string(), app_msg, app_ts, None, Vec::new()));
                }

                if is_invitation {
                    if char_id != applicant_id {
                        return Err(BridgeRpcError { code: "AccessDenied".into(), message: "Only the invited character can accept or reject a corporation invitation".into() });
                    }
                } else if !has_personnel_role {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Updating applications requires Personnel Manager or Director role".into() });
                }

                if action.eq_ignore_ascii_case("rejected") {
                    let corp_mut = snap.corporations.get_mut(&corp_id).unwrap();
                    if let Some(app) = corp_mut.applications.get_mut(&app_id) {
                        app.status = "Rejected".into();
                    }
                    return Ok((corp_id, applicant_id, old_corp_id, "Rejected".to_string(), app_msg, app_ts, None, Vec::new()));
                }
                if !action.eq_ignore_ascii_case("accepted") {
                    return Err(BridgeRpcError { code: "InvalidApplicationAction".into(), message: format!("Unknown application action '{action}'") });
                }
                if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(ctx, applicant_id, now_unix, None) {
                    return Err(BridgeRpcError { code: "SessionChangeInProgress".into(), message: format!("Cannot join corporation while 10s session-change timer ({}) is active", active_timer.reason) });
                }
                if snap.characters.get(&applicant_id).is_some_and(|ch| ch.station_id.is_none() && ch.structure_id.is_none()) {
                    return Err(BridgeRpcError { code: "MustBeDockedToJoinCorp".into(), message: "Character must be docked in a station or structure to accept a corporation transition".into() });
                }
                if snap.corporations.values().any(|c| c.corporation_id != corp_id && c.ceo_id == applicant_id) {
                    return Err(BridgeRpcError::user_error("CeoCannotLeaveCorp"));
                }

                if old_corp_id != corp_id {
                    if let Some(old_corp) = snap.corporations.get(&old_corp_id) {
                        if old_corp.ceo_id == applicant_id {
                            return Err(BridgeRpcError::user_error("CeoCannotLeaveCorp"));
                        }
                        if let Some(blocker) = super::fleet_social_corp_tail::member_kick_blocker(snap, old_corp_id, applicant_id, now_unix) {
                            return Err(BridgeRpcError { code: "CannotLeaveOldCorpWithRoles".into(), message: format!("Cannot leave current corporation while {blocker}") });
                        }
                    }
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

                if old_corp_id != corp_id {
                    if let Some(old_corp) = snap.corporations.get_mut(&old_corp_id) {
                        old_corp.members.remove(&applicant_id);
                        old_corp.member_roles.remove(&applicant_id);
                    }
                    if let Some(old_ch) = snap.chat_channels.get_mut(&format!("corpid:{old_corp_id}")) {
                        old_ch.members.remove(&applicant_id);
                    }
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
                let welcome_mail_id = if welcome.0 && !welcome.1.is_empty() {
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.mails.push(MailRecord { message_id: mid, sender_id: ceo_id, recipient_id: applicant_id, subject: welcome.1.clone(), body: welcome.2 });
                    snap.mail_timestamps.insert(mid, now_unix);
                    snap.mail_assigned_labels.insert((applicant_id, mid), vec![1]);
                    Some((mid, ceo_id, welcome.1))
                } else {
                    None
                };
                let released = record_corp_join_in_snap(snap, applicant_id, old_corp_id, corp_id, now_unix);

                Ok((corp_id, applicant_id, old_corp_id, "Accepted".to_string(), app_msg, app_ts, welcome_mail_id, released))
            });

            match res {
                Ok((corp_id, applicant_id, old_corp_id, status_str, app_msg, app_ts, welcome_mail_id, released)) => {
                    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
                    if status_str == "Accepted" {
                        let fallback_sys = session.solar_system_id.unwrap_or(0);
                        let fallback_dock = session.station_id.or(session.structure_id);
                        let (old_ally, new_ally, old_role, app_sys, app_dock) = ctx.db.with_snapshot(|s| {
                            let (sys_id, dock_id) = s
                                .characters
                                .get(&applicant_id)
                                .map(|c| (c.solar_system_id, c.station_id.or(c.structure_id)))
                                .unwrap_or((fallback_sys, fallback_dock));
                            (
                                s.corporations.get(&old_corp_id).and_then(|c| c.alliance_id),
                                s.corporations.get(&corp_id).and_then(|c| c.alliance_id),
                                s.effective_corp_roles_at(old_corp_id, applicant_id, None, 0, now_unix).1,
                                sys_id,
                                dock_id,
                            )
                        });
                        if char_id == applicant_id {
                            session.corporation_id = Some(corp_id);
                            session.alliance_id = new_ally;
                            session.corp_role = 0;
                            if let Some(act_ship) = ctx.db.with_snapshot(|s| s.characters.get(&applicant_id).map(|c| c.active_ship_id)) {
                                if act_ship != 0 {
                                    let _ = ctx.execute_session_transition_sync(
                                        Some(session),
                                        crate::router::SessionTransitionSpec::for_kind(
                                            applicant_id,
                                            crate::router::TransitionKind::BoardShip { ship_id: act_ship },
                                            "corp_change",
                                        )
                                        .with_apply_db(false)
                                        .with_emit_on_session_changed(false),
                                    );
                                }
                            }
                        }
                        if old_corp_id != corp_id {
                            ctx.invalidate_bound_objects_for_session_transition(
                                applicant_id,
                                app_sys,
                                app_dock,
                                "corp_change",
                            );
                        }
                        super::fleet_social_persistence::with_social_state(ctx, |s| {
                            s.forced_session_corp_sync.insert(applicant_id, (corp_id, new_ally, 0));
                        });
                        let m_change = PyRep::Dict(vec![(PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::Integer(old_corp_id), PyRep::Integer(corp_id)]))]);
                        let m_args = vec![PyRep::Integer(corp_id), PyRep::Integer(applicant_id), m_change.clone()];
                        if old_corp_id != corp_id && old_corp_id > 0 {
                            ctx.notify(
                                NotificationTarget::Corporation(old_corp_id),
                                "OnCorporationMemberChanged",
                                "corpid",
                                vec![PyRep::Integer(old_corp_id), PyRep::Integer(applicant_id), m_change],
                            );
                        }
                        ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMemberChanged", "corpid", m_args);
                        let sess_changes = PyRep::Dict(vec![
                            (PyRep::String(("corpid").into()), PyRep::Tuple(vec![PyRep::Integer(old_corp_id), PyRep::Integer(corp_id)])),
                            (PyRep::String(("corprole").into()), PyRep::Tuple(vec![PyRep::Integer(old_role as i64), PyRep::Integer(0)])),
                            (
                                PyRep::String(("allianceid").into()),
                                PyRep::Tuple(vec![
                                    old_ally.map_or(PyRep::None, PyRep::Integer),
                                    new_ally.map_or(PyRep::None, PyRep::Integer),
                                ]),
                            ),
                        ]);
                        ctx.notify(
                            NotificationTarget::Character(applicant_id),
                            "OnSessionChanged",
                            "clientID",
                            vec![PyRep::Bool(false), PyRep::Integer(applicant_id), sess_changes],
                        );
                        if let Some((mid, sender_id, subj)) = welcome_mail_id {
                            let hdr = PyRep::keyval([
                                ("messageID", PyRep::Integer(mid)),
                                ("senderID", PyRep::Integer(sender_id)),
                                ("toCharacterIDs", PyRep::List(vec![PyRep::Integer(applicant_id)])),
                                ("toCorpOrAllianceID", PyRep::Integer(corp_id)),
                                ("subject", PyRep::String(subj)),
                                ("sentDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(now_unix))),
                            ]);
                            ctx.notify(
                                NotificationTarget::Character(applicant_id),
                                "OnNewMailReceived",
                                "charid",
                                vec![hdr],
                            );
                        }
                        let _ = crate::services::quasar_social_chat::on_character_local_chat_membership_refreshed(
                            &ctx.quasar_gateway, &ctx.db, applicant_id as u32, now_unix,
                        );
                        let app_sys = ctx.db.with_snapshot(|s| s.characters.get(&applicant_id).map(|c| c.solar_system_id)).unwrap_or(0);
                        crate::services::space_destiny::record_session_change_timer(
                            ctx,
                            applicant_id,
                            now_unix,
                            app_sys,
                            0,
                            "corp_change",
                        );
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
            let rpc = crate::router::RpcArgs::positional(args);
            let target_id = match rpc.req_positive_i64(0, "characterID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let accept_now = matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            if accept_now {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "InsertInvitation cannot auto-join a character; the target must accept via UpdateApplicationOffer".into(),
                }));
            }
            promote_due_pending_corp_grants(ctx, corp_id);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let ceo_id = corp.ceo_id;
                let (is_ceo, eff_roles) =
                    crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                        snap, corp_id, char_id, 0, now_unix,
                    );
                if !is_ceo
                    && (eff_roles & (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_DIRECTOR)) == 0
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Inviting characters requires Personnel Manager role".into(),
                    });
                }
                if !snap.characters.contains_key(&target_id) {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {target_id} not found"),
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
                        status: "Invited".into(),
                    },
                );
                Ok(app_id)
            });
            match res {
                Ok(app_id) => {
                    let app_kv = PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)), ("corporationID", PyRep::Integer(corp_id)),
                        ("characterID", PyRep::Integer(target_id)), ("applicationText", PyRep::String(("Corporation Invitation").into())),
                        ("status", PyRep::String(("Invited").into())), ("applicationDateTime", PyRep::Integer(now_unix as i64)),
                    ]);
                    ctx.notify(NotificationTarget::Character(target_id), "OnCorporationApplicationChanged", "charid", vec![PyRep::Integer(corp_id), PyRep::Integer(target_id), PyRep::Integer(app_id), app_kv]);
                    Some(Ok(PyRep::keyval([
                        ("applicationID", PyRep::Integer(app_id)),
                        ("characterID", PyRep::Integer(target_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("joined", PyRep::Bool(false)),
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
            let corp_id = session.corporation_id.unwrap_or(0);
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
            let now_unix = ctx.now_unix();
            promote_due_pending_corp_grants(ctx, corp_id);
            let can_kick = ctx.db.with_snapshot(|snap| {
                let Some(c) = snap.corporations.get(&corp_id) else { return false; };
                if !c.members.contains(&target_id) { return false; }
                let (is_ceo, caller_role) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                let auth = char_id == target_id
                    || is_ceo
                    || corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR)
                    || corp_roles::has_role(caller_role, corp_roles::ROLE_PERSONNEL_MANAGER);
                if !auth || target_id == c.ceo_id { return false; }
                if super::fleet_social_corp_tail::member_kick_blocker(snap, corp_id, target_id, now_unix).is_some() {
                    return false;
                }
                !snap.characters.get(&target_id).is_some_and(|ch| ch.station_id.is_none() && ch.structure_id.is_none())
            });
            Some(Ok(PyRep::Bool(can_kick)))
        }

        ("corpRegistry", "ResignFromCorp")
        | ("corpmgr", "ResignFromCorp")
        // rpc-lint: allow alias for corp quit cleanup
        | ("corpRegistry", "LeaveCorporation")
        | ("corpRegistry", "KickOutMember")
        | ("corpRegistry", "KickOutMembers") => {
            super::fleet_social_persistence::handle_kick_out_members(ctx, session, args)
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
            let rpc = crate::router::RpcArgs::positional(args);
            let from_div_i = match rpc.req_i64(0, "fromDivision") {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTransferDivision".into(),
                        message: e.message,
                    }));
                }
            };
            let to_div_i = match rpc.req_i64(1, "toDivision") {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTransferDivision".into(),
                        message: e.message,
                    }));
                }
            };
            let amount_isk = match rpc.req_positive_f64(2, "amountISK") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (Ok(from_div), Ok(to_div)) = (u8::try_from(from_div_i), u8::try_from(to_div_i)) else {
                return Some(Err(BridgeRpcError { code: "InvalidTransferDivision".into(), message: "Source and target wallet divisions must be distinct (1..=7)".into() }));
            };
            if from_div == to_div || !(1..=7).contains(&from_div) || !(1..=7).contains(&to_div) {
                return Some(Err(BridgeRpcError { code: "InvalidTransferDivision".into(), message: "Source and target wallet divisions must be distinct (1..=7)".into() }));
            }
            let transfer_amount = IskCents::from_f64_isk(amount_isk);
            promote_due_pending_corp_grants(ctx, corp_id);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                }
                let (is_ceo, eff_roles) =
                    crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                        snap, corp_id, char_id, 0, now_unix,
                    );
                let take_bit = corp_roles::ROLE_ACCOUNT_CAN_TAKE[usize::from(from_div - 1)];
                if !is_ceo && (eff_roles & (corp_roles::ROLE_DIRECTOR | take_bit)) == 0 {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Transferring between corp wallet divisions requires Director or source wallet division take role".into() });
                }
                let corp = snap.corporations.get_mut(&corp_id).unwrap();
                corp.ensure_wallet_divisions();
                let source_bal = corp.division_balance(from_div);
                let new_source = source_bal.checked_sub(transfer_amount)
                    .ok_or_else(|| BridgeRpcError { code: "InsufficientDivisionFunds".into(), message: "Insufficient funds in source wallet division".into() })?;
                corp.set_division_balance(from_div, new_source);
                let new_target = corp.credit_division(to_div, transfer_amount);
                snap.next_entity_id += 2;
                let eid = snap.next_entity_id;
                let reason = format!("Corp wallet division transfer div {from_div} -> div {to_div}");
                snap.corp_wallet_journals.entry((corp_id, i64::from(from_div))).or_default().push(eve_db::CorpWalletJournalRecord {
                    id: eid - 1, date_unix: now_unix, ref_type: "corporation_account_withdrawal".into(),
                    first_party_id: Some(corp_id), second_party_id: Some(corp_id), amount: -amount_isk, balance: new_source.to_wire_f64(), description: reason.clone(), reason: Some(reason.clone()),
                });
                snap.corp_wallet_journals.entry((corp_id, i64::from(to_div))).or_default().push(eve_db::CorpWalletJournalRecord {
                    id: eid, date_unix: now_unix, ref_type: "corporation_account_withdrawal".into(),
                    first_party_id: Some(corp_id), second_party_id: Some(corp_id), amount: amount_isk, balance: new_target.to_wire_f64(), description: reason.clone(), reason: Some(reason),
                });
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([("fromDivisionID", PyRep::Integer(from_div as i64)), ("toDivisionID", PyRep::Integer(to_div as i64)), ("transferredISK", PyRep::Real(amount_isk))]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => super::fleet_social_corp_gov::handle_corp_gov(ctx, session, service, method, args, kwargs),
    }
}

pub(super) use super::fleet_social_corp_tail::handle_insert_vote_case;

#[cfg(test)]
#[path = "fleet_social_corps_tests.rs"]
pub(crate) mod fleet_social_corps_tests;

#[cfg(test)]
#[path = "fleet_social_corps_edges_tests.rs"]
mod fleet_social_corps_edges_tests;

