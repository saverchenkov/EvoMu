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
            || g.pending_scoped_role_grants.contains_key(&target_id)
        {
            return Some("MemberHasRoles");
        }
        if let Some(&tmask) = g.member_titles.get(&target_id) {
            if tmask != 0 {
                for (&tid, &(_, troles)) in &g.titles {
                    let bit = 1u64 << ((tid - 1).clamp(0, 62) as u64);
                    let alt_bit = 1u64 << (tid.rem_euclid(16) as u64);
                    if ((tmask & bit) != 0 || (tmask & alt_bit) != 0) && troles != 0 {
                        return Some("MemberHasRoleGrantingTitle");
                    }
                }
                if let Some(titles) = snap.corp_titles.get(&corp_id) {
                    for t in titles {
                        let bit = 1u64 << ((t.title_id - 1).clamp(0, 62) as u64);
                        if (tmask & bit) != 0
                            && (!t.roles.is_empty()
                                || !t.grantable_roles.is_empty()
                                || !t.roles_at_hq.is_empty()
                                || !t.roles_at_base.is_empty()
                                || !t.roles_at_other.is_empty())
                        {
                            return Some("MemberHasRoleGrantingTitle");
                        }
                    }
                }
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

fn title_mask_grants_roles(snap: &eve_db::WorldStateSnapshot, corp_id: i64, tmask: u64) -> bool {
    if tmask == 0 {
        return false;
    }
    if let Some(g) = snap.corp_governance.get(&corp_id) {
        for (&tid, &(_, troles)) in &g.titles {
            let bit = 1u64 << ((tid - 1).clamp(0, 62) as u64);
            let alt_bit = 1u64 << (tid.rem_euclid(16) as u64);
            if ((tmask & bit) != 0 || (tmask & alt_bit) != 0) && troles != 0 {
                return true;
            }
        }
    }
    if let Some(titles) = snap.corp_titles.get(&corp_id) {
        for t in titles {
            let bit = 1u64 << ((t.title_id - 1).clamp(0, 62) as u64);
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
        if matches!(r.get(1), Some(PyRep::String(_))) {
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
        }));
    }
    if specs.iter().any(|sp| sp.has_negative_mask) {
        return Some(Err(BridgeRpcError::user_error("CrpAccessDenied")));
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
        let is_ceo = char_id == corp.ceo_id;
        let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        if !is_ceo && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Only CEO or Director can update corporation roles".into(),
            });
        }
        for sp in &specs {
            if !corp.members.contains(&sp.target_char_id) {
                return Err(BridgeRpcError {
                    code: "TargetNotInCorp".into(),
                    message: format!("Character {} is not a member of corporation {corp_id}", sp.target_char_id),
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
                    return Err(BridgeRpcError::user_error("CrpAccessDenied"));
                }
            }
        }

        let dec = |m: u64| -> Vec<String> {
            crate::esi::corp_governance_routes::decode_corp_roles(m)
                .into_iter()
                .map(String::from)
                .collect()
        };
        const HANGAR_DIV_MASK: u64 = ((1u64 << 14) - 1) << 13;
        let split_hangar = |old_m: u64, prev_p: u64, opt_m: Option<u64>| -> (u64, u64, u64, u64) {
            let new_m = opt_m.unwrap_or(old_m | prev_p);
            let (held, desired) = (old_m & HANGAR_DIV_MASK, new_m & HANGAR_DIV_MASK);
            let newly = desired & !held & !prev_p;
            let pend = if opt_m.is_some() { newly | (desired & prev_p) } else { prev_p };
            let eff = if opt_m.is_some() { (new_m & !HANGAR_DIV_MASK) | (desired & held) } else { old_m };
            (new_m, eff, pend, newly)
        };
        let mut results: Vec<(i64, u64, u64, u64, bool)> = Vec::new();
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
            let (_, eff_hq, pend_hq, newly_hq) =
                split_hangar(old_ra.roles_at_hq, prev_hq_p, sp.roles_at_hq_opt);
            let (_, eff_base, pend_base, newly_base) =
                split_hangar(old_ra.roles_at_base, prev_base_p, sp.roles_at_base_opt);
            let (_, eff_other, pend_other, newly_other) =
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
            if let Some(tm) = sp.title_mask_opt {
                let mut new_title_hangar = 0u64;
                for (&tid, &(_, troles)) in &state.titles {
                    let bit = 1u64 << ((tid - 1).clamp(0, 62) as u64);
                    if (tm & bit) != 0 || tm == tid as u64 {
                        new_title_hangar |= troles & HANGAR_DIV_MASK;
                    }
                }
                let newly_title_hangar = new_title_hangar & !(old_roles & HANGAR_DIV_MASK);
                if newly_title_hangar != 0 {
                    state.pending_role_grants.insert(
                        target_char_id,
                        (effective_roles | pending_hangar | newly_title_hangar, now_unix + 86_400),
                    );
                }
                state.member_titles.insert(target_char_id, tm);
                let mut tids: Vec<i64> = (0..16u64)
                    .filter(|&b| (tm & (1u64 << b)) != 0)
                    .map(|b| (b + 1) as i64)
                    .collect();
                if tids.is_empty() && tm > 0 {
                    tids.push(tm as i64);
                }
                snap.corp_member_titles.insert((corp_id, target_char_id), tids);
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
            snap.corp_role_history.entry(corp_id).or_default().push(
                eve_db::snapshot_ext::CorpRoleHistoryEntry {
                    character_id: target_char_id,
                    changed_at_unix: now_unix,
                    issuer_id: char_id,
                    role_type: "roles".into(),
                    old_roles: dec(old_roles),
                    new_roles: dec(new_roles),
                },
            );
            let delayed = (pending_hangar | pend_hq | pend_base | pend_other) != 0 || new_roles > old_roles;
            results.push((target_char_id, old_roles, new_roles, effective_roles, delayed));
        }
        Ok(results)
    });

    match res {
        Ok(results) => {
            for &(tid, old_roles, new_roles, effective_roles, _) in &results {
                if session.character_id == Some(tid) {
                    session.corp_role = effective_roles;
                }
                let change = PyRep::Dict(vec![(
                    PyRep::String(("roles").into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(old_roles as i64),
                        PyRep::Integer(new_roles as i64),
                    ]),
                )]);
                ctx.notify(
                    NotificationTarget::Corporation(corp_id),
                    "OnCorporationMemberChanged",
                    "corpid",
                    vec![PyRep::Integer(corp_id), PyRep::Integer(tid), change],
                );
            }
            let &(first_tid, _, first_new, _, delayed) = &results[0];
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

fn remove_member_to_bloodline_npc_in_snap(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    tid: i64,
    now_unix: u64,
) -> (i64, u64, Option<i64>, bool) {
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
    if let Some(corp_mut) = snap.corporations.get_mut(&corp_id) {
        corp_mut.members.remove(&tid);
        corp_mut.member_roles.remove(&tid);
        if corp_mut.members.is_empty() {
            closed = true;
        }
    }
    if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
        g.pending_role_grants.remove(&tid);
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
    record_corp_join_in_snap(snap, tid, old_corp_id, npc_corp, now_unix);
    (npc_corp, old_role, old_alliance, closed)
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
    let m_args = vec![PyRep::Integer(corp_id), PyRep::Integer(tid), m_change];
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnCorporationMemberChanged",
        "corpid",
        m_args.clone(),
    );
    ctx.notify(
        NotificationTarget::Character(tid),
        "OnCorporationMemberChanged",
        "charid",
        m_args,
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
    ctx.notify(
        NotificationTarget::Character(tid),
        "OnSessionChanged",
        "clientID",
        vec![PyRep::Bool(false), PyRep::Integer(tid), sess_changes],
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
            }));
        }
    }
    promote_due_pending_corp_grants(ctx, corp_id);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
            });
        };
        let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        let any_other = target_ids.iter().any(|&tid| tid != char_id);
        if any_other
            && char_id != corp.ceo_id
            && !corp_roles::has_role(
                caller_role,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER,
            )
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Kicking a member requires Personnel Manager or Director role".into(),
            });
        }
        for &tid in &target_ids {
            if !corp.members.contains(&tid) {
                return Err(BridgeRpcError {
                    code: "NotCorpMember".into(),
                    message: format!("Character {tid} is not a member of corporation {corp_id}"),
                });
            }
            if tid == corp.ceo_id && corp.members.len() > 1 {
                return Err(BridgeRpcError {
                    code: "CeoCannotLeave".into(),
                    message: "CEO cannot leave while other members remain in the corporation".into(),
                });
            }
            if tid != char_id {
                if let Some(blocker) = member_kick_blocker(snap, corp_id, tid, now_unix) {
                    return Err(BridgeRpcError {
                        code: blocker.into(),
                        message: format!(
                            "Cannot expel member {tid}: roles/titles must be dropped and 24h must pass ({blocker})"
                        ),
                    });
                }
            }
        }
        let has_active_war = snap.wars.values().any(|w| {
            w.active
                && (w.aggressor_corp_id == corp_id
                    || w.defender_corp_id == corp_id
                    || corp.alliance_id.is_some_and(|aid| {
                        w.aggressor_corp_id == aid || w.defender_corp_id == aid
                    }))
        });
        if corp.members.len() == 1 && target_ids.contains(&corp.ceo_id) && has_active_war {
            return Err(BridgeRpcError {
                code: "ActiveWarsPreventClosure".into(),
                message: "Corporation cannot close while active wars remain".into(),
            });
        }

        let mut removed_members: Vec<(i64, i64, u64, Option<i64>)> = Vec::new();
        let mut queued_members: Vec<i64> = Vec::new();
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
            let (npc_corp, old_role, old_alliance, corp_closed) =
                remove_member_to_bloodline_npc_in_snap(ctx, snap, corp_id, tid, now_unix);
            if corp_closed {
                closed = true;
            }
            removed_members.push((tid, npc_corp, old_role, old_alliance));
        }
        Ok((removed_members, queued_members, closed))
    });

    match res {
        Ok((removed_members, queued_members, closed)) => {
            for &(tid, npc_corp, old_role, old_alliance) in &removed_members {
                if tid == char_id {
                    session.corporation_id = Some(npc_corp);
                    session.alliance_id = None;
                    session.corp_role = 0;
                }
                emit_member_removed_notifications(
                    ctx,
                    corp_id,
                    tid,
                    npc_corp,
                    old_role,
                    old_alliance,
                    now_unix,
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
        let (npc_corp, old_role, old_alliance, _) =
            remove_member_to_bloodline_npc_in_snap(ctx, snap, corp_id, char_id, now_unix);
        Some((corp_id, npc_corp, old_role, old_alliance))
    });
    if let Some((corp_id, npc_corp, old_role, old_alliance)) = executed {
        if session.character_id == Some(char_id) {
            session.corporation_id = Some(npc_corp);
            session.alliance_id = None;
            session.corp_role = 0;
        }
        emit_member_removed_notifications(
            ctx,
            corp_id,
            char_id,
            npc_corp,
            old_role,
            old_alliance,
            now_unix,
        );
    }
}

/// Executes all queued corporation `pending_auto_kicks` at daily downtime (`feat:lifecycle.corp-autokick-queue`).
pub fn execute_pending_auto_kicks_on_downtime(ctx: &ServerContext) -> usize {
    let now_unix = ctx.now_unix();
    let removed = ctx.db.with_mut_snapshot(|snap| {
        let mut due: Vec<(i64, i64)> = Vec::new();
        for (&corp_id, g) in &snap.corp_governance {
            for &tid in g.pending_auto_kicks.keys() {
                due.push((corp_id, tid));
            }
        }
        let mut out = Vec::new();
        for (corp_id, tid) in due {
            if snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(&tid)) {
                let (npc_corp, old_role, old_alliance, _) =
                    remove_member_to_bloodline_npc_in_snap(ctx, snap, corp_id, tid, now_unix);
                out.push((corp_id, tid, npc_corp, old_role, old_alliance));
            } else if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
                g.pending_auto_kicks.remove(&tid);
            }
        }
        out
    });
    let count = removed.len();
    for (corp_id, tid, npc_corp, old_role, old_alliance) in removed {
        emit_member_removed_notifications(
            ctx,
            corp_id,
            tid,
            npc_corp,
            old_role,
            old_alliance,
            now_unix,
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
                return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
            };
            if !corp.members.contains(&char_id) {
                return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Caller is not a member of the corporation".into() });
            }
            let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
            let is_ceo_or_dir = char_id == corp.ceo_id || corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR);
            if vote_type == 0 {
                let caller_shares = snap.corp_governance.get(&corp_id).and_then(|g| g.shares.get(&char_id).copied()).unwrap_or(0);
                if !is_ceo_or_dir && caller_shares <= 0 {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Proposing a CEO election requires holding corporation shares or Director role".into() });
                }
            } else if !is_ceo_or_dir {
                return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Inserting a corporation vote case requires CEO or Director role".into() });
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

