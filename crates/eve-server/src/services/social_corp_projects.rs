//! `social_corp_projects` — Corporation Goals, Projects, Reward Pools, Expiry & Freelance Jobs (`SO-3`).

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, CorpProjectRecord, InventoryFlag, IskCents, PersistedProjectDetails,
    WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub const SERVER_TRACKED_MATCHERS: &[&str] = &[
    "DamageNPCShips",
    "DestroyNPCShips",
    "DestroyCapsuleerShips",
    "MineMaterials",
    "ManufactureItems",
    "CaptureFWComplexes",
    "DefendFWComplexes",
    "DamageShips",
    "RemoteRepairShield",
    "RemoteRepairArmor",
    "ScanSignatures",
    "SalvageWrecks",
    "EarnLoyaltyPoints",
];

/// Delayed 24h division access/take role bits: hangar divisions (`13..=26`) + `ROLE_ACCOUNT_CAN_TAKE` (`27..=33`).
pub(crate) const HANGAR_DIV_MASK: u64 = eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK;

/// Computes `(is_ceo, effective_roles)` for `char_id` in `corp_id` from DB corporation state
/// (`corp.member_roles`, `corp_role_assignments`, and 1-based title bitmasks `1 << (tid - 1)`),
/// masking unmatured 24h `pending_role_grants` and `pending_title_role_grants` on `HANGAR_DIV_MASK`.
pub(crate) fn effective_corp_member_roles(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    session_roles: u64,
    now_unix: u64,
) -> (bool, u64) {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return (false, session_roles);
    };
    let is_ceo = corp.ceo_id == char_id;
    let mut roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
        roles |= ra.roles;
    }
    if let Some(gov) = snap.corp_governance.get(&corp_id) {
        if let Some(&(p_mask, matured_at)) = gov.pending_role_grants.get(&char_id) {
            if now_unix >= matured_at {
                roles |= p_mask;
            }
        }
        let unmatured_title_bits = gov
            .pending_title_role_grants
            .get(&char_id)
            .filter(|&&(_, matured_at)| now_unix < matured_at)
            .map_or(0, |&(bits, _)| bits & HANGAR_DIV_MASK);
        if let Some(&tmask) = gov.member_titles.get(&char_id) {
            for (&tid, &(_, troles)) in &gov.titles {
                if (1..=64).contains(&tid) && (tmask & (1u64 << (tid - 1))) != 0 {
                    roles |= troles & !unmatured_title_bits;
                }
            }
        }
    }
    (is_ceo, roles)
}

pub fn sweep_expired_corp_projects_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
) -> Vec<(i64, i64)> {
    let mut expired_list = Vec::new();
    let expired_pids: Vec<i64> = snap
        .alliance_war_state
        .project_details
        .iter()
        .filter_map(|(&pid, det)| {
            (det.status == "Active" && det.expires_unix.is_some_and(|exp| now_unix >= exp))
                .then_some(pid)
        })
        .collect();
    for pid in expired_pids {
        let div_id = snap
            .alliance_war_state
            .project_details
            .get_mut(&pid)
            .map(|det| {
                det.status = "Expired".into();
                det.wallet_division.clamp(1, 7)
            })
            .unwrap_or(1);
        if let Some(proj) = snap.corp_projects.get_mut(&pid) {
            let refund = std::mem::replace(&mut proj.escrow_remaining, IskCents::ZERO);
            let corp_id = proj.corporation_id;
            if refund.raw_cents() > 0 {
                let mut bal_after = IskCents::ZERO;
                if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                    corp.ensure_wallet_divisions();
                    if let Some(div) = corp
                        .wallet_divisions
                        .iter_mut()
                        .find(|d| d.division_id == div_id)
                    {
                        div.balance = div.balance.checked_add(refund).unwrap_or(div.balance);
                        bal_after = div.balance;
                    }
                    if div_id == 1 {
                        corp.master_wallet = corp
                            .master_wallet
                            .checked_add(refund)
                            .unwrap_or(corp.master_wallet);
                        bal_after = corp.master_wallet;
                    }
                }
                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: jid,
                    character_id: corp_id,
                    entry_type: "CorporationProjectRefund".into(),
                    amount: refund,
                    balance_after: bal_after,
                    reason: format!("Project {pid} escrow refund on expiry"),
                });
                snap.wallet_journal_timestamps.insert(jid, now_unix);
                let next_cjid = snap
                    .corp_wallet_journals
                    .values()
                    .flatten()
                    .map(|j| j.id)
                    .max()
                    .unwrap_or(500_000)
                    + 1;
                snap.corp_wallet_journals
                    .entry((corp_id, div_id as i64))
                    .or_default()
                    .push(eve_db::snapshot_ext::CorpWalletJournalRecord {
                        id: next_cjid,
                        date_unix: now_unix,
                        ref_type: "CorporationProjectRefund".into(),
                        description: format!("Project {pid} escrow refund on expiry"),
                        amount: refund.to_f64_isk(),
                        balance: bal_after.to_f64_isk(),
                        first_party_id: Some(corp_id),
                        second_party_id: None,
                        reason: Some("Corporation project escrow refund".into()),
                    });
            }
            expired_list.push((pid, corp_id));
        }
    }
    expired_list
}

pub fn sweep_expired_corp_projects(ctx: &ServerContext) {
    let now_unix = ctx.now_unix();
    let expired = ctx
        .db
        .with_mut_snapshot(|snap| sweep_expired_corp_projects_in_snap(snap, now_unix));
    for (pid, corp_id) in expired {
        ctx.notify(
            NotificationTarget::Corporation(corp_id),
            "OnGoalDeleted",
            "corpid",
            vec![PyRep::Integer(pid)],
        );
    }
}

pub fn record_server_project_activity_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    matcher_types: &[&str],
    qty: u64,
    filter_opt: Option<i64>,
    now_unix: u64,
) -> Vec<(i64, u64, bool)> {
    if qty == 0 || char_id <= 0 {
        return Vec::new();
    }
    let _ = sweep_expired_corp_projects_in_snap(snap, now_unix);
    let char_corp_id = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let candidate_pids: Vec<i64> = snap
        .corp_projects
        .values()
        .filter_map(|p| {
            let (matcher, is_fl, _) = snap
                .alliance_war_state
                .project_meta
                .get(&p.project_id)
                .cloned()
                .unwrap_or_else(|| ("DeliverItem".into(), false, false));
            let det = snap
                .alliance_war_state
                .project_details
                .get(&p.project_id)
                .cloned()
                .unwrap_or_default();
            if det.status != "Active" || p.current_quantity >= p.target_quantity {
                return None;
            }
            if p.corporation_id != char_corp_id && !is_fl {
                return None;
            }
            if !matcher_types
                .iter()
                .any(|m| m.eq_ignore_ascii_case(&matcher))
            {
                return None;
            }
            if let Some(req_f) = det.filter_id.filter(|&f| f > 0) {
                if filter_opt != Some(req_f) {
                    return None;
                }
            }
            Some(p.project_id)
        })
        .collect();

    let mut updates = Vec::new();
    for pid in candidate_pids {
        let Some(proj) = snap.corp_projects.get(&pid).cloned() else {
            continue;
        };
        let det = snap
            .alliance_war_state
            .project_details
            .entry(pid)
            .or_default();
        let prev_char_qty = det.char_contributions.get(&char_id).copied().unwrap_or(0);
        let char_rem = det
            .max_per_character
            .map_or(u64::MAX, |cap| cap.saturating_sub(prev_char_qty));
        let proj_rem = proj.target_quantity.saturating_sub(proj.current_quantity);
        let accepted = qty.min(proj_rem).min(char_rem);
        if accepted == 0 {
            continue;
        }
        *det.char_contributions.entry(char_id).or_insert(0) += accepted;
        let reward = proj
            .isk_per_unit
            .checked_mul_qty(accepted as i64)
            .unwrap_or(IskCents::ZERO)
            .min(proj.escrow_remaining);
        let mut completed = false;
        let mut new_cur = proj.current_quantity;
        if let Some(proj_mut) = snap.corp_projects.get_mut(&pid) {
            proj_mut.current_quantity += accepted;
            new_cur = proj_mut.current_quantity;
            proj_mut.escrow_remaining = proj_mut
                .escrow_remaining
                .checked_sub(reward)
                .unwrap_or(IskCents::ZERO);
            completed = proj_mut.current_quantity >= proj_mut.target_quantity;
        }
        if completed {
            if let Some(m) = snap.alliance_war_state.project_meta.get_mut(&pid) {
                m.2 = true;
            }
            if let Some(d) = snap.alliance_war_state.project_details.get_mut(&pid) {
                d.status = "Completed".into();
            }
        }
        if reward.raw_cents() > 0 {
            let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = ch.balance.checked_add(reward).unwrap_or(ch.balance);
                ch.balance
            } else {
                reward
            };
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: char_id,
                entry_type: "corporation_project_reward".into(),
                amount: reward,
                balance_after: bal_after,
                reason: format!("Project {pid} ({}) contribution reward", proj.name),
            });
            snap.wallet_journal_timestamps.insert(jid, now_unix);

            let corp_bal = snap
                .corporations
                .get(&proj.corporation_id)
                .map(|c| c.master_wallet)
                .unwrap_or(IskCents::ZERO);
            snap.next_entity_id += 1;
            let corp_jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: corp_jid,
                character_id: proj.corporation_id,
                entry_type: "CorporationProjectPayout".into(),
                amount: IskCents(-reward.raw_cents()),
                balance_after: corp_bal,
                reason: format!("Project {pid} ({}) payout to pilot {char_id}", proj.name),
            });
            snap.wallet_journal_timestamps.insert(corp_jid, now_unix);

            let next_cjid = snap
                .corp_wallet_journals
                .values()
                .flatten()
                .map(|j| j.id)
                .max()
                .unwrap_or(500_000)
                + 1;
            snap.corp_wallet_journals
                .entry((proj.corporation_id, 1))
                .or_default()
                .push(eve_db::snapshot_ext::CorpWalletJournalRecord {
                    id: next_cjid,
                    date_unix: now_unix,
                    ref_type: "CorporationProjectPayout".into(),
                    description: format!("Project {pid} ({}) payout to pilot {char_id}", proj.name),
                    amount: -reward.to_f64_isk(),
                    balance: corp_bal.to_f64_isk(),
                    first_party_id: Some(proj.corporation_id),
                    second_party_id: Some(char_id),
                    reason: Some("Corporation project contribution reward".into()),
                });
        }
        updates.push((pid, new_cur, completed));
    }
    updates
}

pub fn record_server_project_activity(
    ctx: &ServerContext,
    char_id: i64,
    matcher_types: &[&str],
    qty: u64,
    filter_opt: Option<i64>,
) {
    let now_unix = ctx.now_unix();
    let updates = ctx.db.with_mut_snapshot(|snap| {
        record_server_project_activity_in_snap(snap, char_id, matcher_types, qty, filter_opt, now_unix)
    });
    for (pid, new_cur, completed) in updates {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnGoalProgressChanged",
            "charid",
            vec![PyRep::Integer(pid), PyRep::Integer(new_cur as i64)],
        );
        if completed {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnGoalCompleted",
                "charid",
                vec![PyRep::Integer(pid)],
            );
        }
    }
}

pub(super) fn handle_corp_projects_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "corporationSvc" {
        return None;
    }
    match method {
        "CreateGoalSubmit" => handle_create_project(ctx, session, args),
        "GoalProgress" | "CompleteCorpProject" | "CompleteGoal" => {
            handle_contribute_project(ctx, session, method, args)
        }
        "EditCorpProject" | "UpdateGoal" | "UpdateCorpProject" => {
            handle_edit_project(ctx, session, args)
        }
        "AcceptFreelanceJob" | "JoinFreelanceProject" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            sweep_expired_corp_projects(ctx);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(proj) = snap.corp_projects.get(&pid) else {
                    return Err(BridgeRpcError {
                        code: "ProjectNotFound".into(),
                        message: format!("Project {pid} not found"),
                    });
                };
                let is_fl = snap
                    .alliance_war_state
                    .project_meta
                    .get(&pid)
                    .is_some_and(|m| m.1);
                if !is_fl {
                    return Err(BridgeRpcError {
                        code: "NotFreelanceProject".into(),
                        message: format!("Project {pid} is not a freelance job"),
                    });
                }
                let det = snap
                    .alliance_war_state
                    .project_details
                    .entry(pid)
                    .or_default();
                if det.status != "Active" {
                    return Err(BridgeRpcError {
                        code: "ProjectNotActive".into(),
                        message: format!("Project {pid} is {}", det.status),
                    });
                }
                det.char_contributions.entry(char_id).or_insert(0);
                Ok(proj.corporation_id)
            });
            match res {
                Ok(cid) => Some(Ok(PyRep::keyval([
                    ("projectID", PyRep::Integer(pid)),
                    ("corporationID", PyRep::Integer(cid)),
                    ("accepted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }
        "GetGoals" | "GetCorpProjects" | "GetActiveFreelanceProjectId" | "GetFreelanceProjects" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            sweep_expired_corp_projects(ctx);
            let corp_id = session.corporation_id.unwrap_or(0);
            let only_freelance =
                matches!(method, "GetActiveFreelanceProjectId" | "GetFreelanceProjects");
            let rows = ctx.db.with_snapshot(|snap| {
                snap.corp_projects
                    .values()
                    .filter(|p| {
                        let is_fl = snap
                            .alliance_war_state
                            .project_meta
                            .get(&p.project_id)
                            .is_some_and(|m| m.1);
                        let det = snap
                            .alliance_war_state
                            .project_details
                            .get(&p.project_id)
                            .cloned()
                            .unwrap_or_default();
                        if only_freelance {
                            is_fl && det.status == "Active"
                        } else {
                            corp_id == 0 || p.corporation_id == corp_id || is_fl
                        }
                    })
                    .map(|p| {
                        let (matcher, is_fl, claimed) = snap
                            .alliance_war_state
                            .project_meta
                            .get(&p.project_id)
                            .cloned()
                            .unwrap_or(("DeliverItem".into(), false, false));
                        let det = snap
                            .alliance_war_state
                            .project_details
                            .get(&p.project_id)
                            .cloned()
                            .unwrap_or_default();
                        let my_contrib = det.char_contributions.get(&char_id).copied().unwrap_or(0);
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
                            PyRep::String(det.status),
                            PyRep::Real(p.escrow_remaining.to_wire_f64()),
                            PyRep::Integer(det.wallet_division as i64),
                            det.max_per_character
                                .map(|c| PyRep::Integer(c as i64))
                                .unwrap_or(PyRep::None),
                            PyRep::Integer(my_contrib as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "projectID",
                    "corporationID",
                    "name",
                    "matcherType",
                    "isFreelance",
                    "currentQuantity",
                    "targetQuantity",
                    "iskPerUnit",
                    "completed",
                    "claimed",
                    "status",
                    "escrowRemaining",
                    "walletDivision",
                    "maxPerCharacter",
                    "myContribution",
                ],
                rows,
            )))
        }
        "CloseCorpProject" | "DeleteCorpProject" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let corp_id = session.corporation_id.unwrap_or(0);
            let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_delete = method == "DeleteCorpProject";
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(proj_corp_id) = snap.corp_projects.get(&pid).map(|p| p.corporation_id)
                else {
                    return Err(BridgeRpcError {
                        code: "ProjectNotFound".into(),
                        message: format!("Corporation project {pid} not found"),
                    });
                };
                if proj_corp_id != corp_id {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message:
                            "Cannot close or delete a project belonging to another corporation"
                                .into(),
                    });
                }
                let is_gm = session.has_gm_role();
                let (is_ceo, caller_roles) = effective_corp_member_roles(
                    snap,
                    corp_id,
                    char_id,
                    session.corp_role,
                    ctx.now_unix(),
                );
                if !is_ceo
                    && !is_gm
                    && !corp_roles::has_role(caller_roles, corp_roles::ROLE_DIRECTOR)
                    && !corp_roles::has_role(caller_roles, corp_roles::ROLE_PROJECT_MANAGER)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Closing or deleting corporation projects requires Project Manager or Director role".into(),
                    });
                }
                let div_id = snap
                    .alliance_war_state
                    .project_details
                    .get(&pid)
                    .map(|d| d.wallet_division.clamp(1, 7))
                    .unwrap_or(1);
                let refund = if is_delete {
                    snap.alliance_war_state.project_details.remove(&pid);
                    snap.alliance_war_state.project_meta.remove(&pid);
                    snap.corp_projects
                        .remove(&pid)
                        .map(|p| p.escrow_remaining)
                        .unwrap_or(IskCents::ZERO)
                } else {
                    if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
                        det.status = "Closed".into();
                    }
                    snap.corp_projects
                        .get_mut(&pid)
                        .map(|p| std::mem::replace(&mut p.escrow_remaining, IskCents::ZERO))
                        .unwrap_or(IskCents::ZERO)
                };
                if refund.raw_cents() > 0 {
                    let mut bal_after = IskCents::ZERO;
                    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                        corp.ensure_wallet_divisions();
                        if let Some(div) = corp
                            .wallet_divisions
                            .iter_mut()
                            .find(|d| d.division_id == div_id)
                        {
                            div.balance = div.balance.checked_add(refund).unwrap_or(div.balance);
                            bal_after = div.balance;
                        }
                        if div_id == 1 {
                            corp.master_wallet = corp
                                .master_wallet
                                .checked_add(refund)
                                .unwrap_or(corp.master_wallet);
                            bal_after = corp.master_wallet;
                        }
                    }
                    snap.next_entity_id += 1;
                    let jid = snap.next_entity_id;
                    snap.wallet_journal.push(WalletJournalEntry {
                        entry_id: jid,
                        character_id: corp_id,
                        entry_type: "CorporationProjectRefund".into(),
                        amount: refund,
                        balance_after: bal_after,
                        reason: format!("Project {pid} escrow refund on closure"),
                    });
                    let now_unix = ctx.now_unix();
                    snap.wallet_journal_timestamps.insert(jid, now_unix);
                    let next_cjid = snap
                        .corp_wallet_journals
                        .values()
                        .flatten()
                        .map(|j| j.id)
                        .max()
                        .unwrap_or(500_000)
                        + 1;
                    snap.corp_wallet_journals
                        .entry((corp_id, div_id as i64))
                        .or_default()
                        .push(eve_db::snapshot_ext::CorpWalletJournalRecord {
                            id: next_cjid,
                            date_unix: now_unix,
                            ref_type: "CorporationProjectRefund".into(),
                            description: format!("Project {pid} escrow refund on closure"),
                            amount: refund.to_f64_isk(),
                            balance: bal_after.to_f64_isk(),
                            first_party_id: Some(corp_id),
                            second_party_id: None,
                            reason: Some("Corporation project escrow refund".into()),
                        });
                }
                Ok(refund.to_wire_f64())
            });
            let refunded_isk = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnGoalDeleted",
                "charid",
                vec![PyRep::Integer(pid)],
            );
            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("deleted", PyRep::Bool(is_delete)),
                ("closed", PyRep::Bool(true)),
                ("refundedISK", PyRep::Real(refunded_isk)),
            ])))
        }
        _ => None,
    }
}

pub fn handle_edit_project(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session.corporation_id.unwrap_or(0);
    let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let new_name_opt = args.get(1).and_then(|a| a.as_str()).map(String::from);
    let new_expires_opt = args.get(2).and_then(|a| a.as_i64()).filter(|&v| v > 0).map(|v| v as u64);
    let new_cap_opt = args.get(3).and_then(|a| a.as_i64()).filter(|&v| v > 0).map(|v| v as u64);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(proj) = snap.corp_projects.get(&pid) else {
            return Err(BridgeRpcError {
                code: "ProjectNotFound".into(),
                message: format!("Corporation project {pid} not found"),
            });
        };
        if proj.corporation_id != corp_id {
            return Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot edit a project belonging to another corporation".into(),
            });
        }
        let is_gm = session.has_gm_role();
        let (is_ceo, caller_roles) =
            effective_corp_member_roles(snap, corp_id, char_id, session.corp_role, ctx.now_unix());
        if snap.corporations.contains_key(&corp_id)
            && !is_ceo
            && !is_gm
            && !corp_roles::has_role(caller_roles, corp_roles::ROLE_DIRECTOR)
            && !corp_roles::has_role(caller_roles, corp_roles::ROLE_PROJECT_MANAGER)
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Editing corporation projects requires Project Manager or Director role".into(),
            });
        }
        if let Some(new_name) = &new_name_opt {
            if let Some(p) = snap.corp_projects.get_mut(&pid) {
                p.name = new_name.clone();
            }
        }
        let det = snap
            .alliance_war_state
            .project_details
            .entry(pid)
            .or_default();
        if let Some(exp) = new_expires_opt {
            det.expires_unix = Some(exp);
        }
        if let Some(cap) = new_cap_opt {
            det.max_per_character = Some(cap);
        }
        Ok(snap.corp_projects.get(&pid).map(|p| p.name.clone()).unwrap_or_default())
    });
    match res {
        Ok(updated_name) => Some(Ok(PyRep::keyval([
            ("projectID", PyRep::Integer(pid)),
            ("name", PyRep::String(updated_name)),
            ("updated", PyRep::Bool(true)),
        ]))),
        Err(e) => Some(Err(e)),
    }
}

fn handle_create_project(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let name = args
        .first()
        .and_then(|a| a.as_str())
        .unwrap_or("Deliver Tritanium");
    let target_qty_raw = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1_000);
    let isk_per_unit = args.get(2).and_then(|a| a.as_f64()).unwrap_or(10.0);
    let matcher_type = args.get(3).and_then(|a| a.as_str()).unwrap_or("DeliverItem");
    let is_freelance = matches!(args.get(4), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
    let raw_div_opt = args.get(5).and_then(|a| a.as_i64());
    let wallet_div: u8 = match raw_div_opt {
        Some(1000..=1006) => (raw_div_opt.unwrap() - 999) as u8,
        Some(d @ 1..=7) => d as u8,
        _ => 1,
    };
    let max_per_char = args
        .get(6)
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .map(|v| v as u64);
    let duration_secs = args
        .get(7)
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .map(|v| v as u64);
    let filter_id = args.get(8).and_then(|a| a.as_i64()).filter(|&v| v > 0);

    if target_qty_raw <= 0 || !isk_per_unit.is_finite() || isk_per_unit < 0.0 {
        return Some(Err(BridgeRpcError {
            code: "InvalidProjectParameters".into(),
            message: "Project requires target_quantity > 0 and finite isk_per_unit >= 0".into(),
        }));
    }
    let target_qty = target_qty_raw as u64;
    let now_unix = ctx.now_unix();
    let expires_unix = duration_secs.map(|d| now_unix.saturating_add(d));
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let per_u = IskCents::from_f64_isk(isk_per_unit);
        let escrow = per_u
            .checked_mul_qty(target_qty_raw)
            .ok_or_else(|| BridgeRpcError {
                code: "ProjectEscrowOverflow".into(),
                message: "Corporation project total escrow overflows 64-bit ISK cents".into(),
            })?;
        let Some(ch) = snap.characters.get(&char_id).cloned() else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
            });
        };
        let corp_id = session.corporation_id.unwrap_or(ch.corporation_id);
        if is_freelance {
            let active_fl_count = snap
                .corp_projects
                .values()
                .filter(|p| {
                    p.corporation_id == corp_id
                        && snap
                            .alliance_war_state
                            .project_meta
                            .get(&p.project_id)
                            .is_some_and(|m| m.1)
                        && snap
                            .alliance_war_state
                            .project_details
                            .get(&p.project_id)
                            .map_or(true, |d| d.status == "Active")
                })
                .count();
            if active_fl_count >= 100 {
                return Err(BridgeRpcError {
                    code: "MaxFreelanceProjectsReached".into(),
                    message: "Corporation cannot have more than 100 active freelance jobs".into(),
                });
            }
        }
        let is_gm = session.has_gm_role();
        let (is_ceo, caller_roles) =
            effective_corp_member_roles(snap, corp_id, char_id, session.corp_role, ctx.now_unix());
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            let take_bit = corp_roles::ROLE_ACCOUNT_CAN_TAKE[(wallet_div.clamp(1, 7) - 1) as usize];
            if !is_ceo
                && !is_gm
                && !corp_roles::has_role(caller_roles, corp_roles::ROLE_DIRECTOR)
                && (!corp_roles::has_role(caller_roles, corp_roles::ROLE_PROJECT_MANAGER)
                    || (escrow.raw_cents() > 0 && !corp_roles::has_role(caller_roles, take_bit)))
            {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Creating corporation projects with escrow requires Project Manager + wallet division take access, or Director role".into(),
                });
            }
            corp.ensure_wallet_divisions();
            let div = corp
                .wallet_divisions
                .iter_mut()
                .find(|d| d.division_id == wallet_div)
                .ok_or_else(|| BridgeRpcError {
                    code: "InvalidDivision".into(),
                    message: format!("Corporation wallet division {wallet_div} not found"),
                })?;
            let new_bal = div.balance.checked_sub(escrow).ok_or_else(|| BridgeRpcError {
                code: "CorpInsufficientFunds".into(),
                message: format!(
                    "Insufficient ISK in corporation wallet division {wallet_div} to fund project escrow"
                ),
            })?;
            div.balance = new_bal;
            if wallet_div == 1 {
                corp.master_wallet = new_bal;
            }
        } else {
            return Err(BridgeRpcError {
                code: "CorpNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
            });
        }
        if escrow.raw_cents() > 0 {
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            let bal_after = snap
                .corporations
                .get(&corp_id)
                .and_then(|c| c.wallet_divisions.iter().find(|d| d.division_id == wallet_div))
                .map(|d| d.balance)
                .unwrap_or(IskCents::ZERO);
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: corp_id,
                entry_type: "CorporationProjectEscrow".into(),
                amount: IskCents(-escrow.raw_cents()),
                balance_after: bal_after,
                reason: format!("Project escrow reserved for project '{name}'"),
            });
            snap.wallet_journal_timestamps.insert(jid, now_unix);
            let next_cjid = snap
                .corp_wallet_journals
                .values()
                .flatten()
                .map(|j| j.id)
                .max()
                .unwrap_or(500_000)
                + 1;
            snap.corp_wallet_journals
                .entry((corp_id, wallet_div as i64))
                .or_default()
                .push(eve_db::snapshot_ext::CorpWalletJournalRecord {
                    id: next_cjid,
                    date_unix: now_unix,
                    ref_type: "CorporationProjectEscrow".into(),
                    description: format!("Project escrow reserved for project '{name}'"),
                    amount: -escrow.to_f64_isk(),
                    balance: bal_after.to_f64_isk(),
                    first_party_id: Some(corp_id),
                    second_party_id: Some(char_id),
                    reason: Some("Corporation project escrow".into()),
                });
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.corp_projects.insert(
            id,
            CorpProjectRecord {
                project_id: id,
                corporation_id: corp_id,
                name: name.to_string(),
                target_quantity: target_qty,
                current_quantity: 0,
                isk_per_unit: per_u,
                escrow_remaining: escrow,
            },
        );
        snap.alliance_war_state
            .project_meta
            .insert(id, (matcher_type.to_string(), is_freelance, false));
        snap.alliance_war_state.project_details.insert(
            id,
            PersistedProjectDetails {
                wallet_division: wallet_div,
                max_per_character: max_per_char,
                expires_unix,
                status: "Active".into(),
                filter_id,
                char_contributions: Default::default(),
                ..Default::default()
            },
        );
        Ok((id, escrow.to_wire_f64()))
    });
    match res {
        Ok((pid, escrow_isk)) => {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnGoalProgressChanged",
                "charid",
                vec![PyRep::Integer(pid), PyRep::Integer(0)],
            );
            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("targetQuantity", PyRep::Integer(target_qty as i64)),
                ("matcherType", PyRep::String(matcher_type.to_string())),
                ("isFreelance", PyRep::Bool(is_freelance)),
                ("walletDivision", PyRep::Integer(wallet_div as i64)),
                ("escrowISK", PyRep::Real(escrow_isk)),
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
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let qty_raw = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100);
    if qty_raw <= 0 {
        return Some(Err(BridgeRpcError {
            code: "InvalidContributionQuantity".into(),
            message: "Contribution quantity must be > 0".into(),
        }));
    }
    let qty = qty_raw as u64;
    let now_unix = ctx.now_unix();
    sweep_expired_corp_projects(ctx);

    let (matcher_type, is_freelance, _) = ctx.db.with_snapshot(|snap| {
        snap.alliance_war_state
            .project_meta
            .get(&pid)
            .cloned()
            .unwrap_or(("DeliverItem".into(), false, false))
    });
    if matcher_type != "DeliverItem" {
        return Some(Err(BridgeRpcError {
            code: "ServerTrackedMatcher".into(),
            message: format!("Project {pid} progress ({matcher_type}) is tracked server-side"),
        }));
    }

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(proj) = snap.corp_projects.get(&pid).cloned() else {
            return Err(BridgeRpcError {
                code: "ProjectNotFound".into(),
                message: format!("Corporation project {pid} does not exist"),
            });
        };
        let det_snapshot = snap
            .alliance_war_state
            .project_details
            .get(&pid)
            .cloned()
            .unwrap_or_default();
        if det_snapshot.status == "Closed" {
            return Err(BridgeRpcError {
                code: "ProjectClosed".into(),
                message: format!("Corporation project {pid} has been closed"),
            });
        }
        if det_snapshot.status == "Expired" {
            return Err(BridgeRpcError {
                code: "ProjectExpired".into(),
                message: format!("Corporation project {pid} has expired"),
            });
        }
        let char_corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if proj.corporation_id != char_corp_id && !is_freelance {
            return Err(BridgeRpcError {
                code: "NotInProjectCorporation".into(),
                message: format!(
                    "Character {char_id} (corp {char_corp_id}) is not a member of project corporation {}",
                    proj.corporation_id
                ),
            });
        }
        let prev_char_qty = det_snapshot
            .char_contributions
            .get(&char_id)
            .copied()
            .unwrap_or(0);
        let char_rem = det_snapshot
            .max_per_character
            .map_or(u64::MAX, |cap| cap.saturating_sub(prev_char_qty));
        if char_rem == 0 {
            return Err(BridgeRpcError {
                code: "CharacterContributionLimitReached".into(),
                message: format!("Character {char_id} has reached the per-character contribution limit for project {pid}"),
            });
        }
        let needed = proj.target_quantity.saturating_sub(proj.current_quantity);
        let accepted = qty.min(needed).min(char_rem);
        if accepted == 0 {
            if method == "CompleteGoal" || method == "CompleteCorpProject" {
                return Ok((0.0, true, proj.current_quantity));
            }
            return Err(BridgeRpcError {
                code: "ProjectAlreadyCompleted".into(),
                message: format!("Corporation project {pid} is already completed"),
            });
        }
        if matcher_type == "DeliverItem" {
            let current_dock = snap
                .characters
                .get(&char_id)
                .and_then(|c| c.station_id.or(c.structure_id))
                .or(session.station_id.or(session.structure_id));
            if current_dock.is_none() {
                return Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "Delivering items to a corporation project requires being docked at a station or structure".into(),
                });
            }
            let current_ship = session.ship_id;
            let req_type = det_snapshot.filter_id.filter(|&v| v > 0).map(|v| v as u32);
            let contrib_item_id = snap
                .items
                .values()
                .find(|i| {
                    let type_ok = match req_type {
                        Some(rt) => i.type_id == rt,
                        None => matches!(i.type_id, 1230 | 62516 | 34),
                    };
                    type_ok
                        && (i.quantity as u64) >= accepted
                        && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                })
                .map(|i| i.item_id);
            let Some(iid) = contrib_item_id else {
                return Err(BridgeRpcError {
                    code: "InsufficientContributionItems".into(),
                    message: format!(
                        "Character {char_id} does not have {accepted} units of required item to contribute"
                    ),
                });
            };
            let mut delivered_type_and_loc: Option<(u32, i64)> = None;
            if let Some(item) = snap.items.get_mut(&iid) {
                let d_loc = current_dock.unwrap_or(item.location_id);
                let d_tid = item.type_id;
                if (item.quantity as u64) > accepted {
                    item.quantity -= accepted as i32;
                    item.version += 1;
                    delivered_type_and_loc = Some((d_tid, d_loc));
                } else {
                    item.owner_id = proj.corporation_id;
                    item.location_id = d_loc;
                    item.flag = InventoryFlag::Deliveries;
                    item.version += 1;
                }
            }
            if let Some((d_tid, d_loc)) = delivered_type_and_loc {
                if let Some(existing) = snap.items.values_mut().find(|i| {
                    i.owner_id == proj.corporation_id
                        && i.location_id == d_loc
                        && i.type_id == d_tid
                        && i.flag == InventoryFlag::Deliveries
                        && !i.singleton
                }) {
                    existing.quantity = existing.quantity.saturating_add(accepted as i32);
                    existing.version += 1;
                } else {
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: d_tid,
                            owner_id: proj.corporation_id,
                            location_id: d_loc,
                            flag: InventoryFlag::Deliveries,
                            quantity: accepted as i32,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
            }
        }
        *snap
            .alliance_war_state
            .project_details
            .entry(pid)
            .or_default()
            .char_contributions
            .entry(char_id)
            .or_insert(0) += accepted;
        let reward = proj
            .isk_per_unit
            .checked_mul_qty(accepted as i64)
            .unwrap_or(IskCents(0))
            .min(proj.escrow_remaining);
        let (mut done, mut cur_q) = (false, 0);
        if let Some(proj_mut) = snap.corp_projects.get_mut(&pid) {
            proj_mut.current_quantity += accepted;
            cur_q = proj_mut.current_quantity;
            proj_mut.escrow_remaining = proj_mut
                .escrow_remaining
                .checked_sub(reward)
                .unwrap_or(IskCents(0));
            done = proj_mut.current_quantity >= proj_mut.target_quantity;
        }
        if done {
            if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
                det.status = "Completed".into();
            }
        }
        let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = ch.balance.checked_add(reward).unwrap_or(ch.balance);
            ch.balance
        } else {
            reward
        };
        if reward.raw_cents() > 0 {
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: char_id,
                entry_type: "corporation_project_reward".into(),
                amount: reward,
                balance_after: bal_after,
                reason: format!("Project {pid} ({}) contribution reward", proj.name),
            });
            snap.wallet_journal_timestamps.insert(jid, now_unix);

            let corp_bal = snap
                .corporations
                .get(&proj.corporation_id)
                .map(|c| c.master_wallet)
                .unwrap_or(IskCents::ZERO);
            snap.next_entity_id += 1;
            let corp_jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: corp_jid,
                character_id: proj.corporation_id,
                entry_type: "CorporationProjectPayout".into(),
                amount: IskCents(-reward.raw_cents()),
                balance_after: corp_bal,
                reason: format!("Project {pid} ({}) payout to pilot {char_id}", proj.name),
            });
            snap.wallet_journal_timestamps.insert(corp_jid, now_unix);

            let next_cjid = snap
                .corp_wallet_journals
                .values()
                .flatten()
                .map(|j| j.id)
                .max()
                .unwrap_or(500_000)
                + 1;
            snap.corp_wallet_journals
                .entry((proj.corporation_id, 1))
                .or_default()
                .push(eve_db::snapshot_ext::CorpWalletJournalRecord {
                    id: next_cjid,
                    date_unix: now_unix,
                    ref_type: "CorporationProjectPayout".into(),
                    description: format!("Project {pid} ({}) payout to pilot {char_id}", proj.name),
                    amount: -reward.to_f64_isk(),
                    balance: corp_bal.to_f64_isk(),
                    first_party_id: Some(proj.corporation_id),
                    second_party_id: Some(char_id),
                    reason: Some("Corporation project contribution reward".into()),
                });
        }
        Ok((reward.to_wire_f64(), done, cur_q))
    });
    match res {
        Ok((payout_isk, completed, cur_q)) => {
            if completed {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(m) = snap.alliance_war_state.project_meta.get_mut(&pid) {
                        m.2 = true;
                    }
                });
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnGoalProgressChanged",
                "charid",
                vec![PyRep::Integer(pid), PyRep::Integer(cur_q as i64)],
            );
            if completed {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnGoalCompleted",
                    "charid",
                    vec![PyRep::Integer(pid)],
                );
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
