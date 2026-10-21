//! Per-`ServerContext` runtime state for ESI endpoints whose mutable metadata extends beyond
//! the compact `WorldStateSnapshot` tables (mail read/label/tombstone state, contact lists and
//! labels, calendar responses, fleet MOTD/wing/squad/member hierarchy, and remote client UI events).

#![forbid(unsafe_code)]

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::RwLock;

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
    pub deleted_mails: RwLock<HashSet<(i64, i64)>>,
    pub mail_meta: RwLock<HashMap<(i64, i64), EsiMailMeta>>,
    pub mail_labels: RwLock<HashMap<i64, Vec<EsiMailLabel>>>,
    pub mail_lists: RwLock<HashMap<i64, Vec<(i64, String)>>>,
    pub char_contacts: RwLock<HashMap<i64, Vec<EsiContactEntry>>>,
    pub char_contact_labels: RwLock<HashMap<i64, Vec<(i64, String)>>>,
    pub corp_contacts: RwLock<HashMap<i64, Vec<EsiContactEntry>>>,
    pub corp_contact_labels: RwLock<HashMap<i64, Vec<(i64, String)>>>,
    pub alliance_contacts: RwLock<HashMap<i64, Vec<EsiContactEntry>>>,
    pub alliance_contact_labels: RwLock<HashMap<i64, Vec<(i64, String)>>>,
    pub calendar_events: RwLock<HashMap<i64, EsiCalendarEvent>>,
    pub calendar_responses: RwLock<HashMap<(i64, i64), String>>,
    pub contact_notifications: RwLock<HashMap<i64, Vec<EsiContactNotification>>>,
    pub char_corp_history: RwLock<HashMap<i64, Vec<EsiCorpHistoryEntry>>>,
    pub char_titles: RwLock<HashMap<i64, Vec<(i64, String)>>>,
    pub fleet_motd: RwLock<HashMap<i64, String>>,
    pub fleet_wing_names: RwLock<HashMap<(i64, i64), String>>,
    pub fleet_squads: RwLock<HashMap<(i64, i64), (i64, String)>>,
    pub fleet_member_meta: RwLock<HashMap<(i64, i64), (String, i64, i64)>>,
    pub autopilot_waypoints: RwLock<HashMap<i64, Vec<i64>>>,
    pub ui_open_window_events: RwLock<HashMap<i64, Vec<Value>>>,
    pub war_allies: RwLock<HashMap<i64, Vec<Value>>>,
    pub custom_item_names: RwLock<HashMap<i64, String>>,
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
            deleted_mails: RwLock::new(HashSet::new()),
            mail_meta: RwLock::new(HashMap::new()),
            mail_labels: RwLock::new(HashMap::new()),
            mail_lists: RwLock::new(HashMap::new()),
            char_contacts: RwLock::new(HashMap::new()),
            char_contact_labels: RwLock::new(HashMap::new()),
            corp_contacts: RwLock::new(HashMap::new()),
            corp_contact_labels: RwLock::new(HashMap::new()),
            alliance_contacts: RwLock::new(HashMap::new()),
            alliance_contact_labels: RwLock::new(HashMap::new()),
            calendar_events: RwLock::new(HashMap::new()),
            calendar_responses: RwLock::new(HashMap::new()),
            contact_notifications: RwLock::new(HashMap::new()),
            char_corp_history: RwLock::new(HashMap::new()),
            char_titles: RwLock::new(HashMap::new()),
            fleet_motd: RwLock::new(HashMap::new()),
            fleet_wing_names: RwLock::new(HashMap::new()),
            fleet_squads: RwLock::new(HashMap::new()),
            fleet_member_meta: RwLock::new(HashMap::new()),
            autopilot_waypoints: RwLock::new(HashMap::new()),
            ui_open_window_events: RwLock::new(HashMap::new()),
            war_allies: RwLock::new(HashMap::new()),
            custom_item_names: RwLock::new(HashMap::new()),
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

    pub fn update_autopilot_waypoint(
        &self,
        char_id: i64,
        destination_id: i64,
        add_to_beginning: bool,
        clear_other_waypoints: bool,
    ) -> Vec<i64> {
        let mut map = self
            .autopilot_waypoints
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let list = map.entry(char_id).or_default();
        if clear_other_waypoints {
            list.clear();
            list.push(destination_id);
        } else if add_to_beginning {
            list.insert(0, destination_id);
        } else {
            list.push(destination_id);
        }
        list.clone()
    }

    pub fn record_ui_event(&self, char_id: i64, event: Value) {
        self.ui_open_window_events
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(char_id)
            .or_default()
            .push(event);
    }

    pub fn get_ui_events(&self, char_id: i64) -> Vec<Value> {
        self.ui_open_window_events
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_autopilot_waypoints(&self, char_id: i64) -> Vec<i64> {
        self.autopilot_waypoints
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .cloned()
            .unwrap_or_default()
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
