//! `fleet_social_persistence` — `D-6` Social Durability SQLite synchronization and startup hydration
//! helpers for `fleet_social`, `character_contacts`, `fleet_social_mail`, and `fleet_social_corp_gov`.

#![forbid(unsafe_code)]

use crate::esi::runtime_state::{
    EsiCalendarEvent, EsiContactEntry, EsiMailLabel, EsiRuntimeState,
};
use crate::router::ServerContext;
use eve_db::IskCents;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub(crate) struct MailingListState {
    pub(crate) list_id: i64,
    pub(crate) name: String,
    pub(crate) owner_id: i64,
    pub(crate) default_access: i64,
    pub(crate) default_role: i64,
    pub(crate) cost: i64,
    pub(crate) members: HashMap<i64, i64>,
    pub(crate) entity_access: HashMap<i64, i64>,
    pub(crate) welcome_mail: Option<(String, String)>,
    pub(crate) mail_ids: HashSet<i64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CalendarEventState {
    pub(crate) event_id: i64,
    pub(crate) owner_id: i64,
    pub(crate) creator_id: i64,
    pub(crate) event_date_time: i64,
    pub(crate) duration: i64,
    pub(crate) importance: i64,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) _scope: String,
    pub(crate) responses: HashMap<i64, i64>,
    pub(crate) is_deleted: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SocialExtrasState {
    pub(crate) next_id: i64,
    pub(crate) mailing_lists_seeded: bool,
    pub(crate) cspa_charges: HashMap<i64, IskCents>,
    pub(crate) trashed_mails: HashSet<(i64, i64)>,
    pub(crate) mailing_lists: HashMap<i64, MailingListState>,
    pub(crate) mail_headers_meta: HashMap<i64, crate::services::mail_notify::MailHeaderMeta>,
    pub(crate) contacts: HashMap<i64, HashMap<i64, (f64, bool, u64)>>,
    pub(crate) blocked: HashMap<i64, HashSet<i64>>,
    pub(crate) char_notes: HashMap<(i64, i64), String>,
    pub(crate) calendar_events: HashMap<i64, CalendarEventState>,
    pub(crate) muted_in_channel: HashMap<(String, i64), (u64, String)>,
    pub(crate) blocked_in_channel: HashMap<String, HashSet<i64>>,
    pub(crate) delayed_local_speakers: HashMap<String, HashSet<i64>>,
    pub(crate) chat_rate_window: HashMap<i64, (u64, u32)>,
    pub(crate) active_probes: HashMap<i64, Vec<(i64, [f64; 3], f64, u64)>>,
    pub(crate) forced_session_corp_sync: HashMap<i64, (i64, Option<i64>, u64)>,
}

impl SocialExtrasState {
    pub(crate) fn new() -> Self {
        Self { next_id: 700_000, ..Default::default() }
    }
}

pub(crate) fn with_social_state<R>(ctx: &ServerContext, f: impl FnOnce(&mut SocialExtrasState) -> R) -> R {
    let mut s = ctx.social_extras.lock().unwrap();
    f(&mut s)
}

pub fn label_mask_from_ids(label_ids: &[i64]) -> i64 {
    label_ids.iter().copied().fold(0i64, |acc, id| acc | id)
}

pub fn label_ids_from_mask(mask: u64) -> Vec<i64> {
    (0..63)
        .map(|b| 1i64 << b)
        .filter(|&bit| (mask & (bit as u64)) != 0)
        .collect()
}

pub fn sync_char_contacts_to_db(ctx: &ServerContext, char_id: i64) {
    let contacts: Vec<eve_db::SocialContactRecord> = ctx
        .esi_state
        .char_contacts
        .read()
        .ok()
        .and_then(|g| g.get(&char_id).cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|e| eve_db::SocialContactRecord {
            contact_id: e.contact_id,
            contact_type: e.contact_type,
            standing: e.standing,
            is_watched: e.is_watched,
            is_blocked: e.is_blocked,
            label_ids: e.label_ids,
        })
        .collect();
    let labels: Vec<(i64, String)> = ctx
        .esi_state
        .char_contact_labels
        .read()
        .ok()
        .and_then(|g| g.get(&char_id).cloned())
        .unwrap_or_default();
    if let Ok(mut se) = ctx.social_extras.lock() {
        let mut cmap = HashMap::new();
        let mut blocked_set = HashSet::new();
        for c in &contacts {
            if !c.is_blocked {
                cmap.insert(
                    c.contact_id,
                    (
                        c.standing,
                        c.is_watched,
                        label_mask_from_ids(&c.label_ids) as u64,
                    ),
                );
            } else {
                blocked_set.insert(c.contact_id);
            }
        }
        se.contacts.insert(char_id, cmap);
        se.blocked.insert(char_id, blocked_set);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.insert(char_id, contacts);
        snap.char_contact_labels.insert(char_id, labels);
    });
}

pub fn is_blocked_by(ctx: &ServerContext, target_char_id: i64, sender_char_id: i64) -> bool {
    let esi_blocked = ctx
        .esi_state
        .char_contacts
        .read()
        .ok()
        .and_then(|map| map.get(&target_char_id).cloned())
        .and_then(|list| list.into_iter().find(|c| c.contact_id == sender_char_id))
        .is_some_and(|c| c.is_blocked);
    if esi_blocked {
        return true;
    }
    ctx.db.with_snapshot(|snap| {
        snap.char_contacts
            .get(&target_char_id)
            .is_some_and(|list| list.iter().any(|c| c.contact_id == sender_char_id && c.is_blocked))
    })
}

pub(crate) fn sync_corp_contacts_esi(ctx: &ServerContext, corp_id: i64) {
    let (contacts, labels) = ctx.db.with_snapshot(|snap| {
        (
            snap.corp_contacts.get(&corp_id).cloned().unwrap_or_default(),
            snap.corp_contact_labels
                .get(&corp_id)
                .cloned()
                .unwrap_or_default(),
        )
    });
    if let Ok(mut esi_c) = ctx.esi_state.corp_contacts.write() {
        esi_c.insert(
            corp_id,
            contacts
                .into_iter()
                .map(|c| EsiContactEntry {
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
    if let Ok(mut esi_l) = ctx.esi_state.corp_contact_labels.write() {
        esi_l.insert(
            corp_id,
            labels.into_iter().map(|(id, name, _)| (id, name)).collect(),
        );
    }
}

pub(crate) fn ensure_social_extras_mailing_lists_seeded(ctx: &ServerContext) {
    let persisted_lists = ctx.db.with_snapshot(|snap| snap.mailing_lists.clone());
    let mut se = ctx.social_extras.lock().unwrap();
    for (&ml_id, ml) in &persisted_lists {
        se.next_id = se.next_id.max(ml_id + 1);
        se.mailing_lists.entry(ml_id).or_insert_with(|| MailingListState {
            list_id: ml.list_id,
            name: ml.name.clone(),
            owner_id: ml.owner_id,
            default_access: ml.default_access,
            default_role: ml.default_role,
            cost: ml.cost,
            members: ml.members.clone(),
            entity_access: ml.entity_access.clone(),
            welcome_mail: ml.welcome_mail.clone(),
            mail_ids: ml.mail_ids.clone(),
        });
    }
}

pub(crate) fn sync_mailing_lists_to_db(ctx: &ServerContext) {
    let lists: HashMap<i64, eve_db::MailingListRecord> = {
        let s = ctx.social_extras.lock().unwrap();
        s.mailing_lists
            .iter()
            .map(|(&lid, ml)| {
                (
                    lid,
                    eve_db::MailingListRecord {
                        list_id: ml.list_id,
                        name: ml.name.clone(),
                        owner_id: ml.owner_id,
                        default_access: ml.default_access,
                        default_role: ml.default_role,
                        cost: ml.cost,
                        members: ml.members.clone(),
                        entity_access: ml.entity_access.clone(),
                        welcome_mail: ml.welcome_mail.clone(),
                        mail_ids: ml.mail_ids.clone(),
                    },
                )
            })
            .collect()
    };
    if let Ok(mut esi_lists) = ctx.esi_state.mail_lists.write() {
        esi_lists.clear();
        for ml in lists.values() {
            for &member_id in ml.members.keys() {
                esi_lists
                    .entry(member_id)
                    .or_default()
                    .push((ml.list_id, ml.name.clone()));
            }
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.mailing_lists = lists;
    });
}

pub fn persist_social_state(ctx: &ServerContext) {
    sync_calendar_events_to_db(ctx);
    sync_mailing_lists_to_db(ctx);
}

pub(crate) fn sync_calendar_events_to_db(ctx: &ServerContext) {
    let db_evs = ctx.db.with_snapshot(|snap| snap.calendar_events.clone());
    let events: HashMap<i64, eve_db::CalendarEventRecord> = {
        let mut s = ctx.social_extras.lock().unwrap();
        for (eid, ev) in db_evs {
            s.next_id = s.next_id.max(eid);
            let entry = s.calendar_events.entry(eid).or_insert_with(|| CalendarEventState {
                event_id: ev.event_id, owner_id: ev.owner_id, creator_id: ev.creator_id,
                event_date_time: ev.event_date_time, duration: ev.duration, importance: ev.importance,
                title: ev.title, description: ev.description, _scope: ev.scope,
                responses: ev.responses, is_deleted: ev.is_deleted,
            });
            if ev.is_deleted {
                entry.is_deleted = true;
            }
        }
        s.calendar_events
            .iter()
            .map(|(&eid, ev)| {
                (eid, eve_db::CalendarEventRecord {
                    event_id: ev.event_id, owner_id: ev.owner_id, creator_id: ev.creator_id,
                    event_date_time: ev.event_date_time, duration: ev.duration, importance: ev.importance,
                    title: ev.title.clone(), description: ev.description.clone(), scope: ev._scope.clone(),
                    responses: ev.responses.clone(), is_deleted: ev.is_deleted,
                })
            })
            .collect()
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let (Ok(mut esi_evs), Ok(mut esi_resps)) = (
            ctx.esi_state.calendar_events.write(),
            ctx.esi_state.calendar_responses.write(),
        ) {
            esi_evs.retain(|&id, _| id <= 10);
            for (&eid, ev) in &events {
                if ev.is_deleted {
                    esi_evs.remove(&eid);
                    continue;
                }
                let event_date_unix = if ev.event_date_time > 100_000_000_000_000_000 {
                    ((ev.event_date_time / 10_000_000) - 11_644_473_600).max(0) as u64
                } else {
                    ev.event_date_time.max(0) as u64
                };
                let owner_type = match ev.scope.as_str() {
                    "Corporation" => "corporation",
                    "Alliance" => "alliance",
                    _ => "character",
                }
                .to_string();
                let owner_name = snap
                    .characters
                    .get(&ev.owner_id)
                    .map(|c| c.name.clone())
                    .or_else(|| snap.corporations.get(&ev.owner_id).map(|c| c.name.clone()))
                    .or_else(|| snap.alliances.get(&ev.owner_id).map(|a| a.name.clone()))
                    .unwrap_or_else(|| format!("Owner {}", ev.owner_id));
                let mut attendees: Vec<i64> = ev.responses.keys().copied().collect();
                attendees.sort_unstable();
                esi_evs.insert(
                    eid,
                    EsiCalendarEvent {
                        event_id: eid,
                        event_date_unix,
                        title: ev.title.clone(),
                        text: ev.description.clone(),
                        duration_minutes: ev.duration,
                        importance: ev.importance,
                        owner_id: ev.owner_id,
                        owner_name,
                        owner_type,
                        attendees,
                    },
                );
                for (&cid, &status) in &ev.responses {
                    let resp_str = match status {
                        4 => "accepted",
                        2 => "declined",
                        5 => "tentative",
                        _ => "not_responded",
                    };
                    esi_resps.insert((cid, eid), resp_str.to_string());
                }
            }
        }
        snap.calendar_events = events;
    });
}

pub(crate) fn update_calendar_response_from_esi(
    ctx: &ServerContext,
    char_id: i64,
    event_id: i64,
    response_code: i64,
) -> Option<(i64, i64, i64, String, i64)> {
    let out = {
        let mut s = ctx.social_extras.lock().unwrap();
        s.calendar_events.get_mut(&event_id).map(|ev| {
            ev.responses.insert(char_id, response_code);
            (ev.owner_id, ev.event_date_time, ev.duration, ev.title.clone(), ev.importance)
        })
    };
    if out.is_some() {
        sync_calendar_events_to_db(ctx);
    }
    out
}

pub fn hydrate_social_state_from_db(ctx: &ServerContext) {
    let social_snap = ctx.db.with_snapshot(|snap| {
        (
            !snap.mailing_lists.is_empty(),
            snap.character_cspa_charges.clone(),
            snap.trashed_mails.clone(),
            snap.char_notes.clone(),
            snap.mailing_lists.clone(),
            snap.calendar_events.clone(),
            snap.char_contacts.clone(),
            snap.chat_channel_acl.clone(),
        )
    });

    if let Ok(mut se) = ctx.social_extras.lock() {
        let (
            seeded,
            cspa_charges,
            trashed_mails,
            char_notes,
            mailing_lists,
            calendar_events,
            char_contacts,
            chat_channel_acl,
        ) = social_snap;
        se.mailing_lists_seeded = seeded;
        for (cid, charge) in cspa_charges {
            se.cspa_charges.insert(cid, charge);
        }
        for pair in trashed_mails {
            se.trashed_mails.insert(pair);
        }
        for (k, v) in char_notes {
            se.char_notes.insert(k, v);
        }
        for (ml_id, ml) in mailing_lists {
            se.next_id = se.next_id.max(ml_id + 1);
            se.mailing_lists.insert(
                ml_id,
                MailingListState {
                    list_id: ml.list_id,
                    name: ml.name,
                    owner_id: ml.owner_id,
                    default_access: ml.default_access,
                    default_role: ml.default_role,
                    cost: ml.cost,
                    members: ml.members,
                    entity_access: ml.entity_access,
                    welcome_mail: ml.welcome_mail,
                    mail_ids: ml.mail_ids,
                },
            );
        }
        for (ev_id, ev) in calendar_events {
            se.next_id = se.next_id.max(ev_id + 1);
            se.calendar_events.insert(
                ev_id,
                CalendarEventState {
                    event_id: ev.event_id,
                    owner_id: ev.owner_id,
                    creator_id: ev.creator_id,
                    event_date_time: ev.event_date_time,
                    duration: ev.duration,
                    importance: ev.importance,
                    title: ev.title,
                    description: ev.description,
                    _scope: ev.scope,
                    responses: ev.responses,
                    is_deleted: ev.is_deleted,
                },
            );
        }
        for (char_id, list) in char_contacts {
            let mut cmap = HashMap::new();
            let mut bset = HashSet::new();
            for c in list {
                let mask = c.label_ids.iter().fold(0u64, |acc, &id| acc | (id as u64));
                if c.is_blocked {
                    bset.insert(c.contact_id);
                } else {
                    cmap.insert(c.contact_id, (c.standing, c.is_watched, mask));
                }
            }
            se.contacts.insert(char_id, cmap);
            se.blocked.insert(char_id, bset);
        }
        for (chan_id, acl) in chat_channel_acl {
            if !acl.deny_list.is_empty() {
                se.blocked_in_channel
                    .insert(chan_id.clone(), acl.deny_list.into_iter().collect());
            }
            for (cid, (until, reason)) in acl.muted_until {
                se.muted_in_channel
                    .insert((chan_id.clone(), cid), (until, reason));
            }
        }
    }

    let (join_reqs_blob, watchlist_blob, options_blob) = ctx.db.with_snapshot(|snap| {
        let to_esi = |list: &[eve_db::SocialContactRecord]| -> Vec<EsiContactEntry> {
            list.iter()
                .map(|c| EsiContactEntry {
                    contact_id: c.contact_id,
                    contact_type: c.contact_type.clone(),
                    standing: c.standing,
                    is_watched: c.is_watched,
                    is_blocked: c.is_blocked,
                    label_ids: c.label_ids.clone(),
                })
                .collect()
        };
        if let Ok(mut esi_cc) = ctx.esi_state.char_contacts.write() {
            for (&id, list) in &snap.char_contacts {
                esi_cc.insert(id, to_esi(list));
            }
        }
        if let Ok(mut esi_cl) = ctx.esi_state.char_contact_labels.write() {
            for (&id, labels) in &snap.char_contact_labels {
                esi_cl.insert(id, labels.clone());
            }
        }
        if let Ok(mut esi_corpc) = ctx.esi_state.corp_contacts.write() {
            for (&id, list) in &snap.corp_contacts {
                esi_corpc.insert(id, to_esi(list));
            }
        }
        if let Ok(mut esi_corpl) = ctx.esi_state.corp_contact_labels.write() {
            for (&id, labels) in &snap.corp_contact_labels {
                esi_corpl.insert(id, labels.iter().map(|(lid, n, _)| (*lid, n.clone())).collect());
            }
        }
        if let Ok(mut esi_ac) = ctx.esi_state.alliance_contacts.write() {
            for (&id, list) in &snap.alliance_contacts {
                esi_ac.insert(id, to_esi(list));
            }
        }
        if let Ok(mut esi_al) = ctx.esi_state.alliance_contact_labels.write() {
            for (&id, labels) in &snap.alliance_contact_labels {
                esi_al.insert(id, labels.clone());
            }
        }
        for &(char_id, mail_id) in &snap.deleted_mails {
            ctx.esi_state.mark_mail_deleted(char_id, mail_id);
        }
        if let Ok(mut esi_ml) = ctx.esi_state.mail_labels.write() {
            for (&char_id, labels) in &snap.mail_labels {
                let entry = esi_ml
                    .entry(char_id)
                    .or_insert_with(EsiRuntimeState::default_mail_labels);
                for l in labels {
                    if let Some(ex) = entry.iter_mut().find(|x| x.label_id == l.label_id) {
                        ex.name = l.name.clone();
                        ex.color = l.color.clone();
                    } else {
                        entry.push(EsiMailLabel {
                            label_id: l.label_id,
                            name: l.name.clone(),
                            color: l.color.clone(),
                        });
                    }
                }
            }
        }
        for &(char_id, mail_id) in &snap.read_mails {
            let mut meta = ctx.esi_state.get_mail_meta(char_id, mail_id, false);
            meta.is_read = true;
            if let Some(lbls) = snap.mail_assigned_labels.get(&(char_id, mail_id)) {
                meta.labels = lbls.clone();
            }
            ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
        }
        for (&(char_id, mail_id), lbls) in &snap.mail_assigned_labels {
            let mut meta = ctx.esi_state.get_mail_meta(char_id, mail_id, false);
            if snap.read_mails.contains(&(char_id, mail_id)) {
                meta.is_read = true;
            }
            meta.labels = lbls.clone();
            ctx.esi_state.set_mail_meta(char_id, mail_id, meta);
        }
        if let Ok(mut esi_lists) = ctx.esi_state.mail_lists.write() {
            for ml in snap.mailing_lists.values() {
                for &member_id in ml.members.keys() {
                    let v = esi_lists.entry(member_id).or_default();
                    if !v.iter().any(|(id, _)| *id == ml.list_id) {
                        v.push((ml.list_id, ml.name.clone()));
                    }
                }
            }
        }
        if let (Ok(mut esi_evs), Ok(mut esi_resps)) = (
            ctx.esi_state.calendar_events.write(),
            ctx.esi_state.calendar_responses.write(),
        ) {
            for (&eid, ev) in &snap.calendar_events {
                if ev.is_deleted {
                    continue;
                }
                let event_date_unix = if ev.event_date_time > 100_000_000_000_000_000 {
                    ((ev.event_date_time / 10_000_000) - 11_644_473_600).max(0) as u64
                } else {
                    ev.event_date_time.max(0) as u64
                };
                let owner_type = match ev.scope.as_str() {
                    "Corporation" => "corporation",
                    "Alliance" => "alliance",
                    _ => "character",
                }
                .to_string();
                let owner_name = snap
                    .characters
                    .get(&ev.owner_id)
                    .map(|c| c.name.clone())
                    .or_else(|| snap.corporations.get(&ev.owner_id).map(|c| c.name.clone()))
                    .or_else(|| snap.alliances.get(&ev.owner_id).map(|a| a.name.clone()))
                    .unwrap_or_else(|| format!("Owner {}", ev.owner_id));
                let mut attendees: Vec<i64> = ev.responses.keys().copied().collect();
                attendees.sort_unstable();
                esi_evs.insert(
                    eid,
                    EsiCalendarEvent {
                        event_id: eid,
                        event_date_unix,
                        title: ev.title.clone(),
                        text: ev.description.clone(),
                        duration_minutes: ev.duration,
                        importance: ev.importance,
                        owner_id: ev.owner_id,
                        owner_name,
                        owner_type,
                        attendees,
                    },
                );
                for (&cid, &status) in &ev.responses {
                    let resp_str = match status {
                        4 => "accepted",
                        2 => "declined",
                        5 => "tentative",
                        _ => "not_responded",
                    };
                    esi_resps.insert((cid, eid), resp_str.to_string());
                }
            }
        }
        (
            snap.service_state_blobs.get("fleet_join_requests").cloned(),
            snap.service_state_blobs.get("fleet_watchlist").cloned(),
            snap.service_state_blobs.get("fleet_options").cloned(),
        )
    });
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.join_requests.clear();
        if let Some(blob) = join_reqs_blob {
            if let Ok(map) = serde_json::from_str::<std::collections::HashMap<i64, Vec<i64>>>(&blob) {
                for (fid, members) in map {
                    ex.join_requests.insert(fid, members.into_iter().collect());
                }
            }
        }
        ex.watchlist.clear();
        ex.watchlist_loaded = true;
        if let Some(blob) = watchlist_blob {
            if let Ok(map) = serde_json::from_str::<std::collections::HashMap<i64, Vec<i64>>>(&blob) {
                for (wid, targets) in map {
                    ex.watchlist.insert(wid, targets.into_iter().collect());
                }
            }
        }
        ex.voice_enabled.clear();
        ex.loot_logging.clear();
        if let Some(blob) = options_blob {
            if let Ok((voice, loot)) = serde_json::from_str::<(
                std::collections::HashMap<i64, bool>,
                std::collections::HashMap<i64, bool>,
            )>(&blob)
            {
                ex.voice_enabled = voice;
                ex.loot_logging = loot;
            }
        }
    }
    super::fleet_social_notify::prune_offline_fleets_and_channels(ctx);
}

pub(crate) use super::fleet_social_corp_tail::{handle_kick_out_members, handle_update_members};

pub(super) fn eval_vote_case_winner(
    vc: &eve_db::CorpVoteCaseRecord,
    ballots: &HashMap<(i64, i64), eve_db::CorpVoteBallotRecord>,
) -> (bool, i64, i64) {
    let mut totals: HashMap<i64, i64> = HashMap::new();
    for b in ballots.values().filter(|b| b.case_id == vc.case_id) {
        *totals.entry(b.option_id).or_default() += b.weight;
    }
    if vc.options.len() > 2 {
        let mut best_opt = 1i64;
        let mut best_param = vc.parameter;
        let mut best_weight = 0i64;
        let mut runner_up = 0i64;
        for &(oid, _, param) in &vc.options {
            let w = totals.get(&oid).copied().unwrap_or(0);
            if param != 0 && w > best_weight {
                runner_up = runner_up.max(best_weight);
                best_weight = w;
                best_opt = oid;
                best_param = param;
            } else {
                runner_up = runner_up.max(w);
            }
        }
        for (&oid, &w) in &totals {
            if oid != best_opt {
                runner_up = runner_up.max(w);
            }
        }
        (best_weight > 0 && best_weight > runner_up, best_param, best_weight)
    } else {
        let yes = totals.get(&1).copied().unwrap_or(0);
        let no: i64 = totals
            .iter()
            .filter(|&(&oid, _)| oid != 1)
            .map(|(_, &w)| w)
            .sum();
        let param = vc
            .options
            .iter()
            .find(|o| o.0 == 1)
            .map(|o| o.2)
            .filter(|&p| p != 0)
            .unwrap_or(vc.parameter);
        (yes > 0 && yes > no, param, yes)
    }
}

pub(super) fn handle_vote_queries(
    ctx: &ServerContext,
    corp_id: i64,
    now_unix: u64,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if method == "GetVotes" {
        let case_id = args
            .get(1)
            .or_else(|| args.first())
            .and_then(|a| a.as_i64())
            .unwrap_or(1);
        let rows = ctx.db.with_snapshot(|snap| {
            snap.corp_vote_ballots
                .values()
                .filter(|b| b.case_id == case_id)
                .map(|b| {
                    vec![
                        PyRep::Integer(b.voter_id),
                        PyRep::Integer(b.case_id),
                        PyRep::Integer(b.option_id),
                        PyRep::Integer(b.weight),
                    ]
                })
                .collect::<Vec<_>>()
        });
        return Some(Ok(PyRep::rowset(
            vec!["characterID", "voteCaseID", "optionID", "weight"],
            rows,
        )));
    }
    if method == "GetVoteCaseOptions" {
        let (target_corp, case_id) = if args.len() >= 2 {
            (
                args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(-1),
            )
        } else {
            (corp_id, args.first().and_then(|a| a.as_i64()).unwrap_or(-1))
        };
        let res = ctx.db.with_snapshot(|snap| {
            let vc = snap
                .corp_vote_cases
                .get(&case_id)
                .filter(|c| c.corporation_id == target_corp)
                .ok_or_else(|| BridgeRpcError {
                    code: "VoteCaseNotFound".into(),
                    message: format!("Vote case {case_id} not found for corporation {target_corp}"),
                })?;
            let opts = if vc.options.is_empty() {
                vec![
                    (1, "Yes / Accept".to_string(), vc.parameter),
                    (2, "No / Reject".to_string(), 0),
                ]
            } else {
                vc.options.clone()
            };
            let rows = opts
                .into_iter()
                .map(|(oid, text, param)| {
                    vec![
                        PyRep::Integer(oid),
                        PyRep::Integer(case_id),
                        PyRep::String(text),
                        PyRep::Integer(param),
                    ]
                })
                .collect::<Vec<_>>();
            Ok(PyRep::rowset(
                vec!["optionID", "voteCaseID", "optionText", "parameter"],
                rows,
            ))
        });
        return Some(res);
    }

    let columns = vec![
        "voteCaseID",
        "corporationID",
        "voteCaseText",
        "description",
        "voteType",
        "parameter",
        "startDateTime",
        "endDateTime",
        "status",
        "votesFor",
        "inEffect",
    ];
    let format_case = |c: &eve_db::CorpVoteCaseRecord,
                       ballots: &HashMap<(i64, i64), eve_db::CorpVoteBallotRecord>|
     -> Vec<PyRep> {
        let (passed, win_param, win_votes) = eval_vote_case_winner(c, ballots);
        let status = if now_unix < c.end_ts { 2 } else { 1 };
        let param = if passed { win_param } else { c.parameter };
        vec![
            PyRep::Integer(c.case_id),
            PyRep::Integer(c.corporation_id),
            PyRep::String(c.text.clone()),
            PyRep::String(c.description.clone()),
            PyRep::Integer(c.vote_type),
            PyRep::Integer(param),
            PyRep::Integer(c.start_ts as i64),
            PyRep::Integer(c.end_ts as i64),
            PyRep::Integer(status),
            PyRep::Integer(win_votes),
            PyRep::Bool(c.executed),
        ]
    };

    if method == "GetVoteCase" {
        let (target_corp, case_id) = if args.len() >= 2 {
            (
                args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(-1),
            )
        } else {
            (corp_id, args.first().and_then(|a| a.as_i64()).unwrap_or(-1))
        };
        let res = ctx.db.with_snapshot(|snap| {
            let vc = snap
                .corp_vote_cases
                .get(&case_id)
                .filter(|c| c.corporation_id == target_corp)
                .ok_or_else(|| BridgeRpcError {
                    code: "VoteCaseNotFound".into(),
                    message: format!("Vote case {case_id} not found for corporation {target_corp}"),
                })?;
            Ok(PyRep::rowset(
                columns,
                vec![format_case(vc, &snap.corp_vote_ballots)],
            ))
        });
        return Some(res);
    }

    let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
    let status_arg = args.get(1);
    let max_len = args
        .get(2)
        .and_then(|a| a.as_i64())
        .filter(|&m| m > 0)
        .map(|m| m as usize);

    let rows = ctx.db.with_snapshot(|snap| {
        let mut cases: Vec<&eve_db::CorpVoteCaseRecord> = snap
            .corp_vote_cases
            .values()
            .filter(|c| {
                if c.corporation_id != target_corp {
                    return false;
                }
                if method == "GetCorpSanctionedActions" {
                    let (passed, _, _) = eval_vote_case_winner(c, &snap.corp_vote_ballots);
                    return match status_arg {
                        Some(PyRep::Integer(2)) | Some(PyRep::Bool(true)) => c.executed,
                        Some(PyRep::String(s))
                            if s.eq_ignore_ascii_case("in effect")
                                || s.eq_ignore_ascii_case("active")
                                || s.eq_ignore_ascii_case("executed") =>
                        {
                            c.executed
                        }
                        Some(PyRep::Integer(0)) => c.executed || (now_unix >= c.end_ts && passed),
                        Some(PyRep::String(s)) if s.eq_ignore_ascii_case("all") => {
                            c.executed || (now_unix >= c.end_ts && passed)
                        }
                        _ => !c.executed && now_unix >= c.end_ts && passed,
                    };
                }
                match status_arg {
                    Some(PyRep::Integer(2)) => now_unix < c.end_ts,
                    Some(PyRep::String(s)) if s.eq_ignore_ascii_case("open") => {
                        now_unix < c.end_ts
                    }
                    Some(PyRep::Integer(1)) => now_unix >= c.end_ts,
                    Some(PyRep::String(s)) if s.eq_ignore_ascii_case("closed") => {
                        now_unix >= c.end_ts
                    }
                    _ => true,
                }
            })
            .collect();
        cases.sort_by(|a, b| {
            b.start_ts
                .cmp(&a.start_ts)
                .then_with(|| b.case_id.cmp(&a.case_id))
        });
        if let Some(m) = max_len {
            cases.truncate(m);
        }
        cases
            .into_iter()
            .map(|c| format_case(c, &snap.corp_vote_ballots))
            .collect::<Vec<_>>()
    });
    Some(Ok(PyRep::rowset(columns, rows)))
}