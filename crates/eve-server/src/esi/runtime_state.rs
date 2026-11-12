//! Per-`ServerContext` runtime state for ESI endpoints whose mutable metadata extends beyond
//! the compact `WorldStateSnapshot` tables (mail read/label/tombstone state, contact lists and
//! labels, calendar responses, fleet MOTD/wing/squad/member hierarchy, and remote client UI events).

#![forbid(unsafe_code)]

use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Debug, Clone)]
pub struct EsiMailLabel {
    pub label_id: i64,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone)]
pub struct EsiMailMeta {
    pub is_read: bool,
    pub labels: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct EsiContactEntry {
    pub contact_id: i64,
    pub contact_type: String,
    pub standing: f64,
    pub is_watched: bool,
    pub is_blocked: bool,
    pub label_ids: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct EsiCalendarEvent {
    pub event_id: i64,
    pub event_date_unix: u64,
    pub title: String,
    pub text: String,
    pub duration_minutes: i64,
    pub importance: i64,
    pub owner_id: i64,
    pub owner_name: String,
    pub owner_type: String,
    pub attendees: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct EsiContactNotification {
    pub notification_id: i64,
    pub sender_character_id: i64,
    pub send_date_unix: u64,
    pub standing_level: f64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct EsiCorpHistoryEntry {
    pub record_id: i64,
    pub corporation_id: i64,
    pub start_date_unix: u64,
    pub is_deleted: bool,
}

#[derive(Debug)]
pub struct EsiRuntimeState {
    pub deleted_mails: RankedRwLock<HashSet<(i64, i64)>, RANK_LEAF>,
    pub mail_meta: RankedRwLock<HashMap<(i64, i64), EsiMailMeta>, RANK_LEAF>,
    pub mail_labels: RankedRwLock<HashMap<i64, Vec<EsiMailLabel>>, RANK_LEAF>,
    /// Legacy empty map retained only for integration test `.clear()` compatibility;
    /// mailing lists are persisted canonically in `WorldStateSnapshot::mailing_lists`.
    pub mail_lists: RankedRwLock<HashMap<i64, Vec<(i64, String)>>, RANK_LEAF>,
    pub char_contacts: RankedRwLock<HashMap<i64, Vec<EsiContactEntry>>, RANK_LEAF>,
    /// Legacy empty maps retained only for integration test `.clear()` compatibility;
    /// contact labels and corp/alliance contacts live canonically in `WorldStateSnapshot`.
    pub char_contact_labels: RankedRwLock<HashMap<i64, Vec<(i64, String)>>, RANK_LEAF>,
    pub corp_contacts: RankedRwLock<HashMap<i64, Vec<EsiContactEntry>>, RANK_LEAF>,
    pub corp_contact_labels: RankedRwLock<HashMap<i64, Vec<(i64, String)>>, RANK_LEAF>,
    pub alliance_contacts: RankedRwLock<HashMap<i64, Vec<EsiContactEntry>>, RANK_LEAF>,
    pub alliance_contact_labels: RankedRwLock<HashMap<i64, Vec<(i64, String)>>, RANK_LEAF>,
    pub calendar_events: RankedRwLock<HashMap<i64, EsiCalendarEvent>, RANK_LEAF>,
    pub calendar_responses: RankedRwLock<HashMap<(i64, i64), String>, RANK_LEAF>,
    pub contact_notifications: RankedRwLock<HashMap<i64, Vec<EsiContactNotification>>, RANK_LEAF>,
    pub char_corp_history: RankedRwLock<HashMap<i64, Vec<EsiCorpHistoryEntry>>, RANK_LEAF>,
    pub char_titles: RankedRwLock<HashMap<i64, Vec<(i64, String)>>, RANK_LEAF>,
    pub fleet_motd: RankedRwLock<HashMap<i64, String>, RANK_LEAF>,
    pub fleet_wing_names: RankedRwLock<HashMap<(i64, i64), String>, RANK_LEAF>,
    pub fleet_squads: RankedRwLock<HashMap<(i64, i64), (i64, String)>, RANK_LEAF>,
    pub fleet_member_meta: RankedRwLock<HashMap<(i64, i64), (String, i64, i64)>, RANK_LEAF>,
    pub war_allies: RankedRwLock<HashMap<i64, Vec<Value>>, RANK_LEAF>,
    pub custom_item_names: RankedRwLock<HashMap<i64, String>, RANK_LEAF>,
    pub next_custom_id: AtomicI64,
}

impl Default for EsiRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl EsiRuntimeState {
    pub fn new() -> Self {
        Self {
            deleted_mails: RankedRwLock::new(HashSet::new()),
            mail_meta: RankedRwLock::new(HashMap::new()),
            mail_labels: RankedRwLock::new(HashMap::new()),
            mail_lists: RankedRwLock::new(HashMap::new()),
            char_contacts: RankedRwLock::new(HashMap::new()),
            char_contact_labels: RankedRwLock::new(HashMap::new()),
            corp_contacts: RankedRwLock::new(HashMap::new()),
            corp_contact_labels: RankedRwLock::new(HashMap::new()),
            alliance_contacts: RankedRwLock::new(HashMap::new()),
            alliance_contact_labels: RankedRwLock::new(HashMap::new()),
            calendar_events: RankedRwLock::new(HashMap::new()),
            calendar_responses: RankedRwLock::new(HashMap::new()),
            contact_notifications: RankedRwLock::new(HashMap::new()),
            char_corp_history: RankedRwLock::new(HashMap::new()),
            char_titles: RankedRwLock::new(HashMap::new()),
            fleet_motd: RankedRwLock::new(HashMap::new()),
            fleet_wing_names: RankedRwLock::new(HashMap::new()),
            fleet_squads: RankedRwLock::new(HashMap::new()),
            fleet_member_meta: RankedRwLock::new(HashMap::new()),
            war_allies: RankedRwLock::new(HashMap::new()),
            custom_item_names: RankedRwLock::new(HashMap::new()),
            next_custom_id: AtomicI64::new(16),
        }
    }

    pub fn alloc_id(&self) -> i64 {
        self.next_custom_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn default_mail_labels() -> Vec<EsiMailLabel> {
        vec![
            EsiMailLabel {
                label_id: 1,
                name: "Inbox".to_string(),
                color: "#ffffff".to_string(),
            },
            EsiMailLabel {
                label_id: 2,
                name: "Sent".to_string(),
                color: "#ffffff".to_string(),
            },
            EsiMailLabel {
                label_id: 4,
                name: "Corp".to_string(),
                color: "#0099ff".to_string(),
            },
            EsiMailLabel {
                label_id: 8,
                name: "Alliance".to_string(),
                color: "#00ff33".to_string(),
            },
        ]
    }

    pub fn is_mail_deleted(&self, char_id: i64, mail_id: i64) -> bool {
        self.deleted_mails
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(char_id, mail_id))
    }

    pub fn mark_mail_deleted(&self, char_id: i64, mail_id: i64) {
        self.deleted_mails
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((char_id, mail_id));
    }

    pub fn get_mail_meta(&self, char_id: i64, mail_id: i64, is_sender: bool) -> EsiMailMeta {
        if let Some(m) = self
            .mail_meta
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(char_id, mail_id))
            .cloned()
        {
            return m;
        }
        EsiMailMeta {
            is_read: is_sender,
            labels: vec![if is_sender { 2 } else { 1 }],
        }
    }

    pub fn set_mail_meta(&self, char_id: i64, mail_id: i64, meta: EsiMailMeta) {
        self.mail_meta
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((char_id, mail_id), meta);
    }

    pub fn set_custom_item_name(&self, item_id: i64, name: impl Into<String>) {
        self.custom_item_names
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(item_id, name.into());
    }

    pub fn get_custom_item_name(&self, item_id: i64) -> Option<String> {
        self.custom_item_names
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&item_id)
            .cloned()
    }
}

#[cfg(test)]
#[path = "runtime_state_unit_tests.rs"]
mod runtime_state_unit_tests;

#[cfg(test)]
#[path = "runtime_state_edges_tests.rs"]
mod runtime_state_edges_tests;

