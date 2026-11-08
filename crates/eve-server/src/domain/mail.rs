//! Protocol-agnostic Mail & CSPA domain services (`#593`).

#![forbid(unsafe_code)]

use crate::domain::corp_governance::{can_send_corp_mail, effective_corp_role_breakdown, ALLOWED_CORP_MAIL_ROLES};
use crate::domain::DomainError;
use crate::router::ServerContext;
use crate::services::character_contacts::is_blocked_by;
use eve_db::{IskCents, MailRecord, WalletJournalEntry};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailRecipientKind {
    Character,
    Corporation,
    Alliance,
    MailingList,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailRecipientSpec {
    pub recipient_id: i64,
    pub kind: MailRecipientKind,
}

#[derive(Clone, Debug)]
pub struct CspaCalculationInput<'a> {
    pub caller_id: i64,
    pub target_ids: &'a [i64],
    pub default_charge: IskCents,
    pub exempt_on_same_alliance: bool,
    pub exempt_min_standing: f64,
}

#[derive(Clone, Debug)]
pub struct SendMailInput {
    pub sender_id: i64,
    pub recipients: Vec<MailRecipientSpec>,
    pub to_list_id: Option<i64>,
    pub to_corp_or_alliance_id: Option<i64>,
    pub subject: String,
    pub body: String,
    pub approved_cost_isk: Option<i64>,
    pub max_subject_chars: usize,
    pub max_body_chars: usize,
    pub expand_org_recipients_in_db: bool,
    pub record_cspa_journal: bool,
    pub cspa_exempt_min_standing: f64,
    pub cspa_exempt_same_corp: bool,
    pub now_unix: u64,
}

#[derive(Clone, Debug)]
pub struct SendMailResult {
    pub mail_id: i64,
    pub sender_id: i64,
    pub stored_recipient_labels: Vec<(i64, i64)>,
    pub notify_recipient_ids: Vec<i64>,
    pub to_character_ids: Vec<i64>,
    pub to_list_id: Option<i64>,
    pub to_corp_or_alliance_id: Option<i64>,
    pub cspa_charged: IskCents,
}

/// Computes the aggregate CSPA fee owed by `caller_id` across `target_ids`, consulting all
/// persisted/transient contact stores (`WorldStateSnapshot`, `esi_state`, `social_extras`) and
/// CSPA fee stores (`character_cspa_charges`, `social_extras.cspa_charges`).
pub fn calculate_cspa_fee(
    ctx: &ServerContext,
    input: &CspaCalculationInput<'_>,
) -> Result<IskCents, DomainError> {
    let (extra_contacts, extra_blocked, extra_cspa) = {
        let mut contacts = HashMap::new();
        let mut blocked = HashSet::new();
        let mut cspa = HashMap::new();
        if let Ok(s) = ctx.social_extras.lock() {
            for &tid in input.target_ids {
                if tid == input.caller_id || tid <= 0 {
                    continue;
                }
                if let Some(&(st, _, _)) = s.contacts.get(&tid).and_then(|m| m.get(&input.caller_id))
                {
                    contacts.insert(tid, st);
                }
                if s.blocked.get(&tid).is_some_and(|b| b.contains(&input.caller_id)) {
                    blocked.insert(tid);
                }
                if let Some(&ch) = s.cspa_charges.get(&tid) {
                    cspa.insert(tid, ch);
                }
            }
        }
        (contacts, blocked, cspa)
    };
    let esi_contacts = ctx
        .esi_state
        .char_contacts
        .read()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();

    ctx.db.with_snapshot(|snap| {
        let Some(caller) = snap.characters.get(&input.caller_id) else {
            return Err(DomainError::not_found(
                "CharacterNotFound",
                format!("Character {} not found", input.caller_id),
            ));
        };
        let mut total_cents = 0i64;
        for &tid in input.target_ids {
            if tid == input.caller_id || tid <= 0 {
                continue;
            }
            let target_char = snap.characters.get(&tid);
            let same_corp = target_char.is_some_and(|t| t.corporation_id == caller.corporation_id);
            let same_alliance = input.exempt_on_same_alliance
                && target_char.is_some_and(|t| {
                    caller.alliance_id.is_some() && caller.alliance_id == t.alliance_id
                });
            let is_exempt_contact = (!extra_blocked.contains(&tid)
                && extra_contacts
                    .get(&tid)
                    .is_some_and(|&st| st > input.exempt_min_standing))
                || snap.char_contacts.get(&tid).is_some_and(|list| {
                    list.iter().any(|c| {
                        c.contact_id == input.caller_id
                            && !c.is_blocked
                            && c.standing > input.exempt_min_standing
                    })
                })
                || esi_contacts.get(&tid).is_some_and(|list| {
                    list.iter().any(|c| {
                        c.contact_id == input.caller_id
                            && !c.is_blocked
                            && c.standing > input.exempt_min_standing
                    })
                });
            if !same_corp && !same_alliance && !is_exempt_contact {
                let charge = snap
                    .character_cspa_charges
                    .get(&tid)
                    .copied()
                    .or_else(|| extra_cspa.get(&tid).copied())
                    .unwrap_or(input.default_charge);
                if charge.0 > 0 {
                    total_cents = total_cents.saturating_add(charge.0);
                }
            }
        }
        Ok(IskCents(total_cents))
    })
}

/// Calculates CSPA fee and atomically debits `caller_id`'s wallet if `total_cspa > 0`.
pub fn charge_cspa_action(
    ctx: &ServerContext,
    input: &CspaCalculationInput<'_>,
) -> Result<IskCents, DomainError> {
    let total_cspa = calculate_cspa_fee(ctx, input)?;
    if total_cspa.0 > 0 {
        ctx.db.try_with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get_mut(&input.caller_id) else {
                return Err(DomainError::not_found(
                    "CharacterNotFound",
                    format!("Character {} not found", input.caller_id),
                ));
            };
            ch.balance = ch.balance.checked_sub(total_cspa).ok_or_else(|| {
                DomainError::invalid_input(
                    "InsufficientFundsForCSPA",
                    "Insufficient ISK to pay recipient CSPA charge",
                )
            })?;
            Ok(())
        })?;
    }
    Ok(total_cspa)
}

/// Validates and delivers mail across MachoNet and ESI callers, enforcing mailing-list ACLs,
/// corp/alliance membership and `restrict_corp_mails`, blocklist + friendly-contact rules,
/// CSPA fee & `approved_cost`, wallet debit + optional CSPA journal entry, and 1000-mail quota.
pub fn send_mail(ctx: &ServerContext, input: SendMailInput) -> Result<SendMailResult, DomainError> {
    if input.subject.is_empty() || input.subject.chars().count() > input.max_subject_chars {
        return Err(DomainError::invalid_input(
            "InvalidMailSubject",
            format!("Mail subject must be 1..={} characters", input.max_subject_chars),
        ));
    }
    if input.body.chars().count() > input.max_body_chars {
        return Err(DomainError::invalid_input(
            "MailBodyTooLong",
            format!("Mail body exceeds {} character limit", input.max_body_chars),
        ));
    }

    let explicit_list_id = input.to_list_id.or_else(|| {
        input
            .recipients
            .iter()
            .find(|r| r.kind == MailRecipientKind::MailingList)
            .map(|r| r.recipient_id)
    });

    let sender_org = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&input.sender_id)
            .map(|ch| (ch.corporation_id, ch.alliance_id))
    });

    // Resolve mailing-list ACL, members, and posting cost if targeting a mailing list.
    let mut ml_members: Vec<i64> = Vec::new();
    let mut ml_cost_transfer: Option<(i64, IskCents)> = None;
    if let Some(list_id) = explicit_list_id {
        let ml_info = {
            let s = ctx.social_extras.lock().unwrap();
            s.mailing_lists.get(&list_id).map(|m| {
                (
                    m.owner_id,
                    m.cost,
                    m.members.clone(),
                    m.default_access,
                    m.entity_access.clone(),
                )
            })
        }
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.mailing_lists.get(&list_id).map(|m| {
                    (
                        m.owner_id,
                        m.cost,
                        m.members.clone(),
                        m.default_access,
                        m.entity_access.clone(),
                    )
                })
            })
        });
        let (owner_id, cost, members, default_access, entity_access) = ml_info.ok_or_else(|| {
            DomainError::not_found("UserError", "MailingListNotFound").with_http_status(400)
        })?;
        if input.sender_id != owner_id {
            let entity_acc = entity_access
                .get(&input.sender_id)
                .or_else(|| sender_org.and_then(|(cid, _)| entity_access.get(&cid)))
                .or_else(|| {
                    sender_org
                        .and_then(|(_, aid)| aid)
                        .and_then(|aid| entity_access.get(&aid))
                })
                .copied()
                .unwrap_or(default_access);
            let role = members.get(&input.sender_id).copied();
            if role.is_none() || entity_acc <= 0 {
                return Err(DomainError::forbidden("UserError", "MailingListAccessDenied"));
            }
            if role == Some(0) {
                return Err(DomainError::forbidden("UserError", "MutedInMailingList"));
            }
        }
        ml_members.extend(members.keys().copied());
        let cost_isk = cost.clamp(0, 1_000_000_000);
        if cost_isk > 0 && input.sender_id != owner_id {
            ml_cost_transfer = Some((owner_id, IskCents::from_isk(cost_isk)));
        }
    }

    // Gather transient social + ESI contact/CSPA overlays once before inspecting snapshot.
    let mut candidate_char_ids: Vec<i64> = input
        .recipients
        .iter()
        .filter(|r| r.kind == MailRecipientKind::Character && r.recipient_id != input.sender_id)
        .map(|r| r.recipient_id)
        .collect();
    candidate_char_ids.extend(ml_members.iter().copied().filter(|&id| id != input.sender_id));
    candidate_char_ids.sort_unstable();
    candidate_char_ids.dedup();

    let (extra_contacts, extra_blocked, extra_cspa) = {
        let s = ctx.social_extras.lock().unwrap();
        let mut contacts = HashMap::new();
        let mut blocked = HashSet::new();
        let mut cspa = HashMap::new();
        for &rid in &candidate_char_ids {
            if let Some(&(st, _, _)) = s.contacts.get(&rid).and_then(|m| m.get(&input.sender_id)) {
                contacts.insert(rid, st);
            }
            if s.blocked.get(&rid).is_some_and(|b| b.contains(&input.sender_id)) {
                blocked.insert(rid);
            }
            if let Some(&ch) = s.cspa_charges.get(&rid) {
                cspa.insert(rid, ch);
            }
        }
        (contacts, blocked, cspa)
    };
    let esi_contacts = ctx
        .esi_state
        .char_contacts
        .read()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();

    // Check blocklist rules against explicit character & mailing-list recipients.
    let block_check_ids: Vec<i64> = input
        .recipients
        .iter()
        .filter(|r| r.kind == MailRecipientKind::Character && r.recipient_id != input.sender_id)
        .map(|r| r.recipient_id)
        .chain(if input.expand_org_recipients_in_db {
            ml_members.clone()
        } else {
            Vec::new()
        })
        .filter(|&id| id != input.sender_id)
        .collect();

    for rid in block_check_ids {
        let explicitly_blocked = ctx.db.with_snapshot(|snap| {
            snap.char_contacts
                .get(&rid)
                .is_some_and(|list| list.iter().any(|c| c.contact_id == input.sender_id && c.is_blocked))
        }) || esi_contacts.get(&rid).is_some_and(|list| {
            list.iter()
                .any(|c| c.contact_id == input.sender_id && c.is_blocked)
        });
        let friendly_contact = (!extra_blocked.contains(&rid)
            && extra_contacts.get(&rid).is_some_and(|&st| st > 0.0))
            || ctx.db.with_snapshot(|snap| {
                snap.char_contacts.get(&rid).is_some_and(|list| {
                    list.iter()
                        .any(|c| c.contact_id == input.sender_id && !c.is_blocked && c.standing > 0.0)
                })
            });
        if explicitly_blocked || (!friendly_contact && is_blocked_by(ctx, rid, input.sender_id)) {
            return Err(DomainError::forbidden(
                "ContactBlocked",
                format!("Recipient {rid} has blocked sender {}", input.sender_id),
            ));
        }
    }

    let now = input.now_unix;
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(sender) = snap.characters.get(&input.sender_id).cloned() else {
            return Err(DomainError::not_found(
                "CharacterNotFound",
                format!("Character {} not found", input.sender_id),
            ));
        };

        let mut target_label = 1i64;
        let mut db_recipients: Vec<(i64, i64)> = Vec::new();
        let mut label_recipients: Vec<(i64, i64)> = Vec::new();
        let mut notify_recipients: Vec<i64> = Vec::new();
        let mut cspa_char_targets: Vec<i64> = Vec::new();

        let handle_corp = |corp_id: i64,
                               db_recipients: &mut Vec<(i64, i64)>,
                               label_recipients: &mut Vec<(i64, i64)>,
                               notify_recipients: &mut Vec<i64>,
                               target_label: &mut i64|
         -> Result<(), DomainError> {
            let Some(corp) = snap.corporations.get(&corp_id) else {
                if ctx.sde.npc_corporations.contains_key(&corp_id) {
                    return Err(DomainError::forbidden(
                        "CorpMailAccessDenied",
                        format!("Cannot send corporation mail to NPC corporation {corp_id}"),
                    ));
                }
                return Err(DomainError::not_found(
                    "CorporationNotFound",
                    format!("Corporation {corp_id} not found"),
                )
                .with_http_status(400));
            };
            if sender.corporation_id != corp_id
                && !corp.members.contains(&input.sender_id)
                && corp.ceo_id != input.sender_id
            {
                return Err(DomainError::forbidden(
                    "CorpMailAccessDenied",
                    "Only corporation members can send corporation mail",
                ));
            }
            if !can_send_corp_mail(snap, corp_id, input.sender_id, now) {
                return Err(DomainError::forbidden(
                    "CorpMailRestricted",
                    "Corporation mail is restricted to leadership",
                ));
            }
            *target_label = 4;
            notify_recipients.extend(corp.members.iter().copied());
            for &mid in &corp.members {
                label_recipients.push((mid, 4));
                if input.expand_org_recipients_in_db {
                    db_recipients.push((mid, 4));
                }
            }
            if !input.expand_org_recipients_in_db {
                db_recipients.push((corp_id, 4));
            }
            Ok(())
        };

        let handle_alliance = |al_id: i64,
                                   db_recipients: &mut Vec<(i64, i64)>,
                                   label_recipients: &mut Vec<(i64, i64)>,
                                   notify_recipients: &mut Vec<i64>,
                                   target_label: &mut i64|
         -> Result<(), DomainError> {
            let Some(al) = snap.alliances.get(&al_id) else {
                return Err(DomainError::not_found(
                    "AllianceNotFound",
                    format!("Alliance {al_id} not found"),
                )
                .with_http_status(400));
            };
            let in_alliance = sender.alliance_id == Some(al_id)
                || (sender.corporation_id > 0 && al.member_corp_ids.contains(&sender.corporation_id));
            if !in_alliance {
                return Err(DomainError::forbidden(
                    "AllianceMailAccessDenied",
                    "Only alliance members can send alliance mail",
                ));
            }
            let al_restrict = (al.executor_corp_id > 0
                && snap
                    .corp_governance
                    .get(&al.executor_corp_id)
                    .is_some_and(|g| g.restrict_corp_mails))
                || snap
                    .corp_governance
                    .get(&sender.corporation_id)
                    .is_some_and(|g| g.restrict_corp_mails);
            if al_restrict {
                let eff = effective_corp_role_breakdown(snap, sender.corporation_id, input.sender_id, now);
                if !eff.is_ceo && (eff.any_active_roles() & ALLOWED_CORP_MAIL_ROLES) == 0 {
                    return Err(DomainError::forbidden(
                        "CorpMailRestricted",
                        format!("Alliance {al_id} restricts alliance mail to authorized officers"),
                    ));
                }
            }
            *target_label = 8;
            for cid in &al.member_corp_ids {
                if let Some(c) = snap.corporations.get(cid) {
                    notify_recipients.extend(c.members.iter().copied());
                    for &mid in &c.members {
                        label_recipients.push((mid, 8));
                        if input.expand_org_recipients_in_db {
                            db_recipients.push((mid, 8));
                        }
                    }
                }
            }
            if !input.expand_org_recipients_in_db {
                db_recipients.push((al_id, 8));
            }
            Ok(())
        };

        for spec in &input.recipients {
            match spec.kind {
                MailRecipientKind::Corporation => {
                    handle_corp(
                        spec.recipient_id,
                        &mut db_recipients,
                        &mut label_recipients,
                        &mut notify_recipients,
                        &mut target_label,
                    )?;
                }
                MailRecipientKind::Alliance => {
                    handle_alliance(
                        spec.recipient_id,
                        &mut db_recipients,
                        &mut label_recipients,
                        &mut notify_recipients,
                        &mut target_label,
                    )?;
                }
                MailRecipientKind::MailingList => {
                    notify_recipients.extend(ml_members.iter().copied());
                    for &mid in &ml_members {
                        label_recipients.push((mid, 1));
                        if input.expand_org_recipients_in_db {
                            db_recipients.push((mid, 1));
                            if mid != input.sender_id {
                                cspa_char_targets.push(mid);
                            }
                        }
                    }
                    if !input.expand_org_recipients_in_db {
                        db_recipients.push((spec.recipient_id, 1));
                    }
                }
                MailRecipientKind::Character => {
                    // MachoNet `SendMail` may pass a corp or alliance ID inside the raw recipient ID list.
                    if input.expand_org_recipients_in_db
                        && snap.corporations.contains_key(&spec.recipient_id)
                    {
                        handle_corp(
                            spec.recipient_id,
                            &mut db_recipients,
                            &mut label_recipients,
                            &mut notify_recipients,
                            &mut target_label,
                        )?;
                    } else if input.expand_org_recipients_in_db
                        && snap.alliances.contains_key(&spec.recipient_id)
                    {
                        handle_alliance(
                            spec.recipient_id,
                            &mut db_recipients,
                            &mut label_recipients,
                            &mut notify_recipients,
                            &mut target_label,
                        )?;
                    } else {
                        if !input.expand_org_recipients_in_db
                            && !snap.characters.contains_key(&spec.recipient_id)
                        {
                            return Err(DomainError::invalid_input(
                                "CharacterNotFound",
                                format!("Character recipient {} not found", spec.recipient_id),
                            ));
                        }
                        db_recipients.push((spec.recipient_id, 1));
                        label_recipients.push((spec.recipient_id, 1));
                        notify_recipients.push(spec.recipient_id);
                        if spec.recipient_id != input.sender_id {
                            cspa_char_targets.push(spec.recipient_id);
                        }
                    }
                }
            }
        }

        // MachoNet `SendMail` with explicit `to_list_id` (not already in `recipients`)
        if input.to_list_id.is_some()
            && !input
                .recipients
                .iter()
                .any(|r| r.kind == MailRecipientKind::MailingList)
        {
            notify_recipients.extend(ml_members.iter().copied());
            for &mid in &ml_members {
                db_recipients.push((mid, 1));
                label_recipients.push((mid, 1));
                if mid != input.sender_id {
                    cspa_char_targets.push(mid);
                }
            }
        }

        // MachoNet `SendMail` fallback when recipient list is empty and `to_corp_or_alliance_id` is set.
        if db_recipients.is_empty() {
            if let Some(org_id) = input.to_corp_or_alliance_id {
                if snap.corporations.contains_key(&org_id) {
                    handle_corp(
                        org_id,
                        &mut db_recipients,
                        &mut label_recipients,
                        &mut notify_recipients,
                        &mut target_label,
                    )?;
                } else if snap.alliances.contains_key(&org_id) {
                    handle_alliance(
                        org_id,
                        &mut db_recipients,
                        &mut label_recipients,
                        &mut notify_recipients,
                        &mut target_label,
                    )?;
                }
            }
        }

        // Dedup DB recipients preserving first label per recipient_id.
        let mut seen_db = HashSet::new();
        db_recipients.retain(|(rid, _)| seen_db.insert(*rid));
        if db_recipients.is_empty() {
            return Err(DomainError::invalid_input(
                "InvalidRecipient",
                "SendMail requires at least one recipient",
            ));
        }

        // Quota check for multi-recipient mail.
        if db_recipients.len() > 1 {
            for &(rid, _) in &db_recipients {
                let char_mail_count = snap
                    .mails
                    .iter()
                    .filter(|m| m.recipient_id == rid && !snap.deleted_mails.contains(&(rid, m.message_id)))
                    .count();
                if char_mail_count >= 1_000 {
                    return Err(DomainError::conflict(
                        "MailQuotaExceeded",
                        "Mailbox limit reached",
                    ));
                }
            }
        }

        // Calculate CSPA charges across explicit character recipients (and expanded mailing-list members).
        let mut total_cspa_cents = 0i64;
        cspa_char_targets.sort_unstable();
        cspa_char_targets.dedup();
        for rid in cspa_char_targets {
            let same_corp = input.cspa_exempt_same_corp
                && snap
                    .characters
                    .get(&rid)
                    .is_some_and(|rc| rc.corporation_id == sender.corporation_id);
            let exempt_contact = (!extra_blocked.contains(&rid)
                && extra_contacts
                    .get(&rid)
                    .is_some_and(|&st| st > input.cspa_exempt_min_standing))
                || snap.char_contacts.get(&rid).is_some_and(|list| {
                    list.iter().any(|c| {
                        c.contact_id == input.sender_id
                            && !c.is_blocked
                            && c.standing > input.cspa_exempt_min_standing
                    })
                })
                || esi_contacts.get(&rid).is_some_and(|list| {
                    list.iter().any(|c| {
                        c.contact_id == input.sender_id
                            && !c.is_blocked
                            && c.standing > input.cspa_exempt_min_standing
                    })
                });
            if !same_corp && !exempt_contact {
                let charge = snap
                    .character_cspa_charges
                    .get(&rid)
                    .copied()
                    .or_else(|| extra_cspa.get(&rid).copied())
                    .unwrap_or(IskCents(0));
                if charge.0 > 0 {
                    total_cspa_cents = total_cspa_cents.saturating_add(charge.0);
                }
            }
        }

        let ml_cost_cents = ml_cost_transfer.map(|(_, c)| c.0).unwrap_or(0);
        let total_cost_cents = total_cspa_cents.saturating_add(ml_cost_cents);

        if let Some(approved_isk) = input.approved_cost_isk {
            let approved_cents = IskCents::from_f64_isk(approved_isk as f64).0;
            if total_cost_cents > approved_cents {
                return Err(DomainError::invalid_input(
                    "ApprovedCostExceeded",
                    format!(
                        "CSPA charge ({:.2} ISK) exceeds approved_cost ({approved_isk} ISK)",
                        IskCents(total_cost_cents).to_wire_f64()
                    ),
                ));
            }
        }

        if total_cost_cents > 0 {
            let sender_ch = snap.characters.get_mut(&input.sender_id).ok_or_else(|| {
                DomainError::not_found(
                    "CharacterNotFound",
                    format!("Character {} not found", input.sender_id),
                )
            })?;
            if sender_ch.balance.0 < total_cost_cents {
                return Err(DomainError::invalid_input(
                    "InsufficientFundsForCSPA",
                    "Insufficient ISK to cover CSPA / mailing list cost",
                ));
            }
            let new_balance = IskCents(sender_ch.balance.0 - total_cost_cents);
            sender_ch.balance = new_balance;

            if let Some((owner_id, cost)) = ml_cost_transfer {
                if let Some(owner_ch) = snap.characters.get_mut(&owner_id) {
                    owner_ch.balance = owner_ch.balance.checked_add(cost).ok_or_else(|| {
                        DomainError::invalid_input(
                            "InsufficientFundsForCSPA",
                            "Balance overflow on mailing list owner",
                        )
                    })?;
                }
            }
            if input.record_cspa_journal && total_cspa_cents > 0 {
                snap.next_entity_id += 1;
                let tx_id = snap.next_entity_id;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: tx_id,
                    character_id: input.sender_id,
                    entry_type: "cspa".to_string(),
                    amount: IskCents(-total_cspa_cents),
                    balance_after: new_balance,
                    reason: "CONCORD Spam Protection Act (CSPA) mail charge".to_string(),
                });
            }
        }

        snap.next_entity_id += 1;
        let mail_id = snap.next_entity_id;
        snap.mail_timestamps.insert(mail_id, now);
        snap.mail_assigned_labels.insert((input.sender_id, mail_id), vec![2]);
        snap.read_mails.insert((input.sender_id, mail_id));

        for (rid, _label_id) in db_recipients {
            snap.mails.push(MailRecord {
                message_id: mail_id,
                sender_id: input.sender_id,
                recipient_id: rid,
                subject: input.subject.clone(),
                body: input.body.clone(),
            });
        }
        let mut seen_label_rids = HashSet::new();
        let mut stored_recipient_labels = Vec::with_capacity(label_recipients.len());
        for (rid, label_id) in label_recipients {
            if rid != input.sender_id && seen_label_rids.insert(rid) {
                snap.mail_assigned_labels.insert((rid, mail_id), vec![label_id]);
                stored_recipient_labels.push((rid, label_id));
            }
        }

        if let Some(lid) = explicit_list_id {
            if let Some(ml) = snap.mailing_lists.get_mut(&lid) {
                ml.mail_ids.insert(mail_id);
            }
        }

        Ok((
            mail_id,
            stored_recipient_labels,
            notify_recipients,
            IskCents(total_cspa_cents),
        ))
    })?;

    let (mail_id, stored_recipient_labels, notify_recipient_ids, cspa_charged) = res;

    if let Some(lid) = explicit_list_id {
        if let Ok(mut s) = ctx.social_extras.lock() {
            if let Some(ml) = s.mailing_lists.get_mut(&lid) {
                ml.mail_ids.insert(mail_id);
            }
        }
    }

    let to_character_ids: Vec<i64> = input
        .recipients
        .iter()
        .filter(|r| r.kind == MailRecipientKind::Character)
        .map(|r| r.recipient_id)
        .collect();
    let to_corp_or_alliance_id = input.to_corp_or_alliance_id.or_else(|| {
        input
            .recipients
            .iter()
            .find(|r| {
                matches!(
                    r.kind,
                    MailRecipientKind::Corporation | MailRecipientKind::Alliance
                )
            })
            .map(|r| r.recipient_id)
    });

    Ok(SendMailResult {
        mail_id,
        sender_id: input.sender_id,
        stored_recipient_labels,
        notify_recipient_ids,
        to_character_ids,
        to_list_id: explicit_list_id,
        to_corp_or_alliance_id,
        cspa_charged,
    })
}
