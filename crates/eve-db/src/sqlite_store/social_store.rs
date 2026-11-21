//! Relational `domain_rows` SQLite persistence for Family 3 (`social`).
//!
//! Normalizes all 24 persisted social collections across mail state, mailing lists, calendar events,
//! bookmarks/access groups, chat channels & ACLs, social contacts/labels/notifications, and
//! character item notes into canonical `domain_rows` rows (`social.mail`, `social.bookmarks`,
//! `social.chat_calendar`, `social.contacts_notes`), with zero-clone `ChangeSet::diff` incremental
//! writes, one-shot startup migration and drop of the 19 legacy `0004_d6_social_durability` tables,
//! and fail-closed reload/rollback.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, Transaction};
use serde::{de::DeserializeOwned, Deserialize};

#[cfg(test)]
use serde::Serialize;

use crate::durable_domain_types::SocialContactNotificationRecord;
use crate::snapshot_ext::{
    CalendarEventRecord, ChatChannelAclRecord, MailingListRecord, SocialContactRecord,
    WorldStateSnapshot,
};
use crate::sqlite_store::domain_rows::{
    decode_map_entry, decode_set_entry, diff_map_with, diff_set_with, fetch_domain_row_entries,
    from_checked_domain_json, table_exists, DomainRowChange, DomainSyncState,
    DOMAIN_SOCIAL_BOOKMARKS, DOMAIN_SOCIAL_CHAT_CALENDAR, DOMAIN_SOCIAL_CONTACTS_NOTES,
    DOMAIN_SOCIAL_MAIL,
};
use crate::sqlite_store::quarantine::{
    row_col_string, row_composite_key_string, row_payload_summary, LoadContext,
};
use crate::types::{BookmarkFolderRecord, BookmarkRecord, ChatChannelRecord, MailLabelRecord};

#[cfg(test)]
pub(crate) const LEGACY_D6_SOCIAL_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS read_mails (
    character_id INTEGER NOT NULL,
    mail_id INTEGER NOT NULL,
    PRIMARY KEY (character_id, mail_id)
);

CREATE TABLE IF NOT EXISTS deleted_mails (
    character_id INTEGER NOT NULL,
    mail_id INTEGER NOT NULL,
    PRIMARY KEY (character_id, mail_id)
);

CREATE TABLE IF NOT EXISTS trashed_mails (
    character_id INTEGER NOT NULL,
    mail_id INTEGER NOT NULL,
    PRIMARY KEY (character_id, mail_id)
);

CREATE TABLE IF NOT EXISTS mail_labels (
    character_id INTEGER PRIMARY KEY,
    labels_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS mail_assigned_labels (
    character_id INTEGER NOT NULL,
    mail_id INTEGER NOT NULL,
    labels_json TEXT NOT NULL,
    PRIMARY KEY (character_id, mail_id)
);

CREATE TABLE IF NOT EXISTS mailing_lists (
    list_id INTEGER PRIMARY KEY,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS calendar_events (
    event_id INTEGER PRIMARY KEY,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS bookmark_meta (
    bookmark_id INTEGER PRIMARY KEY,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS bookmark_folders (
    folder_id INTEGER PRIMARY KEY,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chat_channels (
    channel_id TEXT PRIMARY KEY,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS char_contacts (
    character_id INTEGER PRIMARY KEY,
    contacts_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS char_contact_notifications (
    character_id INTEGER PRIMARY KEY,
    notifications_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS char_contact_labels (
    character_id INTEGER PRIMARY KEY,
    labels_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS corp_contacts (
    corporation_id INTEGER PRIMARY KEY,
    contacts_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS corp_contact_labels (
    corporation_id INTEGER PRIMARY KEY,
    labels_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS alliance_contacts (
    alliance_id INTEGER PRIMARY KEY,
    contacts_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS alliance_contact_labels (
    alliance_id INTEGER PRIMARY KEY,
    labels_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS char_notes (
    character_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    note TEXT NOT NULL,
    PRIMARY KEY (character_id, item_id)
);

CREATE TABLE IF NOT EXISTS access_groups (
    group_id INTEGER PRIMARY KEY,
    members_json TEXT NOT NULL
);
"#;

const LEGACY_SOCIAL_TABLES: &[&str] = &[
    "read_mails",
    "deleted_mails",
    "trashed_mails",
    "mail_labels",
    "mail_assigned_labels",
    "mailing_lists",
    "calendar_events",
    "bookmark_meta",
    "bookmark_folders",
    "chat_channels",
    "char_contacts",
    "char_contact_notifications",
    "char_contact_labels",
    "corp_contacts",
    "corp_contact_labels",
    "alliance_contacts",
    "alliance_contact_labels",
    "char_notes",
    "access_groups",
];

#[derive(Debug, Default, Clone, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
struct PersistedBookmarkMeta {
    subfolder_id: Option<i64>,
    expiry_unix: Option<u64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
struct PersistedBookmarkFolder {
    record: BookmarkFolderRecord,
    access_group_ids: Vec<i64>,
    known_by: Vec<(i64, bool)>,
    parent_folder_id: Option<i64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
struct PersistedChatChannel {
    record: ChatChannelRecord,
    acl: ChatChannelAclRecord,
    standalone_acl_only: bool,
}

#[derive(Debug, Default)]
struct StagedSocialState {
    read_mails: HashSet<(i64, i64)>,
    deleted_mails: HashSet<(i64, i64)>,
    trashed_mails: HashSet<(i64, i64)>,
    mail_labels: HashMap<i64, Vec<MailLabelRecord>>,
    mail_assigned_labels: HashMap<(i64, i64), Vec<i64>>,
    mailing_lists: HashMap<i64, MailingListRecord>,
    calendar_events: HashMap<i64, CalendarEventRecord>,
    bookmark_folders: HashMap<i64, BookmarkFolderRecord>,
    bookmark_subfolders: HashMap<i64, i64>,
    bookmark_subfolder_parents: HashMap<i64, i64>,
    bookmark_expiries: HashMap<i64, u64>,
    bookmark_folder_acls: HashMap<i64, Vec<i64>>,
    access_groups: HashMap<i64, Vec<i64>>,
    bookmark_folder_known_by: HashMap<(i64, i64), bool>,
    chat_channels: HashMap<String, ChatChannelRecord>,
    chat_channel_acl: HashMap<String, ChatChannelAclRecord>,
    char_contacts: HashMap<i64, Vec<SocialContactRecord>>,
    char_contact_notifications: HashMap<i64, Vec<SocialContactNotificationRecord>>,
    char_contact_labels: HashMap<i64, Vec<(i64, String)>>,
    corp_contacts: HashMap<i64, Vec<SocialContactRecord>>,
    corp_contact_labels: HashMap<i64, Vec<(i64, String, u32)>>,
    alliance_contacts: HashMap<i64, Vec<SocialContactRecord>>,
    alliance_contact_labels: HashMap<i64, Vec<(i64, String)>>,
    char_notes: HashMap<(i64, i64), String>,
}

/// Returns `true` when `acl` carries at least one allow, deny, or mute rule and therefore warrants
/// a durable row in `domain_rows`.
pub(crate) fn is_chat_acl_non_empty(acl: &ChatChannelAclRecord) -> bool {
    !acl.allow_list.is_empty() || !acl.deny_list.is_empty() || !acl.muted_until.is_empty()
}

fn load_domain_entries<F>(
    conn: &Connection,
    domain: &'static str,
    ctx: &mut LoadContext,
    mut apply: F,
) -> Result<(), String>
where
    F: FnMut(&str, &str, &str) -> Result<(), String>,
{
    for (rowid, raw_key, raw_val) in fetch_domain_row_entries(conn, domain)? {
        let res = match raw_key.split_once('/') {
            Some((field, subkey)) => apply(field, subkey, &raw_val),
            None => Err(format!("invalid domain_rows key '{raw_key}' in '{domain}'")),
        };
        if let Err(err) = res {
            ctx.record_bad_row("domain_rows", domain, raw_key, raw_val, err, Some(rowid))?;
        }
    }
    Ok(())
}

macro_rules! social_domain_ops {
    (
        $diff_fn:ident,
        $sync_fn:ident,
        $load_fn:ident,
        $domain:expr,
        sets: [$($set_field:ident),* $(,)?],
        maps: [$($map_field:ident),* $(,)?]
    ) => {
        fn $diff_fn(
            before: &WorldStateSnapshot,
            after: &WorldStateSnapshot,
            out: &mut Vec<DomainRowChange>,
        ) {
            $(
                if !before.$set_field.ptr_eq(&after.$set_field) {
                    diff_set_with(
                        $domain,
                        stringify!($set_field),
                        before.$set_field.len(),
                        after.$set_field.iter(),
                        |k| before.$set_field.contains(k),
                        before.$set_field.iter(),
                        |k| after.$set_field.contains(k),
                        out,
                    );
                }
            )*
            $(
                if !before.$map_field.ptr_eq(&after.$map_field) {
                    diff_map_with(
                        $domain,
                        stringify!($map_field),
                        before.$map_field.len(),
                        after.$map_field.iter(),
                        |k| before.$map_field.get(k),
                        before.$map_field.keys(),
                        |k| after.$map_field.contains_key(k),
                        out,
                    );
                }
            )*
        }

        fn $sync_fn(tx: &Transaction<'_>, snap: &WorldStateSnapshot) -> Result<bool, String> {
            let mut state = DomainSyncState::load(tx, $domain)?;
            $(
                state.push_set(stringify!($set_field), snap.$set_field.iter())?;
            )*
            $(
                state.push_map(stringify!($map_field), snap.$map_field.iter())?;
            )*
            state.finish(tx)
        }

        fn $load_fn(
            conn: &Connection,
            staged: &mut StagedSocialState,
            ctx: &mut LoadContext,
        ) -> Result<(), String> {
            load_domain_entries(conn, $domain, ctx, |field, subkey, raw_val| match field {
                $(
                    stringify!($set_field) => {
                        let key = decode_set_entry(subkey, raw_val)?;
                        staged.$set_field.insert(key);
                        Ok(())
                    }
                )*
                $(
                    stringify!($map_field) => {
                        let (key, val) = decode_map_entry(subkey, raw_val)?;
                        staged.$map_field.insert(key, val);
                        Ok(())
                    }
                )*
                _ => Err(format!("unknown field '{field}' in domain '{}'", $domain)),
            })
        }
    };
}

macro_rules! define_social_domains {
    (
        $(
            ($diff_fn:ident, $sync_fn:ident, $load_fn:ident, $domain:ident,
             sets: [$($set_field:ident),* $(,)?],
             maps: [$($map_field:ident),* $(,)?])
        ),+ $(,)?;
        chat_calendar: [$cal_ev:ident, $chat_ch:ident, $chat_acl:ident $(,)?]
    ) => {
        $(
            social_domain_ops!(
                $diff_fn,
                $sync_fn,
                $load_fn,
                $domain,
                sets: [$($set_field),*],
                maps: [$($map_field),*]
            );
        )+

        fn apply_staged_social(staged: StagedSocialState, snap: &mut WorldStateSnapshot) {
            let StagedSocialState {
                $($($set_field,)* $($map_field,)*)+
                $cal_ev,
                $chat_ch,
                $chat_acl,
            } = staged;
            $(
                $(snap.$set_field = $set_field.into();)*
                $(snap.$map_field = $map_field.into();)*
            )+
            snap.$cal_ev = $cal_ev.into();
            snap.$chat_ch = $chat_ch.into();
            snap.$chat_acl = $chat_acl.into();
        }
    };
}

define_social_domains!(
    (
        diff_social_mail,
        sync_social_mail,
        load_social_mail_domain_rows,
        DOMAIN_SOCIAL_MAIL,
        sets: [read_mails, deleted_mails, trashed_mails],
        maps: [mail_labels, mail_assigned_labels, mailing_lists]
    ),
    (
        diff_social_bookmarks,
        sync_social_bookmarks,
        load_social_bookmarks_domain_rows,
        DOMAIN_SOCIAL_BOOKMARKS,
        sets: [],
        maps: [
            bookmark_folders,
            bookmark_subfolders,
            bookmark_subfolder_parents,
            bookmark_expiries,
            bookmark_folder_acls,
            access_groups,
            bookmark_folder_known_by,
        ]
    ),
    (
        diff_social_contacts_notes,
        sync_social_contacts_notes,
        load_social_contacts_notes_domain_rows,
        DOMAIN_SOCIAL_CONTACTS_NOTES,
        sets: [],
        maps: [
            char_contacts,
            char_contact_notifications,
            char_contact_labels,
            corp_contacts,
            corp_contact_labels,
            alliance_contacts,
            alliance_contact_labels,
            char_notes,
        ]
    );
    chat_calendar: [calendar_events, chat_channels, chat_channel_acl]
);

fn diff_social_chat_calendar(
    before: &WorldStateSnapshot,
    after: &WorldStateSnapshot,
    out: &mut Vec<DomainRowChange>,
) {
    if !before.calendar_events.ptr_eq(&after.calendar_events) {
        diff_map_with(
            DOMAIN_SOCIAL_CHAT_CALENDAR,
            "calendar_events",
            before.calendar_events.len(),
            after.calendar_events.iter(),
            |k| before.calendar_events.get(k),
            before.calendar_events.keys(),
            |k| after.calendar_events.contains_key(k),
            out,
        );
    }
    if !before.chat_channels.ptr_eq(&after.chat_channels) {
        diff_map_with(
            DOMAIN_SOCIAL_CHAT_CALENDAR,
            "chat_channels",
            before.chat_channels.len(),
            after.chat_channels.iter(),
            |k| before.chat_channels.get(k),
            before.chat_channels.keys(),
            |k| after.chat_channels.contains_key(k),
            out,
        );
    }
    if !before.chat_channel_acl.ptr_eq(&after.chat_channel_acl) {
        diff_map_with(
            DOMAIN_SOCIAL_CHAT_CALENDAR,
            "chat_channel_acl",
            before.chat_channel_acl.len(),
            after
                .chat_channel_acl
                .iter()
                .filter(|(_, acl)| is_chat_acl_non_empty(acl)),
            |k| {
                before
                    .chat_channel_acl
                    .get(k)
                    .filter(|acl| is_chat_acl_non_empty(acl))
            },
            before
                .chat_channel_acl
                .iter()
                .filter_map(|(k, acl)| is_chat_acl_non_empty(acl).then_some(k)),
            |k| {
                after
                    .chat_channel_acl
                    .get(k)
                    .is_some_and(is_chat_acl_non_empty)
            },
            out,
        );
    }
}

fn sync_social_chat_calendar(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    let mut state = DomainSyncState::load(tx, DOMAIN_SOCIAL_CHAT_CALENDAR)?;
    state.push_map("calendar_events", snap.calendar_events.iter())?;
    state.push_map("chat_channels", snap.chat_channels.iter())?;
    state.push_map(
        "chat_channel_acl",
        snap.chat_channel_acl
            .iter()
            .filter(|(_, acl)| is_chat_acl_non_empty(acl)),
    )?;
    state.finish(tx)
}

fn load_social_chat_calendar_domain_rows(
    conn: &Connection,
    staged: &mut StagedSocialState,
    ctx: &mut LoadContext,
) -> Result<(), String> {
    load_domain_entries(
        conn,
        DOMAIN_SOCIAL_CHAT_CALENDAR,
        ctx,
        |field, subkey, raw_val| match field {
            "calendar_events" => {
                let (key, val) = decode_map_entry(subkey, raw_val)?;
                staged.calendar_events.insert(key, val);
                Ok(())
            }
            "chat_channels" => {
                let (key, val) = decode_map_entry(subkey, raw_val)?;
                staged.chat_channels.insert(key, val);
                Ok(())
            }
            "chat_channel_acl" => {
                let (key, val) = decode_map_entry::<String, ChatChannelAclRecord>(subkey, raw_val)?;
                if is_chat_acl_non_empty(&val) {
                    staged.chat_channel_acl.insert(key, val);
                }
                Ok(())
            }
            _ => Err(format!(
                "unknown field '{field}' in domain '{DOMAIN_SOCIAL_CHAT_CALENDAR}'"
            )),
        },
    )
}

/// Appends per-row `domain_rows` diffs for all 24 Family 3 (`social`) collections without cloning
/// cold fingerprint maps.
pub(crate) fn collect_social_domain_row_diffs(
    before: &WorldStateSnapshot,
    after: &WorldStateSnapshot,
    out: &mut Vec<DomainRowChange>,
) {
    diff_social_mail(before, after, out);
    diff_social_bookmarks(before, after, out);
    diff_social_chat_calendar(before, after, out);
    diff_social_contacts_notes(before, after, out);
}

/// Synchronizes all 4 Family 3 (`social`) `domain_rows` domains against `snap`.
pub(crate) fn sync_social_domain_rows(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    let mut changed = false;
    changed |= sync_social_mail(tx, snap)?;
    changed |= sync_social_bookmarks(tx, snap)?;
    changed |= sync_social_chat_calendar(tx, snap)?;
    changed |= sync_social_contacts_notes(tx, snap)?;
    Ok(changed)
}

fn load_legacy_pair_set(
    conn: &Connection,
    ctx: &mut LoadContext,
    table: &'static str,
    dst: &mut HashSet<(i64, i64)>,
) -> Result<bool, String> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let sql = format!("SELECT rowid, character_id, mail_id FROM {table}");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let res: Result<(i64, i64), String> = (|| {
            Ok((
                r.get::<_, i64>(1).map_err(|e| e.to_string())?,
                r.get::<_, i64>(2).map_err(|e| e.to_string())?,
            ))
        })();
        match res {
            Ok(pair) => {
                dst.insert(pair);
            }
            Err(err) => {
                let key = row_composite_key_string(r, 1, 2);
                let payload = row_payload_summary(r, 1, 2);
                ctx.record_bad_row(table, table, key, payload, err, rowid)?;
            }
        }
    }
    Ok(true)
}

fn load_legacy_i64_json_table<T: DeserializeOwned>(
    conn: &Connection,
    ctx: &mut LoadContext,
    table: &'static str,
    pk_col: &str,
    json_col: &str,
    mut on_entry: impl FnMut(i64, T),
) -> Result<bool, String> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let sql = format!("SELECT rowid, {pk_col}, {json_col} FROM {table}");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let res: Result<(i64, T), String> = (|| {
            let id: i64 = r.get(1).map_err(|e| e.to_string())?;
            let raw: String = r.get(2).map_err(|e| e.to_string())?;
            let val: T = from_checked_domain_json(&raw)?;
            Ok((id, val))
        })();
        match res {
            Ok((id, val)) => on_entry(id, val),
            Err(err) => {
                let key = row_col_string(r, 1);
                let payload = row_col_string(r, 2);
                ctx.record_bad_row(table, table, key, payload, err, rowid)?;
            }
        }
    }
    Ok(true)
}

fn load_legacy_pair_table<T>(
    conn: &Connection,
    ctx: &mut LoadContext,
    table: &'static str,
    cols: (&str, &str, &str),
    parse: impl Fn(&str) -> Result<T, String>,
    dst: &mut HashMap<(i64, i64), T>,
) -> Result<bool, String> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let (k1_col, k2_col, val_col) = cols;
    let sql = format!("SELECT rowid, {k1_col}, {k2_col}, {val_col} FROM {table}");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let res: Result<((i64, i64), T), String> = (|| {
            let k1: i64 = r.get(1).map_err(|e| e.to_string())?;
            let k2: i64 = r.get(2).map_err(|e| e.to_string())?;
            let raw: String = r.get(3).map_err(|e| e.to_string())?;
            let val = parse(&raw)?;
            Ok(((k1, k2), val))
        })();
        match res {
            Ok((k, val)) => {
                dst.insert(k, val);
            }
            Err(err) => {
                let key = row_composite_key_string(r, 1, 2);
                let payload = row_col_string(r, 3);
                ctx.record_bad_row(table, table, key, payload, err, rowid)?;
            }
        }
    }
    Ok(true)
}

fn load_legacy_chat_channels(
    conn: &Connection,
    ctx: &mut LoadContext,
    staged: &mut StagedSocialState,
) -> Result<bool, String> {
    if !table_exists(conn, "chat_channels")? {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare("SELECT rowid, channel_id, record_json FROM chat_channels")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let res: Result<(String, PersistedChatChannel), String> = (|| {
            let cid: String = r.get(1).map_err(|e| e.to_string())?;
            let raw: String = r.get(2).map_err(|e| e.to_string())?;
            let persisted: PersistedChatChannel = from_checked_domain_json(&raw)?;
            Ok((cid, persisted))
        })();
        match res {
            Ok((cid, persisted)) => {
                if !persisted.standalone_acl_only {
                    staged.chat_channels.insert(cid.clone(), persisted.record);
                }
                if is_chat_acl_non_empty(&persisted.acl) {
                    staged.chat_channel_acl.insert(cid, persisted.acl);
                }
            }
            Err(err) => {
                let key = row_col_string(r, 1);
                let payload = row_col_string(r, 2);
                ctx.record_bad_row("chat_channels", "chat_channels", key, payload, err, rowid)?;
            }
        }
    }
    Ok(true)
}

fn load_legacy_social_tables(
    conn: &Connection,
    bookmarks: &HashMap<i64, BookmarkRecord>,
    ctx: &mut LoadContext,
    staged: &mut StagedSocialState,
) -> Result<bool, String> {
    let mut had_legacy = false;
    had_legacy |= load_legacy_pair_set(conn, ctx, "read_mails", &mut staged.read_mails)?;
    had_legacy |= load_legacy_pair_set(conn, ctx, "deleted_mails", &mut staged.deleted_mails)?;
    had_legacy |= load_legacy_pair_set(conn, ctx, "trashed_mails", &mut staged.trashed_mails)?;
    had_legacy |= load_legacy_i64_json_table::<Vec<MailLabelRecord>>(
        conn,
        ctx,
        "mail_labels",
        "character_id",
        "labels_json",
        |cid, val| {
            staged.mail_labels.insert(cid, val);
        },
    )?;
    had_legacy |= load_legacy_pair_table(
        conn,
        ctx,
        "mail_assigned_labels",
        ("character_id", "mail_id", "labels_json"),
        from_checked_domain_json::<Vec<i64>>,
        &mut staged.mail_assigned_labels,
    )?;
    had_legacy |= load_legacy_i64_json_table::<MailingListRecord>(
        conn,
        ctx,
        "mailing_lists",
        "list_id",
        "record_json",
        |id, val| {
            staged.mailing_lists.insert(id, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<CalendarEventRecord>(
        conn,
        ctx,
        "calendar_events",
        "event_id",
        "record_json",
        |id, val| {
            staged.calendar_events.insert(id, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<PersistedBookmarkMeta>(
        conn,
        ctx,
        "bookmark_meta",
        "bookmark_id",
        "record_json",
        |bid, meta| {
            // Preserve the legacy invariant that orphaned bookmark subfolder entries are not
            // revived once their owning bookmark record has been deleted or swept.
            if bookmarks.contains_key(&bid) {
                if let Some(sf) = meta.subfolder_id {
                    staged.bookmark_subfolders.insert(bid, sf);
                }
            }
            if let Some(exp) = meta.expiry_unix {
                if exp > 0 {
                    staged.bookmark_expiries.insert(bid, exp);
                }
            }
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<PersistedBookmarkFolder>(
        conn,
        ctx,
        "bookmark_folders",
        "folder_id",
        "record_json",
        |fid, persisted| {
            staged.bookmark_folders.insert(fid, persisted.record);
            if !persisted.access_group_ids.is_empty() {
                staged
                    .bookmark_folder_acls
                    .insert(fid, persisted.access_group_ids);
            }
            for (cid, active) in persisted.known_by {
                staged.bookmark_folder_known_by.insert((fid, cid), active);
            }
            if let Some(pfid) = persisted.parent_folder_id {
                staged.bookmark_subfolder_parents.insert(fid, pfid);
            }
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<i64>>(
        conn,
        ctx,
        "access_groups",
        "group_id",
        "members_json",
        |gid, members| {
            staged.access_groups.insert(gid, members);
        },
    )?;
    had_legacy |= load_legacy_chat_channels(conn, ctx, staged)?;
    had_legacy |= load_legacy_i64_json_table::<Vec<SocialContactRecord>>(
        conn,
        ctx,
        "char_contacts",
        "character_id",
        "contacts_json",
        |cid, val| {
            staged.char_contacts.insert(cid, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<SocialContactNotificationRecord>>(
        conn,
        ctx,
        "char_contact_notifications",
        "character_id",
        "notifications_json",
        |cid, val| {
            staged.char_contact_notifications.insert(cid, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<(i64, String)>>(
        conn,
        ctx,
        "char_contact_labels",
        "character_id",
        "labels_json",
        |cid, val| {
            staged.char_contact_labels.insert(cid, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<SocialContactRecord>>(
        conn,
        ctx,
        "corp_contacts",
        "corporation_id",
        "contacts_json",
        |id, val| {
            staged.corp_contacts.insert(id, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<(i64, String, u32)>>(
        conn,
        ctx,
        "corp_contact_labels",
        "corporation_id",
        "labels_json",
        |id, val| {
            staged.corp_contact_labels.insert(id, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<SocialContactRecord>>(
        conn,
        ctx,
        "alliance_contacts",
        "alliance_id",
        "contacts_json",
        |id, val| {
            staged.alliance_contacts.insert(id, val);
        },
    )?;
    had_legacy |= load_legacy_i64_json_table::<Vec<(i64, String)>>(
        conn,
        ctx,
        "alliance_contact_labels",
        "alliance_id",
        "labels_json",
        |id, val| {
            staged.alliance_contact_labels.insert(id, val);
        },
    )?;
    had_legacy |= load_legacy_pair_table(
        conn,
        ctx,
        "char_notes",
        ("character_id", "item_id", "note"),
        |raw| Ok(raw.to_owned()),
        &mut staged.char_notes,
    )?;
    Ok(had_legacy)
}

fn load_staged_social(
    conn: &Connection,
    bookmarks: &HashMap<i64, BookmarkRecord>,
    ctx: &mut LoadContext,
) -> Result<(StagedSocialState, bool), String> {
    let mut staged = StagedSocialState::default();
    // Load legacy 0004_d6_social tables first, then overlay canonical `domain_rows` entries so
    // any already-migrated rows take precedence over stale legacy records.
    let had_legacy = load_legacy_social_tables(conn, bookmarks, ctx, &mut staged)?;
    load_social_mail_domain_rows(conn, &mut staged, ctx)?;
    load_social_bookmarks_domain_rows(conn, &mut staged, ctx)?;
    load_social_chat_calendar_domain_rows(conn, &mut staged, ctx)?;
    load_social_contacts_notes_domain_rows(conn, &mut staged, ctx)?;
    Ok((staged, had_legacy))
}

/// Loads all 24 Family 3 (`social`) collections from `domain_rows` (and any unmigrated legacy
/// `0004_d6_social` tables) into `snap`, returning `Ok(true)` if any legacy tables existed.
///
/// Fails closed without mutating `snap` on any error when `ctx.quarantine_unreadable` is `false`.
pub fn load_social_domain_rows(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut LoadContext,
) -> Result<bool, String> {
    let (staged, had_legacy) = load_staged_social(conn, &snap.bookmarks, ctx)?;
    apply_staged_social(staged, snap);
    Ok(had_legacy)
}

/// Drops the retired legacy `0004_d6_social_durability` SQLite tables once migrated into
/// `domain_rows`.
pub(crate) fn drop_legacy_social_tables(tx: &Transaction<'_>) -> Result<(), String> {
    for tbl in LEGACY_SOCIAL_TABLES {
        tx.execute(&format!("DROP TABLE IF EXISTS {tbl}"), [])
            .map_err(|e| format!("drop legacy social table '{tbl}': {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "social_store_edges_tests.rs"]
mod social_store_edges_tests;
