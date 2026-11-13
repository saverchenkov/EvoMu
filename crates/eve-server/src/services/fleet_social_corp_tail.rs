//! Corporation Membership Lifecycle, Role Updates, Auto-Kick Execution, Vote Case Creation,
//! and Corporation Asset/Aggression Tail Handlers (`SO-3`).

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{corp_roles, ChatChannelRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashSet;

use super::fleet_social_corps::{promote_due_pending_corp_grants, record_corp_join_in_snap};

/// Resolves the bloodline default NPC corporation for `char_id` (`feat:corporation.npc-corp-placement-rules`,
/// `feat:lifecycle.corp-quit-cleanup`). Never returns the character's school starter corporation.
pub(crate) fn bloodline_default_npc_corp(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> i64 {
    snap.characters
        .get(&char_id)
        .and_then(|c| ctx.sde.bloodline(c.bloodline_id))
        .map(|b| b.corporation_id)
        .filter(|&id| id > 0)
        .unwrap_or(1_000_006)
}

/// Checks whether `target_id` holds active roles, grantable/scoped roles, pending role grants,
/// role-granting titles, or is within the 24-hour (`86_400s`) role-drop cooldown in `corp_id`
/// (`feat:lifecycle.corp-autokick-queue`, `feat:lifecycle.corp-join-leaves-old-corp`).
pub(crate) fn member_kick_blocker(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    target_id: i64,
    now_unix: u64,
) -> Option<&'static str> {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return None;
    };
    if corp.member_roles.get(&target_id).copied().unwrap_or(0) != 0 {
        return Some("MemberHasRoles");
    }
    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, target_id)) {
        if (ra.roles
            | ra.grantable_roles
            | ra.roles_at_hq
            | ra.grantable_roles_at_hq
            | ra.roles_at_base
            | ra.grantable_roles_at_base
            | ra.roles_at_other
            | ra.grantable_roles_at_other)
            != 0
        {
            return Some("MemberHasRoles");
        }
    }
    if let Some(g) = snap.corp_governance.get(&corp_id) {
        if g.pending_role_grants.contains_key(&target_id)
            || g.pending_title_role_grants.contains_key(&target_id)
            || g.pending_scoped_role_grants.contains_key(&target_id)
        {
            return Some("MemberHasRoles");
        }
        if let Some(&tmask) = g.member_titles.get(&target_id) {
            if title_mask_grants_roles(snap, corp_id, tmask) {
                return Some("MemberHasRoleGrantingTitle");
            }
        }
        if let Some(&drop_ts) = g.role_drop_timestamps.get(&target_id) {
            if now_unix < drop_ts.saturating_add(86_400) {
                return Some("RoleDropCooldownActive");
            }
        }
    }
    None
}

pub(crate) fn title_mask_grants_roles(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    tmask: u64,
) -> bool {
    if tmask == 0 {
        return false;
    }
    if let Some(g) = snap.corp_governance.get(&corp_id) {
        for (&tid, &(_, troles)) in &g.titles {
            let bit = eve_db::snapshot_ext::title_id_to_bit(tid);
            if (tmask & bit) != 0 && troles != 0 {
                return true;
            }
        }
    }
    if let Some(titles) = snap.corp_titles.get(&corp_id) {
        for t in titles {
            let bit = eve_db::snapshot_ext::title_id_to_bit(t.title_id);
            if (tmask & bit) != 0
                && (!t.roles.is_empty()
                    || !t.grantable_roles.is_empty()
                    || !t.roles_at_hq.is_empty()
                    || !t.roles_at_base.is_empty()
                    || !t.roles_at_other.is_empty())
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn handle_update_members(
    ctx: &ServerContext,
    session: &mut SessionState,
    _method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (corp_id, char_id, _) =
        match super::fleet_social_corps::require_corp_caller(ctx, session, None) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };

    struct MemberUpdateSpec {
        target_char_id: i64,
        division_id_opt: Option<i64>,
        squadron_id_opt: Option<i64>,
        has_negative_mask: bool,
        new_roles_opt: Option<u64>,
        grantable_roles_opt: Option<u64>,
        roles_at_hq_opt: Option<u64>,
        grantable_roles_at_hq_opt: Option<u64>,
        roles_at_base_opt: Option<u64>,
        grantable_roles_at_base_opt: Option<u64>,
        roles_at_other_opt: Option<u64>,
        grantable_roles_at_other_opt: Option<u64>,
        base_id_opt: Option<i64>,
        title_mask_opt: Option<u64>,
    }

    let mask_opt = |opt: Option<&PyRep>, neg: &mut bool| -> Option<u64> {
        opt.and_then(|a| a.as_i64()).map(|v| {
            if v < 0 {
                *neg = true;
            }
            v as u64
        })
    };

    let parse_row_vec = |r: &[PyRep]| -> Option<MemberUpdateSpec> {
        let tid = r.first()?.as_i64()?;
        let mut neg = false;
        if matches!(r.get(1), Some(PyRep::String(_))) && r.len() == 4 {
            Some(MemberUpdateSpec {
                target_char_id: tid,
                division_id_opt: None,
                squadron_id_opt: None,
                new_roles_opt: mask_opt(r.get(2), &mut neg),
                grantable_roles_opt: None,
                roles_at_hq_opt: None,
                grantable_roles_at_hq_opt: None,
                roles_at_base_opt: None,
                grantable_roles_at_base_opt: None,
                roles_at_other_opt: None,
                grantable_roles_at_other_opt: None,
                base_id_opt: None,
                title_mask_opt: mask_opt(r.get(3), &mut neg),
                has_negative_mask: neg,
            })
        } else if matches!(r.get(1), Some(PyRep::String(_))) {
            Some(MemberUpdateSpec {
                target_char_id: tid,
                division_id_opt: r.get(2).and_then(|a| a.as_i64()),
                squadron_id_opt: r.get(3).and_then(|a| a.as_i64()),
                new_roles_opt: mask_opt(r.get(4), &mut neg),
                grantable_roles_opt: mask_opt(r.get(5), &mut neg),
                roles_at_hq_opt: mask_opt(r.get(6), &mut neg),
                grantable_roles_at_hq_opt: mask_opt(r.get(7), &mut neg),
                roles_at_base_opt: mask_opt(r.get(8), &mut neg),
                grantable_roles_at_base_opt: mask_opt(r.get(9), &mut neg),
                roles_at_other_opt: mask_opt(r.get(10), &mut neg),
                grantable_roles_at_other_opt: mask_opt(r.get(11), &mut neg),
                base_id_opt: r.get(12).and_then(|a| a.as_i64()),
                title_mask_opt: mask_opt(r.get(13), &mut neg),
                has_negative_mask: neg,
            })
        } else {
            let offset = usize::from(matches!(r.get(3), Some(PyRep::Bool(_))));
            Some(MemberUpdateSpec {
                target_char_id: tid,
                division_id_opt: None,
                squadron_id_opt: None,
                new_roles_opt: mask_opt(r.get(1), &mut neg),
                title_mask_opt: mask_opt(r.get(2), &mut neg),
                grantable_roles_opt: mask_opt(r.get(3 + offset), &mut neg),
                roles_at_hq_opt: mask_opt(r.get(4 + offset), &mut neg),
                grantable_roles_at_hq_opt: mask_opt(r.get(5 + offset), &mut neg),
                roles_at_base_opt: mask_opt(r.get(6 + offset), &mut neg),
                grantable_roles_at_base_opt: mask_opt(r.get(7 + offset), &mut neg),
                roles_at_other_opt: mask_opt(r.get(8 + offset), &mut neg),
                grantable_roles_at_other_opt: mask_opt(r.get(9 + offset), &mut neg),
                base_id_opt: r.get(10 + offset).and_then(|a| a.as_i64()),
                has_negative_mask: neg,
            })
        }
    };

    let parse_item = |item: &PyRep| -> Option<MemberUpdateSpec> {
        match item {
            PyRep::List(r) | PyRep::Tuple(r) => parse_row_vec(r),
            PyRep::Dict(_) | PyRep::KeyVal(_) => {
                let tid = item
                    .dict_get("characterID")
                    .or_else(|| item.dict_get("memberID"))
                    .and_then(|v| v.as_i64())?;
                let mut neg = false;
                let r_opt = item.dict_get("roles").or_else(|| item.dict_get("role"));
                Some(MemberUpdateSpec {
                    target_char_id: tid,
                    division_id_opt: item.dict_get("divisionID").and_then(|v| v.as_i64()),
                    squadron_id_opt: item.dict_get("squadronID").and_then(|v| v.as_i64()),
                    new_roles_opt: mask_opt(r_opt, &mut neg),
                    grantable_roles_opt: mask_opt(item.dict_get("grantableRoles"), &mut neg),
                    roles_at_hq_opt: mask_opt(item.dict_get("rolesAtHQ"), &mut neg),
                    grantable_roles_at_hq_opt: mask_opt(item.dict_get("grantableRolesAtHQ"), &mut neg),
                    roles_at_base_opt: mask_opt(item.dict_get("rolesAtBase"), &mut neg),
                    grantable_roles_at_base_opt: mask_opt(item.dict_get("grantableRolesAtBase"), &mut neg),
                    roles_at_other_opt: mask_opt(item.dict_get("rolesAtOther"), &mut neg),
                    grantable_roles_at_other_opt: mask_opt(item.dict_get("grantableRolesAtOther"), &mut neg),
                    base_id_opt: item.dict_get("baseID").and_then(|v| v.as_i64()),
                    title_mask_opt: mask_opt(item.dict_get("titleMask"), &mut neg),
                    has_negative_mask: neg,
                })
            }
            _ => None,
        }
    };

    let specs: Vec<MemberUpdateSpec> = match args.first() {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            if items.first().and_then(|x| x.as_i64()).is_some() {
                parse_row_vec(items).into_iter().collect()
            } else {
                items.iter().filter_map(parse_item).collect()
            }
        }
        Some(PyRep::Rowset { rows, .. }) => rows.iter().filter_map(|r| parse_row_vec(r)).collect(),
        Some(d @ (PyRep::Dict(_) | PyRep::KeyVal(_))) => parse_item(d).into_iter().collect(),
        Some(PyRep::Integer(_)) => parse_row_vec(args).into_iter().collect(),
        _ => Vec::new(),
    };

    if specs.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "UpdateMember/UpdateMembers requires at least one target characterID".into(),
            client_error: None,
        }));
    }
    if specs.iter().any(|sp| sp.has_negative_mask) {
        return Some(Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied)));
    }

    promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let (is_ceo, caller_role) =
            snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
                client_error: None,
            });
        };
        if !is_ceo && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Only CEO or Director can update corporation roles".into(),
                client_error: None,
            });
        }
        for sp in &specs {
            if !corp.members.contains(&sp.target_char_id) {
                return Err(BridgeRpcError {
                    code: "TargetNotInCorp".into(),
                    message: format!("Character {} is not a member of corporation {corp_id}", sp.target_char_id),
                    client_error: None,
                });
            }
            if !is_ceo {
                let old_r = corp.member_roles.get(&sp.target_char_id).copied().unwrap_or(0);
                let old_ra = snap
                    .corp_role_assignments
                    .get(&(corp_id, sp.target_char_id))
                    .cloned()
                    .unwrap_or_default();
                let chk = |opt: Option<u64>, cur: u64| opt.is_some_and(|n| ((n ^ cur) & corp_roles::ROLE_DIRECTOR) != 0);
                let dir_changed = chk(sp.new_roles_opt, old_r)
                    || chk(sp.grantable_roles_opt, old_ra.grantable_roles)
                    || chk(sp.roles_at_hq_opt, old_ra.roles_at_hq)
                    || chk(sp.grantable_roles_at_hq_opt, old_ra.grantable_roles_at_hq)
                    || chk(sp.roles_at_base_opt, old_ra.roles_at_base)
                    || chk(sp.grantable_roles_at_base_opt, old_ra.grantable_roles_at_base)
                    || chk(sp.roles_at_other_opt, old_ra.roles_at_other)
                    || chk(sp.grantable_roles_at_other_opt, old_ra.grantable_roles_at_other);
                if dir_changed {
                    return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                }
            }
        }

        let dec = |m: u64| -> Vec<String> {
            crate::esi::corp_governance_routes::decode_corp_roles(m)
                .into_iter()
                .map(String::from)
                .collect()
        };
        const HANGAR_DIV_MASK: u64 = eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK;
        let split_hangar = |old_m: u64, prev_p: u64, opt_m: Option<u64>| -> (u64, u64, u64, u64) {
            let new_m = opt_m.unwrap_or(old_m | prev_p);
            let (held, desired) = (old_m & HANGAR_DIV_MASK, new_m & HANGAR_DIV_MASK);
            let newly = desired & !held & !prev_p;
            let pend = if opt_m.is_some() { newly | (desired & prev_p) } else { prev_p };
            let eff = if opt_m.is_some() { (new_m & !HANGAR_DIV_MASK) | (desired & held) } else { old_m };
            (new_m, eff, pend, newly)
        };
        let mut results: Vec<(i64, u64, u64, u64, bool, (u64, u64, u64, u64, u64, u64))> = Vec::new();
        for sp in &specs {
            let target_char_id = sp.target_char_id;
            let old_roles = snap
                .corporations
                .get(&corp_id)
                .and_then(|c| c.member_roles.get(&target_char_id).copied())
                .unwrap_or(0);
            let old_ra = snap
                .corp_role_assignments
                .get(&(corp_id, target_char_id))
                .cloned()
                .unwrap_or_default();
            let old_title_mask = snap
                .corp_governance
                .get(&corp_id)
                .and_then(|g| g.member_titles.get(&target_char_id).copied())
                .unwrap_or(0);
            let old_had_title_roles = title_mask_grants_roles(snap, corp_id, old_title_mask);
            let (prev_pending, prev_scoped) = snap
                .corp_governance
                .get(&corp_id)
                .map(|g| {
                    (
                        g.pending_role_grants.get(&target_char_id).copied(),
                        g.pending_scoped_role_grants.get(&target_char_id).copied(),
                    )
                })
                .unwrap_or((None, None));
            let prev_pending_hangar = prev_pending.map_or(0, |(pm, _)| pm & HANGAR_DIV_MASK);
            let (prev_hq_p, prev_base_p, prev_other_p) = prev_scoped.map_or((0, 0, 0), |(h, b, o, _)| {
                (h & HANGAR_DIV_MASK, b & HANGAR_DIV_MASK, o & HANGAR_DIV_MASK)
            });
            let (new_roles, effective_roles, pending_hangar, newly_added_hangar) =
                split_hangar(old_roles, prev_pending_hangar, sp.new_roles_opt);
            let (new_hq, eff_hq, pend_hq, newly_hq) =
                split_hangar(old_ra.roles_at_hq, prev_hq_p, sp.roles_at_hq_opt);
            let (new_base, eff_base, pend_base, newly_base) =
                split_hangar(old_ra.roles_at_base, prev_base_p, sp.roles_at_base_opt);
            let (new_other, eff_other, pend_other, newly_other) =
                split_hangar(old_ra.roles_at_other, prev_other_p, sp.roles_at_other_opt);
            if let Some(corp_mut) = snap.corporations.get_mut(&corp_id) {
                corp_mut.member_roles.insert(target_char_id, effective_roles);
            }
            let new_had_title_roles = sp
                .title_mask_opt
                .map(|tm| title_mask_grants_roles(snap, corp_id, tm))
                .unwrap_or(old_had_title_roles);
            let new_grantable = sp.grantable_roles_opt.unwrap_or(old_ra.grantable_roles);
            let new_grantable_hq = sp.grantable_roles_at_hq_opt.unwrap_or(old_ra.grantable_roles_at_hq);
            let new_grantable_base = sp.grantable_roles_at_base_opt.unwrap_or(old_ra.grantable_roles_at_base);
            let new_grantable_other = sp.grantable_roles_at_other_opt.unwrap_or(old_ra.grantable_roles_at_other);
            let old_any_roles = old_roles
                | prev_pending_hangar
                | old_ra.grantable_roles
                | old_ra.roles_at_hq
                | old_ra.grantable_roles_at_hq
                | old_ra.roles_at_base
                | old_ra.grantable_roles_at_base
                | old_ra.roles_at_other
                | old_ra.grantable_roles_at_other;
            let new_any_roles = effective_roles
                | pending_hangar
                | new_grantable
                | eff_hq
                | new_grantable_hq
                | eff_base
                | new_grantable_base
                | eff_other
                | new_grantable_other;
            let roles_dropped = (old_any_roles & !new_any_roles) != 0
                || (old_had_title_roles && !new_had_title_roles);

            let state = snap.corp_governance.entry(corp_id).or_default();
            if roles_dropped {
                state.role_drop_timestamps.insert(target_char_id, now_unix);
            }
            if sp.division_id_opt.is_some() || sp.squadron_id_opt.is_some() {
                let cur = state.member_divisions.get(&target_char_id).copied().unwrap_or((0, 0));
                state.member_divisions.insert(
                    target_char_id,
                    (
                        sp.division_id_opt.unwrap_or(cur.0),
                        sp.squadron_id_opt.unwrap_or(cur.1),
                    ),
                );
            }
            if let Some(base_id) = sp.base_id_opt {
                if base_id > 0 {
                    state.member_bases.insert(target_char_id, base_id);
                } else {
                    state.member_bases.remove(&target_char_id);
                }
            }
            state
                .role_audit_log
                .push((target_char_id, char_id, old_roles, new_roles, now_unix));
            let mut pending_title_delayed = state
                .pending_title_role_grants
                .get(&target_char_id)
                .filter(|&&(_, eff)| now_unix < eff)
                .map_or(0, |&(b, _)| b & HANGAR_DIV_MASK);
            if let Some(tm) = sp.title_mask_opt {
                let prev_title_entry = state.pending_title_role_grants.get(&target_char_id).copied();
                let mut old_title_delayed = 0u64;
                let mut desired_title_delayed = 0u64;
                for (&tid, &(_, troles)) in &state.titles {
                    let bit = eve_db::snapshot_ext::title_id_to_bit(tid);
                    if (old_title_mask & bit) != 0 {
                        old_title_delayed |= troles & HANGAR_DIV_MASK;
                    }
                    if (tm & bit) != 0 {
                        desired_title_delayed |= troles & HANGAR_DIV_MASK;
                    }
                }
                let matured_old_title_delayed = old_title_delayed & !pending_title_delayed;
                let newly_title_delayed = desired_title_delayed
                    & !matured_old_title_delayed
                    & !(old_roles & HANGAR_DIV_MASK)
                    & !pending_title_delayed;
                let still_pending_title =
                    newly_title_delayed | (desired_title_delayed & pending_title_delayed);
                pending_title_delayed = still_pending_title;
                if still_pending_title != 0 {
                    let ready_at = if newly_title_delayed != 0 {
                        now_unix + 86_400
                    } else {
                        prev_title_entry.map_or(now_unix + 86_400, |(_, eff)| eff)
                    };
                    state
                        .pending_title_role_grants
                        .insert(target_char_id, (still_pending_title, ready_at));
                } else {
                    state.pending_title_role_grants.remove(&target_char_id);
                }
                state.member_titles.insert(target_char_id, tm);
                let mut tids: std::collections::BTreeSet<i64> = state
                    .titles
                    .keys()
                    .copied()
                    .filter(|&tid| (tm & eve_db::snapshot_ext::title_id_to_bit(tid)) != 0)
                    .collect();
                for b in 0..16u64 {
                    if (tm & (1u64 << b)) != 0 {
                        tids.insert(1i64 << b);
                        tids.insert((b + 1) as i64);
                    }
                }
                if tids.is_empty() && tm > 0 {
                    tids.insert(tm as i64);
                }
                snap.corp_member_titles
                    .insert((corp_id, target_char_id), tids.into_iter().collect());
            }
            if sp.new_roles_opt.is_some() {
                if pending_hangar != 0 {
                    let ready_at = if newly_added_hangar != 0 {
                        now_unix + 86_400
                    } else {
                        prev_pending.map_or(now_unix + 86_400, |(_, eff)| eff)
                    };
                    state
                        .pending_role_grants
                        .insert(target_char_id, (effective_roles | pending_hangar, ready_at));
                } else {
                    state.pending_role_grants.remove(&target_char_id);
                }
            }
            if sp.roles_at_hq_opt.is_some()
                || sp.roles_at_base_opt.is_some()
                || sp.roles_at_other_opt.is_some()
            {
                if (pend_hq | pend_base | pend_other) != 0 {
                    let ready_at = if (newly_hq | newly_base | newly_other) != 0 {
                        now_unix + 86_400
                    } else {
                        prev_scoped.map_or(now_unix + 86_400, |(_, _, _, eff)| eff)
                    };
                    state
                        .pending_scoped_role_grants
                        .insert(target_char_id, (pend_hq, pend_base, pend_other, ready_at));
                } else {
                    state.pending_scoped_role_grants.remove(&target_char_id);
                }
            }
            snap.corp_role_assignments.insert(
                (corp_id, target_char_id),
                eve_db::snapshot_ext::CorpMemberRoleAssignment {
                    character_id: target_char_id,
                    roles: effective_roles,
                    grantable_roles: new_grantable,
                    roles_at_hq: eff_hq,
                    grantable_roles_at_hq: new_grantable_hq,
                    roles_at_base: eff_base,
                    grantable_roles_at_base: new_grantable_base,
                    roles_at_other: eff_other,
                    grantable_roles_at_other: new_grantable_other,
                },
            );
            snap.char_scoped_roles.insert(
                target_char_id,
                eve_db::snapshot_ext::CharacterScopedRolesRecord {
                    roles: dec(effective_roles),
                    roles_at_hq: dec(eff_hq),
                    roles_at_base: dec(eff_base),
                    roles_at_other: dec(eff_other),
                },
            );
            let role_diffs = [
                ("roles", old_roles | prev_pending_hangar, new_roles, sp.new_roles_opt.is_some()),
                ("grantable_roles", old_ra.grantable_roles, new_grantable, sp.grantable_roles_opt.is_some()),
                ("roles_at_hq", old_ra.roles_at_hq | prev_hq_p, new_hq, sp.roles_at_hq_opt.is_some()),
                ("grantable_roles_at_hq", old_ra.grantable_roles_at_hq, new_grantable_hq, sp.grantable_roles_at_hq_opt.is_some()),
                ("roles_at_base", old_ra.roles_at_base | prev_base_p, new_base, sp.roles_at_base_opt.is_some()),
                ("grantable_roles_at_base", old_ra.grantable_roles_at_base, new_grantable_base, sp.grantable_roles_at_base_opt.is_some()),
                ("roles_at_other", old_ra.roles_at_other | prev_other_p, new_other, sp.roles_at_other_opt.is_some()),
                ("grantable_roles_at_other", old_ra.grantable_roles_at_other, new_grantable_other, sp.grantable_roles_at_other_opt.is_some()),
            ];
            let hist = snap.corp_role_history.entry(corp_id).or_default();
            let mut pushed_any = false;
            for (rtype, om, nm, specified) in role_diffs {
                if specified && om != nm {
                    hist.push(eve_db::snapshot_ext::CorpRoleHistoryEntry {
                        character_id: target_char_id,
                        changed_at_unix: now_unix,
                        issuer_id: char_id,
                        role_type: rtype.into(),
                        old_roles: dec(om),
                        new_roles: dec(nm),
                    });
                    pushed_any = true;
                }
            }
            if !pushed_any && sp.new_roles_opt.is_some() {
                hist.push(eve_db::snapshot_ext::CorpRoleHistoryEntry {
                    character_id: target_char_id,
                    changed_at_unix: now_unix,
                    issuer_id: char_id,
                    role_type: "roles".into(),
                    old_roles: dec(old_roles),
                    new_roles: dec(new_roles),
                });
            }
            let delayed = (pending_hangar | pending_title_delayed | pend_hq | pend_base | pend_other) != 0
                || new_roles > old_roles;
            results.push((
                target_char_id,
                old_roles,
                new_roles,
                effective_roles,
                delayed,
                (old_ra.roles_at_hq, eff_hq, old_ra.roles_at_base, eff_base, old_ra.roles_at_other, eff_other),
            ));
        }
        let released_controllers =
            crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(snap);
        Ok((results, released_controllers))
    });

    match res {
        Ok((results, released_controllers)) => {
            crate::services::structure_control_ops::notify_released_structure_controllers(
                ctx,
                &released_controllers,
            );
            let corp_ally = ctx
                .db
                .with_snapshot(|s| s.corporations.get(&corp_id).and_then(|c| c.alliance_id));
            for &(tid, old_roles, new_roles, effective_roles, _, (old_hq, eff_hq, old_base, eff_base, old_other, eff_other)) in &results {
                if session.character_id == Some(tid) {
                    session.corp_role = effective_roles;
                    if let Some((_, _, _, restored_ship)) = released_controllers.iter().find(|r| r.1 == tid) {
                        if *restored_ship > 0 {
                            let _ = ctx.execute_session_transition_sync(
                                Some(session),
                                crate::router::SessionTransitionSpec::for_kind(
                                    tid,
                                    crate::router::TransitionKind::BoardShip { ship_id: *restored_ship },
                                    "structure_control_release",
                                )
                                .with_apply_db(false)
                                .with_emit_on_session_changed(false),
                            );
                        }
                    }
                } else {
                    super::fleet_social_persistence::with_social_state(ctx, |s| {
                        s.forced_session_corp_sync
                            .insert(tid, (corp_id, corp_ally, effective_roles));
                    });
                }
                emit_corp_role_session_changed(
                    ctx, corp_id, tid, old_roles, effective_roles,
                    old_hq, eff_hq, old_base, eff_base, old_other, eff_other,
                );
                let change = PyRep::Dict(vec![(
                    PyRep::String(("roles").into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(old_roles as i64),
                        PyRep::Integer(new_roles as i64),
                    ]),
                )]);
                ctx.emit_typed(
                    NotificationTarget::Corporation(corp_id),
                    crate::router::notify_registry::OnCorporationMemberChanged {
                        corporation_id: corp_id,
                        member_id: tid,
                        change,
                    },
                );
            }
            let &(first_tid, _, first_new, _, delayed, _) = &results[0];
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(first_tid)),
                ("roles", PyRep::Integer(first_new as i64)),
                ("delayed24h", PyRep::Bool(delayed)),
                ("updatedCount", PyRep::Integer(results.len() as i64)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_corp_role_session_changed(
    ctx: &ServerContext,
    corp_id: i64,
    tid: i64,
    old_roles: u64,
    new_roles: u64,
    old_hq: u64,
    new_hq: u64,
    old_base: u64,
    new_base: u64,
    old_other: u64,
    new_other: u64,
) {
    let mut changes: Vec<(PyRep, PyRep)> = Vec::new();
    let pair = |a: u64, b: u64| PyRep::Tuple(vec![PyRep::Integer(a as i64), PyRep::Integer(b as i64)]);
    if old_roles != new_roles {
        changes.push((PyRep::String("corprole".into()), pair(old_roles, new_roles)));
        changes.push((PyRep::String("rolesAtAll".into()), pair(old_roles, new_roles)));
    }
    if old_hq != new_hq {
        changes.push((PyRep::String("rolesAtHQ".into()), pair(old_hq, new_hq)));
    }
    if old_base != new_base {
        changes.push((PyRep::String("rolesAtBase".into()), pair(old_base, new_base)));
    }
    if old_other != new_other {
        changes.push((PyRep::String("rolesAtOther".into()), pair(old_other, new_other)));
    }
    if changes.is_empty() {
        return;
    }
    let (uid, ally_id, ship_id, sta_id, st_id, sys_id) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&tid);
        let ally = ch.and_then(|c| c.alliance_id).or_else(|| s.corporations.get(&corp_id).and_then(|c| c.alliance_id));
        let sys = ch.map(|c| c.solar_system_id).filter(|&s| s > 0).unwrap_or_else(|| {
            ctx.bound_objects
                .read()
                .ok()
                .and_then(|b| b.values().find(|e| e.character_id == tid && e.service == "beyonce" && e.object_id > 0).map(|e| e.object_id as u32))
                .unwrap_or(0)
        });
        (
            ch.map_or(1, |c| c.user_id),
            ally,
            ch.map_or(0, |c| c.active_ship_id),
            ch.and_then(|c| c.station_id),
            ch.and_then(|c| c.structure_id),
            sys,
        )
    });
    let (const_id, reg_id) = ctx.sde.solar_systems.get(&sys_id).map_or((20_000_020, 10_000_002), |sys| (sys.constellation_id, sys.region_id));
    let sess_kv = PyRep::keyval([
        ("userid", PyRep::Integer(uid as i64)),
        ("charid", PyRep::Integer(tid)),
        ("corpid", PyRep::Integer(corp_id)),
        ("allianceid", ally_id.map_or(PyRep::None, PyRep::Integer)),
        ("corprole", PyRep::Integer(new_roles as i64)),
        ("rolesAtAll", PyRep::Integer(new_roles as i64)),
        ("rolesAtHQ", PyRep::Integer(new_hq as i64)),
        ("rolesAtBase", PyRep::Integer(new_base as i64)),
        ("rolesAtOther", PyRep::Integer(new_other as i64)),
        ("shipid", if ship_id > 0 { PyRep::Integer(ship_id) } else { PyRep::None }),
        ("stationid", sta_id.map_or(PyRep::None, PyRep::Integer)),
        ("structureid", st_id.map_or(PyRep::None, PyRep::Integer)),
        ("solarsystemid", if sta_id.is_none() && st_id.is_none() { PyRep::Integer(sys_id as i64) } else { PyRep::None }),
        ("solarsystemid2", PyRep::Integer(sys_id as i64)),
        ("constellationid", PyRep::Integer(const_id as i64)),
        ("regionid", PyRep::Integer(reg_id as i64)),
        ("sessionChangeReason", PyRep::String("corprole".into())),
        ("nextSessionChange", PyRep::None),
    ]);
    ctx.emit_typed(
        NotificationTarget::Character(tid),
        crate::router::notify_registry::OnSessionChanged {
            is_remote: false,
            session: sess_kv,
            change: PyRep::Dict(changes),
        },
    );
}

struct MemberRemovalOutcome {
    npc_corp: i64,
    old_role: u64,
    old_alliance: Option<i64>,
    closed: bool,
    released: Vec<(i64, i64, u32, i64)>,
    ceo_liquidation_wallet: Option<(i64, eve_db::IskCents)>,
    item_mutations: Vec<(Option<eve_db::InventoryItemRecord>, Option<eve_db::InventoryItemRecord>)>,
}

fn emit_closure_liquidation_notifications(
    ctx: &ServerContext,
    tid: i64,
    ceo_liquidation_wallet: Option<(i64, eve_db::IskCents)>,
    item_mutations: &[(Option<eve_db::InventoryItemRecord>, Option<eve_db::InventoryItemRecord>)],
) {
    if let Some((wallet_char_id, new_bal)) = ceo_liquidation_wallet {
        ctx.notify_wallet_balance(false, wallet_char_id, 1, new_bal);
    }
    for (pre, post) in item_mutations {
        ctx.notify_item_mutation(tid, pre.as_ref(), post.as_ref());
    }
}

fn remove_member_to_bloodline_npc_in_snap(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    tid: i64,
    now_unix: u64,
) -> MemberRemovalOutcome {
    let npc_corp = bloodline_default_npc_corp(ctx, snap, tid);
    let old_role = snap
        .corporations
        .get(&corp_id)
        .and_then(|c| c.member_roles.get(&tid).copied())
        .unwrap_or(0);
    let old_alliance = snap
        .corporations
        .get(&corp_id)
        .and_then(|c| c.alliance_id)
        .or_else(|| snap.characters.get(&tid).and_then(|c| c.alliance_id));
    let mut closed = false;
    let mut ceo_liquidation_wallet = None;
    let mut mutated_by_id: std::collections::HashMap<
        i64,
        (Option<eve_db::InventoryItemRecord>, Option<eve_db::InventoryItemRecord>),
    > = std::collections::HashMap::new();
    if let Some(corp_mut) = snap.corporations.get_mut(&corp_id) {
        corp_mut.members.remove(&tid);
        corp_mut.member_roles.remove(&tid);
        if corp_id >= 2_000_000 && corp_mut.members.is_empty() {
            closed = true;
        }
    }
    if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
        g.pending_role_grants.remove(&tid);
        g.pending_title_role_grants.remove(&tid);
        g.pending_scoped_role_grants.remove(&tid);
        g.pending_auto_kicks.remove(&tid);
        g.member_titles.remove(&tid);
        g.member_divisions.remove(&tid);
        g.member_bases.remove(&tid);
        g.member_account_keys.remove(&tid);
        g.role_drop_timestamps.remove(&tid);
    }
    snap.corp_member_titles.remove(&(corp_id, tid));
    snap.corp_role_assignments.remove(&(corp_id, tid));
    snap.char_scoped_roles.remove(&tid);
    if let Some(ch_rec) = snap.chat_channels.get_mut(&format!("corpid:{corp_id}")) {
        ch_rec.members.remove(&tid);
        ch_rec.operators.remove(&tid);
    }
    snap.chat_channels
        .entry(format!("corpid:{npc_corp}"))
        .or_insert_with(|| ChatChannelRecord {
            channel_id: format!("corpid:{npc_corp}"),
            name: format!("Corporation {npc_corp}"),
            motd: String::new(),
            owner_id: None,
            members: HashSet::new(),
            is_system_local: false,
            solar_system_id: None,
            is_private: false,
            operators: HashSet::new(),
            password: None,
            is_delayed_local: false,
        })
        .members
        .insert(tid);
    if closed {
        let corp_order_ids: Vec<i64> = snap
            .market_orders
            .values()
            .filter(|o| o.owner_id == corp_id)
            .map(|o| o.order_id)
            .collect();
        let mut escrow_refund_cents = 0_i64;
        for oid in corp_order_ids {
            if let Some(ord) = snap.market_orders.remove(&oid) {
                snap.market_order_meta.remove(&oid);
                if ord.is_bid {
                    if ord.escrow.0 > 0 {
                        escrow_refund_cents = escrow_refund_cents.saturating_add(ord.escrow.0);
                    }
                } else if ord.vol_remaining > 0 {
                    let next_max = snap
                        .next_entity_id
                        .max(snap.items.keys().copied().max().unwrap_or(1_000_000));
                    snap.next_entity_id = next_max + 1;
                    let ret_id = snap.next_entity_id;
                    let ret_item = eve_db::InventoryItemRecord {
                        item_id: ret_id,
                        type_id: ord.type_id,
                        owner_id: tid,
                        location_id: ord.station_id,
                        flag: eve_db::InventoryFlag::Deliveries,
                        quantity: ord.vol_remaining,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    };
                    snap.items.insert(ret_id, ret_item.clone());
                    mutated_by_id.insert(ret_id, (None, Some(ret_item)));
                }
            }
        }
        let mut total_corp_cents = escrow_refund_cents;
        if let Some(corp_rec) = snap.corporations.get_mut(&corp_id) {
            corp_rec.ensure_wallet_divisions();
            for d in 1..=7u8 {
                let bal = corp_rec.division_balance(d);
                if bal.0 > 0 {
                    total_corp_cents = total_corp_cents.saturating_add(bal.0);
                }
                corp_rec.set_division_balance(d, eve_db::IskCents(0));
            }
        }
        if total_corp_cents > 0 {
            if let Some(ch) = snap.characters.get_mut(&tid) {
                ch.balance = eve_db::IskCents(ch.balance.0.saturating_add(total_corp_cents));
                let new_bal = ch.balance;
                snap.append_journal(
                    tid,
                    "corporation_liquidation",
                    eve_db::IskCents(total_corp_cents),
                    new_bal,
                    format!("Corporation {corp_id} closure liquidation to CEO"),
                );
                ceo_liquidation_wallet = Some((tid, new_bal));
            }
        }
        let mut office_to_station: std::collections::HashMap<i64, i64> =
            std::collections::HashMap::new();
        if let Some(g) = snap.corp_governance.remove(&corp_id) {
            for (station_id, office_id) in g.offices {
                office_to_station.insert(office_id, station_id);
            }
        }
        for it in snap.items.values() {
            if it.owner_id == corp_id && it.type_id == 27 {
                office_to_station.entry(it.item_id).or_insert(it.location_id);
            }
        }
        let rehome_ids: Vec<(i64, Option<i64>)> = snap
            .items
            .values()
            .filter(|it| {
                !office_to_station.contains_key(&it.item_id)
                    && (office_to_station.contains_key(&it.location_id) || it.owner_id == corp_id)
                    && !snap.structures.contains_key(&it.item_id)
                    && !(snap.structures.contains_key(&it.location_id)
                        && !matches!(it.flag as u16, 4 | 62 | 116..=122 | 173))
                    && it.flag != eve_db::InventoryFlag::Locked
                    && !snap.is_item_locked_in_contract_or_job(it.item_id)
            })
            .map(|it| (it.item_id, office_to_station.get(&it.location_id).copied()))
            .collect();
        for (item_id, station_opt) in rehome_ids {
            if let Some(pre_it) = snap.items.get(&item_id).cloned() {
                if let Some(it_mut) = snap.items.get_mut(&item_id) {
                    if let Some(station_id) = station_opt {
                        it_mut.location_id = station_id;
                        it_mut.flag = eve_db::InventoryFlag::Deliveries;
                    } else if (116..=122).contains(&(it_mut.flag as u16))
                        || it_mut.flag == eve_db::InventoryFlag::CorpDeliveries
                    {
                        it_mut.flag = eve_db::InventoryFlag::Deliveries;
                    }
                    it_mut.owner_id = tid;
                    it_mut.version += 1;
                    let post_it = it_mut.clone();
                    mutated_by_id
                        .entry(item_id)
                        .and_modify(|e| e.1 = Some(post_it.clone()))
                        .or_insert((Some(pre_it), Some(post_it)));
                }
            }
            for (c_pre, c_post) in snap.set_owner_recursive(item_id, tid) {
                mutated_by_id
                    .entry(c_post.item_id)
                    .and_modify(|e| e.1 = Some(c_post.clone()))
                    .or_insert((Some(c_pre), Some(c_post)));
            }
        }
        for (office_id, _) in office_to_station {
            if let Some(removed_office) = snap.remove_system_cleanup_item(office_id) {
                mutated_by_id
                    .entry(office_id)
                    .and_modify(|e| e.1 = None)
                    .or_insert((Some(removed_office), None));
            }
        }
        snap.corporations.remove(&corp_id);
        snap.chat_channels.remove(&format!("corpid:{corp_id}"));
    }
    let old_corp_id = snap
        .characters
        .get(&tid)
        .map(|c| c.corporation_id)
        .unwrap_or(corp_id);
    if let Some(ch) = snap.characters.get_mut(&tid) {
        ch.corporation_id = npc_corp;
        ch.alliance_id = None;
    }
    let released = record_corp_join_in_snap(snap, tid, old_corp_id, npc_corp, now_unix);
    let mut item_mutations: Vec<_> = mutated_by_id.into_values().collect();
    item_mutations.sort_by_key(|(pre, post)| {
        post.as_ref()
            .or(pre.as_ref())
            .map(|it| it.item_id)
            .unwrap_or(0)
    });
    MemberRemovalOutcome {
        npc_corp,
        old_role,
        old_alliance,
        closed,
        released,
        ceo_liquidation_wallet,
        item_mutations,
    }
}

fn emit_member_removed_notifications(
    ctx: &ServerContext,
    corp_id: i64,
    tid: i64,
    npc_corp: i64,
    old_role: u64,
    old_alliance: Option<i64>,
    now_unix: u64,
) {
    if let Ok(mut extras) = ctx.social_extras.lock() {
        extras.forced_session_corp_sync.insert(tid, (npc_corp, None, 0));
    }
    let m_change = PyRep::Dict(vec![(
        PyRep::String(("corporationID").into()),
        PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(npc_corp)]),
    )]);
    let member_changed = crate::router::notify_registry::OnCorporationMemberChanged {
        corporation_id: corp_id,
        member_id: tid,
        change: m_change,
    };
    ctx.emit_typed(NotificationTarget::Corporation(corp_id), member_changed.clone());
    ctx.emit_typed_with_id_type(
        NotificationTarget::Character(tid),
        "charid",
        member_changed,
    );
    let sess_changes = PyRep::Dict(vec![
        (
            PyRep::String("corpid".into()),
            PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(npc_corp)]),
        ),
        (
            PyRep::String("corprole".into()),
            PyRep::Tuple(vec![PyRep::Integer(old_role as i64), PyRep::Integer(0)]),
        ),
        (
            PyRep::String("allianceid".into()),
            PyRep::Tuple(vec![
                old_alliance.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::None,
            ]),
        ),
    ]);
    ctx.emit_typed_with_id_type(
        NotificationTarget::Character(tid),
        "clientID",
        crate::router::notify_registry::OnSessionChanged {
            is_remote: false,
            session: PyRep::Integer(tid),
            change: sess_changes,
        },
    );
    let _ = crate::services::quasar_social_chat::on_character_local_chat_membership_refreshed(
        &ctx.quasar_gateway,
        &ctx.db,
        tid as u32,
        now_unix,
    );
    let tid_sys = ctx
        .db
        .with_snapshot(|s| s.characters.get(&tid).map(|c| c.solar_system_id))
        .unwrap_or(0);
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        tid,
        now_unix,
        tid_sys,
        0,
        "corp_change",
    );
}

pub(crate) fn handle_kick_out_members(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session.corporation_id.unwrap_or(0);
    let target_ids: Vec<i64> = match args.first() {
        Some(PyRep::Integer(id)) => vec![*id],
        Some(PyRep::List(items) | PyRep::Tuple(items)) => {
            let v: Vec<i64> = items.iter().filter_map(|x| x.as_i64()).collect();
            if v.is_empty() { vec![char_id] } else { v }
        }
        _ => vec![char_id],
    };
    let now_unix = ctx.now_unix();
    if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(
        ctx, char_id, now_unix, None,
    ) {
        return Some(Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot perform corp member kick while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
            client_error: None,
        }));
    }
    for &tid in &target_ids {
        if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(
            ctx, tid, now_unix, None,
        ) {
            return Some(Err(BridgeRpcError {
                code: "SessionChangeInProgress".into(),
                message: format!(
                    "Cannot kick member while 10s session-change timer ({}) is active",
                    active_timer.reason
                ),
                client_error: None,
            }));
        }
    }
    promote_due_pending_corp_grants(ctx, corp_id);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
                client_error: None,
            });
        };
        let (is_ceo, eff_caller_role) =
            crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                snap, corp_id, char_id, 0, now_unix,
            );
        let any_other = target_ids.iter().any(|&tid| tid != char_id);
        if any_other
            && !is_ceo
            && (eff_caller_role & (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER)) == 0
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Kicking a member requires Personnel Manager or Director role".into(),
                client_error: None,
            });
        }
        let caller_sess_in_space = session.station_id.is_none()
            && session.structure_id.is_none()
            && session.ship_id.is_some();
        for &tid in &target_ids {
            if !corp.members.contains(&tid) {
                return Err(BridgeRpcError {
                    code: "NotCorpMember".into(),
                    message: format!("Character {tid} is not a member of corporation {corp_id}"),
                    client_error: None,
                });
            }
            if tid == corp.ceo_id && corp.members.len() > 1 {
                return Err(BridgeRpcError {
                    code: "CeoCannotLeave".into(),
                    message: "CEO cannot leave while other members remain in the corporation".into(),
                    client_error: None,
                });
            }
            if tid == char_id {
                let ch_undocked = snap.characters.get(&tid).is_some_and(|ch| {
                    ch.station_id.is_none() && ch.structure_id.is_none()
                        && session.station_id.is_none() && session.structure_id.is_none()
                });
                if caller_sess_in_space || ch_undocked {
                    return Err(BridgeRpcError {
                        code: "MustBeDocked".into(),
                        message: "You must be docked in a station or structure to leave a corporation".into(),
                        client_error: None,
                    });
                }
            }
            let solo_ceo_closing = tid == corp.ceo_id && corp.members.len() == 1;
            if !solo_ceo_closing {
                if let Some(blocker) = member_kick_blocker(snap, corp_id, tid, now_unix) {
                    return Err(BridgeRpcError {
                        code: blocker.into(),
                        message: format!(
                            "Cannot leave or expel member {tid}: roles/titles must be dropped and 24h must pass ({blocker})"
                        ),
                        client_error: None,
                    });
                }
            }
        }
        let solo_ceo_closing =
            corp_id >= 2_000_000 && corp.members.len() == 1 && target_ids.contains(&corp.ceo_id);
        let has_active_war = snap.wars.values().any(|w| {
            w.active
                && (w.aggressor_corp_id == corp_id
                    || w.defender_corp_id == corp_id
                    || corp.alliance_id.is_some_and(|aid| {
                        w.aggressor_corp_id == aid || w.defender_corp_id == aid
                    }))
        });
        if solo_ceo_closing && has_active_war {
            return Err(BridgeRpcError {
                code: "ActiveWarsPreventClosure".into(),
                message: "Corporation cannot close while active wars remain".into(),
                client_error: None,
            });
        }
        if solo_ceo_closing {
            let has_active_structure = snap
                .structures
                .values()
                .any(|st| st.owner_corp_id == corp_id && st.state != "Destroyed");
            if has_active_structure {
                return Err(BridgeRpcError {
                    code: "ActiveStructuresPreventClosure".into(),
                    message: "Corporation cannot close while active owned structures remain"
                        .into(),
                    client_error: None,
                });
            }
            let has_open_corp_contract = snap.contracts.values().any(|c| {
                matches!(c.status.as_str(), "Outstanding" | "InProgress")
                    && ((c.for_corp && c.issuer_corp_id == corp_id)
                        || c.acceptor_id == Some(corp_id))
            });
            let has_open_corp_job = snap.industry_jobs.values().any(|j| {
                matches!(j.status.as_str(), "Active" | "Paused")
                    && (j.installer_id == corp_id
                        || snap
                            .items
                            .get(&j.blueprint_item_id)
                            .is_some_and(|b| b.owner_id == corp_id)
                        || snap
                            .items
                            .get(&j.blueprint_location_id)
                            .is_some_and(|loc| loc.owner_id == corp_id && loc.type_id == 27)
                        || snap
                            .items
                            .get(&j.output_location_id)
                            .is_some_and(|loc| loc.owner_id == corp_id && loc.type_id == 27))
            });
            if has_open_corp_contract || has_open_corp_job {
                return Err(BridgeRpcError {
                    code: "OpenCorpContractsOrJobsPreventClosure".into(),
                    message: "Corporation cannot close while open corporate contracts or industry jobs remain".into(),
                    client_error: None,
                });
            }
        }

        let mut removed_members: Vec<(i64, MemberRemovalOutcome)> = Vec::new();
        let mut queued_members: Vec<i64> = Vec::new();
        let mut released_controllers = Vec::new();
        let mut closed = false;
        for &tid in &target_ids {
            let in_space = snap
                .characters
                .get(&tid)
                .is_some_and(|ch| ch.station_id.is_none() && ch.structure_id.is_none());
            if tid != char_id && in_space {
                snap.corp_governance
                    .entry(corp_id)
                    .or_default()
                    .pending_auto_kicks
                    .insert(tid, now_unix);
                queued_members.push(tid);
                continue;
            }
            let outcome =
                remove_member_to_bloodline_npc_in_snap(ctx, snap, corp_id, tid, now_unix);
            if outcome.closed {
                closed = true;
            }
            released_controllers.extend(outcome.released.iter().copied());
            removed_members.push((tid, outcome));
        }
        Ok((removed_members, queued_members, closed, released_controllers))
    });

    match res {
        Ok((removed_members, queued_members, closed, released_controllers)) => {
            crate::services::structure_control_ops::notify_released_structure_controllers(
                ctx,
                &released_controllers,
            );
            for (tid, outcome) in &removed_members {
                let tid = *tid;
                let npc_corp = outcome.npc_corp;
                let (act_ship, m_sys, m_dock) = ctx.db.with_snapshot(|s| {
                    s.characters
                        .get(&tid)
                        .map(|c| (c.active_ship_id, c.solar_system_id, c.station_id.or(c.structure_id)))
                        .unwrap_or((0, session.solar_system_id.unwrap_or(0), session.station_id.or(session.structure_id)))
                });
                if tid == char_id {
                    session.corporation_id = Some(npc_corp);
                    session.alliance_id = None;
                    session.corp_role = 0;
                    if act_ship != 0 {
                        let _ = ctx.execute_session_transition_sync(
                            Some(session),
                            crate::router::SessionTransitionSpec::for_kind(
                                tid,
                                crate::router::TransitionKind::BoardShip { ship_id: act_ship },
                                "corp_change",
                            )
                            .with_apply_db(false)
                            .with_emit_on_session_changed(false),
                        );
                    }
                }
                if corp_id != npc_corp {
                    ctx.invalidate_bound_objects_for_session_transition(
                        tid,
                        m_sys,
                        m_dock,
                        "corp_change",
                    );
                }
                emit_member_removed_notifications(
                    ctx,
                    corp_id,
                    tid,
                    npc_corp,
                    outcome.old_role,
                    outcome.old_alliance,
                    now_unix,
                );
                emit_closure_liquidation_notifications(
                    ctx,
                    tid,
                    outcome.ceo_liquidation_wallet,
                    &outcome.item_mutations,
                );
            }
            let first_tid = target_ids[0];
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(first_tid)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("removed", PyRep::Bool(!removed_members.is_empty())),
                ("kickedCount", PyRep::Integer(removed_members.len() as i64)),
                ("queuedAutoKick", PyRep::Bool(!queued_members.is_empty())),
                ("corporationClosed", PyRep::Bool(closed)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

/// Executes any pending auto-kick for `char_id` when they dock (`feat:lifecycle.corp-autokick-queue`).
pub fn execute_pending_auto_kicks_for_docked_char(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
) {
    let now_unix = ctx.now_unix();
    let executed = ctx.db.with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return None;
        };
        if ch.station_id.is_none() && ch.structure_id.is_none() {
            return None;
        }
        let corp_id = ch.corporation_id;
        let is_queued = snap
            .corp_governance
            .get(&corp_id)
            .is_some_and(|g| g.pending_auto_kicks.contains_key(&char_id));
        if !is_queued {
            return None;
        }
        let outcome =
            remove_member_to_bloodline_npc_in_snap(ctx, snap, corp_id, char_id, now_unix);
        Some((corp_id, outcome))
    });
    if let Some((corp_id, outcome)) = executed {
        let npc_corp = outcome.npc_corp;
        crate::services::structure_control_ops::notify_released_structure_controllers(
            ctx,
            &outcome.released,
        );
        if session.character_id == Some(char_id) {
            session.corporation_id = Some(npc_corp);
            session.alliance_id = None;
            session.corp_role = 0;
            if let Some(act_ship) = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id)) {
                if act_ship != 0 {
                    let _ = ctx.execute_session_transition_sync(
                        Some(session),
                        crate::router::SessionTransitionSpec::for_kind(
                            char_id,
                            crate::router::TransitionKind::BoardShip { ship_id: act_ship },
                            "corp_change",
                        )
                        .with_apply_db(false)
                        .with_emit_on_session_changed(false),
                    );
                }
            }
        }
        if corp_id != npc_corp {
            let loc = ctx.db.with_snapshot(|s| {
                s.characters
                    .get(&char_id)
                    .map(|c| (c.solar_system_id, c.station_id.or(c.structure_id)))
            });
            if let Some((c_sys, c_dock)) = loc {
                ctx.invalidate_bound_objects_for_session_transition(
                    char_id,
                    c_sys,
                    c_dock,
                    "corp_change",
                );
            }
        }
        emit_member_removed_notifications(
            ctx,
            corp_id,
            char_id,
            npc_corp,
            outcome.old_role,
            outcome.old_alliance,
            now_unix,
        );
        emit_closure_liquidation_notifications(
            ctx,
            char_id,
            outcome.ceo_liquidation_wallet,
            &outcome.item_mutations,
        );
    }
}

/// Executes all queued corporation `pending_auto_kicks` at daily downtime (`feat:lifecycle.corp-autokick-queue`).
pub fn execute_pending_auto_kicks_on_downtime(ctx: &ServerContext) -> usize {
    let now_unix = ctx.now_unix();
    let (removed, released_controllers) = ctx.db.with_mut_snapshot(|snap| {
        let mut due: Vec<(i64, i64)> = Vec::new();
        for (&corp_id, g) in &snap.corp_governance {
            for &tid in g.pending_auto_kicks.keys() {
                due.push((corp_id, tid));
            }
        }
        let mut out = Vec::new();
        let mut released_controllers = Vec::new();
        for (corp_id, tid) in due {
            if snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(&tid)) {
                let outcome =
                    remove_member_to_bloodline_npc_in_snap(ctx, snap, corp_id, tid, now_unix);
                released_controllers.extend(outcome.released.iter().copied());
                out.push((corp_id, tid, outcome));
            } else if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
                g.pending_auto_kicks.remove(&tid);
            }
        }
        (out, released_controllers)
    });
    crate::services::structure_control_ops::notify_released_structure_controllers(
        ctx,
        &released_controllers,
    );
    let count = removed.len();
    for (corp_id, tid, outcome) in removed {
        emit_member_removed_notifications(
            ctx,
            corp_id,
            tid,
            outcome.npc_corp,
            outcome.old_role,
            outcome.old_alliance,
            now_unix,
        );
        emit_closure_liquidation_notifications(
            ctx,
            tid,
            outcome.ceo_liquidation_wallet,
            &outcome.item_mutations,
        );
    }
    count
}

pub(super) fn handle_insert_vote_case(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (corp_id, args) = if let (Some(arg_cid), Some(_)) = (
        args.first().and_then(|a| a.as_i64()),
        args.get(1).and_then(|a| a.as_str()),
    ) {
        (arg_cid, &args[1..])
    } else {
        (corp_id, args)
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
            let Some(corp) = snap.corporations.get(&corp_id) else {
                return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found"), client_error: None });
            };
            if !corp.members.contains(&char_id) {
                return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Caller is not a member of the corporation".into(), client_error: None });
            }
            let (is_ceo, caller_role) =
                snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
            let is_ceo_or_dir = is_ceo || corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR);
            if vote_type == 0 {
                let caller_shares = snap.corp_governance.get(&corp_id).and_then(|g| g.shares.get(&char_id).copied()).unwrap_or(0);
                if !is_ceo_or_dir && caller_shares <= 0 {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Proposing a CEO election requires holding corporation shares or Director role".into(), client_error: None });
                }
            } else if !is_ceo_or_dir {
                return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Inserting a corporation vote case requires CEO or Director role".into(), client_error: None });
            }
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
                            if vote_type == 0
                                && !snap
                                    .corporations
                                    .get(&corp_id)
                                    .is_some_and(|c| c.members.contains(cand_id))
                            {
                                return Err(BridgeRpcError {
                                    code: "CandidateNotMember".into(),
                                    message: "CEO election candidate must be a member of the corporation".into(),
                                    client_error: None,
                                });
                            }
                            let cname = snap
                                .characters
                                .get(cand_id)
                                .map(|c| c.name.clone())
                                .unwrap_or_else(|| format!("Candidate {cand_id}"));
                            options.push((oid, cname, *cand_id));
                        }
                        PyRep::List(els) | PyRep::Tuple(els) => {
                            let (opt_id, otext, param) =
                                if els.first().and_then(|x| x.as_str()).is_some() {
                                    (
                                        oid,
                                        els.first().and_then(|x| x.as_str()).unwrap_or("Option").to_string(),
                                        els.get(1).and_then(|x| x.as_i64()).unwrap_or(0),
                                    )
                                } else {
                                    let id = els.first().and_then(|x| x.as_i64()).unwrap_or(oid);
                                    let txt = els.get(1).and_then(|x| x.as_str()).unwrap_or("Option").to_string();
                                    let p = els.get(2).and_then(|x| x.as_i64()).unwrap_or(id);
                                    (id, txt, p)
                                };
                            if vote_type == 0
                                && param > 0
                                && !snap
                                    .corporations
                                    .get(&corp_id)
                                    .is_some_and(|c| c.members.contains(&param))
                            {
                                return Err(BridgeRpcError {
                                    code: "CandidateNotMember".into(),
                                    message: "CEO election candidate must be a member of the corporation".into(),
                                    client_error: None,
                                });
                            }
                            options.push((opt_id, otext, param));
                        }
                        PyRep::String(s) => options.push((oid, s.clone(), oid)),
                        _ => {}
                    }
                }
            }
            let target_id = options
                .first()
                .map(|o| o.2)
                .or_else(|| if args.len() >= 5 { args.get(4).and_then(|a| a.as_i64()) } else { None })
                .or_else(|| args.get(3).and_then(|a| a.as_i64()))
                .unwrap_or(char_id);
            if vote_type == 5 && (target_id <= 0 || target_id > 10_000_000_000) {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Share creation amount must be positive and bounded".into(),
                    client_error: None,
                });
            }
            if options.is_empty() {
                if vote_type == 0 {
                    if !snap
                        .corporations
                        .get(&corp_id)
                        .is_some_and(|c| c.members.contains(&target_id))
                    {
                        return Err(BridgeRpcError {
                            code: "CandidateNotMember".into(),
                            message: "CEO election candidate must be a member of the corporation".into(),
                            client_error: None,
                        });
                    }
                    let cname = snap
                        .characters
                        .get(&target_id)
                        .map(|c| c.name.clone())
                        .unwrap_or_else(|| format!("Candidate {target_id}"));
                    options = vec![
                        (1, format!("Vote for {cname}"), target_id),
                        (2, "Reject".into(), 0),
                    ];
                } else {
                    options = vec![
                        (1, "Yes / Accept".into(), target_id),
                        (2, "No / Reject".into(), 0),
                    ];
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
    ctx.emit_typed(
        NotificationTarget::Corporation(corp_id),
        crate::router::notify_registry::OnVoteCaseChanged {
            corporation_id: corp_id,
            vote_case_id: case_id,
        },
    );
    ctx.emit_typed(
        NotificationTarget::Corporation(corp_id),
        crate::router::notify_registry::OnSanctionedActionChanged {
            corporation_id: corp_id,
            vote_case_id: case_id,
        },
    );
    Some(Ok(PyRep::keyval([
        ("voteCaseID", PyRep::Integer(case_id)),
        ("voteType", PyRep::Integer(vote_type)),
        ("parameter", PyRep::Integer(target_id)),
    ])))
}

#[cfg(test)]
#[path = "fleet_social_corp_tail_tests.rs"]
pub(crate) mod fleet_social_corp_tail_tests;

#[cfg(test)]
#[path = "fleet_social_corp_tail_edges_tests.rs"]
mod fleet_social_corp_tail_edges_tests;

