//! Relational SQLite persistence for `D-6` Batch 2 social durability (`0004_d6_social_durability`).
//!
//! Persists character/corporation/alliance contacts and contact labels, mail read/deleted/trash
//! flags and assigned labels, mailing lists, calendar events, bookmark extended metadata and
//! bookmark folders (including subfolders and ACLs), player-owned chat channels and ACLs, and
//! character/owner notes across server restarts.

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::{params, Connection, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::fingerprints::{fp_json, to_checked_json_string, TableFingerprints};
use crate::durable_domain_types::SocialContactNotificationRecord;
use crate::snapshot_ext::{
    CalendarEventRecord, ChatChannelAclRecord, MailingListRecord, SocialContactRecord,
    WorldStateSnapshot,
};
use crate::types::{BookmarkFolderRecord, ChatChannelRecord, MailLabelRecord};

pub(super) const D6_SOCIAL_SCHEMA_VERSION: i64 = 4;
pub(super) const D6_SOCIAL_MIGRATION_NAME: &str = "0004_d6_social_durability";

pub(super) const DDL_0004_D6_SOCIAL: &str = r#"
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

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PersistedBookmarkMeta {
    subfolder_id: Option<i64>,
    expiry_unix: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PersistedBookmarkFolder {
    record: BookmarkFolderRecord,
    access_group_ids: Vec<i64>,
    known_by: Vec<(i64, bool)>,
    parent_folder_id: Option<i64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PersistedChatChannel {
    record: ChatChannelRecord,
    acl: ChatChannelAclRecord,
    standalone_acl_only: bool,
}

fn is_chat_acl_non_empty(acl: &ChatChannelAclRecord) -> bool {
    !acl.allow_list.is_empty() || !acl.deny_list.is_empty() || !acl.muted_until.is_empty()
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct D6SocialFingerprints {
    pub(crate) read_mails: HashSet<(i64, i64)>,
    pub(crate) deleted_mails: HashSet<(i64, i64)>,
    pub(crate) trashed_mails: HashSet<(i64, i64)>,
    pub(crate) mail_labels: HashMap<i64, u64>,
    pub(crate) mail_assigned_labels: HashMap<(i64, i64), u64>,
    pub(crate) mailing_lists: HashMap<i64, u64>,
    pub(crate) calendar_events: HashMap<i64, u64>,
    pub(crate) bookmark_meta: HashMap<i64, u64>,
    pub(crate) bookmark_folders: HashMap<i64, u64>,
    pub(crate) access_groups: HashMap<i64, u64>,
    pub(crate) chat_channels: HashMap<String, u64>,
    pub(crate) char_contacts: HashMap<i64, u64>,
    pub(crate) char_contact_notifications: HashMap<i64, u64>,
    pub(crate) char_contact_labels: HashMap<i64, u64>,
    pub(crate) corp_contacts: HashMap<i64, u64>,
    pub(crate) corp_contact_labels: HashMap<i64, u64>,
    pub(crate) alliance_contacts: HashMap<i64, u64>,
    pub(crate) alliance_contact_labels: HashMap<i64, u64>,
    pub(crate) char_notes: HashMap<(i64, i64), u64>,
}

pub(super) fn apply_d6_social_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(DDL_0004_D6_SOCIAL)
        .map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![D6_SOCIAL_SCHEMA_VERSION, D6_SOCIAL_MIGRATION_NAME, now_unix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn fp_keyed_map<K: Copy + Eq + std::hash::Hash, V: Serialize>(map: &HashMap<K, V>) -> HashMap<K, u64> {
    map.iter().map(|(&k, v)| (k, fp_json(v))).collect()
}

fn build_persisted_bookmark_meta(snap: &WorldStateSnapshot, bid: i64) -> PersistedBookmarkMeta {
    PersistedBookmarkMeta {
        subfolder_id: snap.bookmark_subfolders.get(&bid).copied(),
        expiry_unix: snap.bookmark_expiries.get(&bid).copied(),
    }
}

fn compute_bookmark_meta_fps(snap: &WorldStateSnapshot) -> HashMap<i64, u64> {
    let mut ids: BTreeSet<i64> = snap.bookmark_subfolders.keys().copied().collect();
    ids.extend(snap.bookmark_expiries.keys().copied());
    ids.into_iter()
        .map(|bid| (bid, fp_json(&build_persisted_bookmark_meta(snap, bid))))
        .collect()
}

fn build_persisted_bookmark_folder(
    snap: &WorldStateSnapshot,
    fid: i64,
    folder: &BookmarkFolderRecord,
) -> PersistedBookmarkFolder {
    let mut known_by: Vec<(i64, bool)> = snap
        .bookmark_folder_known_by
        .iter()
        .filter_map(|(&(f, c), &active)| (f == fid).then_some((c, active)))
        .collect();
    known_by.sort_unstable_by_key(|&(c, _)| c);
    PersistedBookmarkFolder {
        record: folder.clone(),
        access_group_ids: snap
            .bookmark_folder_acls
            .get(&fid)
            .cloned()
            .unwrap_or_default(),
        known_by,
        parent_folder_id: snap.bookmark_subfolder_parents.get(&fid).copied(),
    }
}

fn compute_bookmark_folders_fps(snap: &WorldStateSnapshot) -> HashMap<i64, u64> {
    snap.bookmark_folders
        .iter()
        .map(|(&fid, folder)| (fid, fp_json(&build_persisted_bookmark_folder(snap, fid, folder))))
        .collect()
}

fn build_persisted_chat_channel(snap: &WorldStateSnapshot, cid: &str) -> Option<PersistedChatChannel> {
    if let Some(chan) = snap.chat_channels.get(cid) {
        Some(PersistedChatChannel {
            record: chan.clone(),
            acl: snap
                .chat_channel_acl
                .get(cid)
                .cloned()
                .unwrap_or_default(),
            standalone_acl_only: false,
        })
    } else if let Some(acl) = snap.chat_channel_acl.get(cid).filter(|a| is_chat_acl_non_empty(a)) {
        Some(PersistedChatChannel {
            record: ChatChannelRecord {
                channel_id: cid.to_string(),
                name: cid.to_string(),
                motd: String::new(),
                owner_id: None,
                members: Default::default(),
                is_system_local: false,
                solar_system_id: None,
                is_private: false,
                operators: Default::default(),
                password: None,
                is_delayed_local: false,
            },
            acl: acl.clone(),
            standalone_acl_only: true,
        })
    } else {
        None
    }
}

fn compute_chat_channels_fps(snap: &WorldStateSnapshot) -> HashMap<String, u64> {
    let mut out = HashMap::with_capacity(snap.chat_channels.len() + snap.chat_channel_acl.len());
    for (cid, chan) in &snap.chat_channels {
        let persisted = PersistedChatChannel {
            record: chan.clone(),
            acl: snap
                .chat_channel_acl
                .get(cid)
                .cloned()
                .unwrap_or_default(),
            standalone_acl_only: false,
        };
        out.insert(cid.clone(), fp_json(&persisted));
    }
    for (cid, acl) in &snap.chat_channel_acl {
        if snap.chat_channels.contains_key(cid) || !is_chat_acl_non_empty(acl) {
            continue;
        }
        if let Some(persisted) = build_persisted_chat_channel(snap, cid) {
            out.insert(cid.clone(), fp_json(&persisted));
        }
    }
    out
}

pub(super) fn compute_d6_social_fp(snap: &WorldStateSnapshot) -> D6SocialFingerprints {
    D6SocialFingerprints {
        read_mails: snap.read_mails.clone(),
        deleted_mails: snap.deleted_mails.clone(),
        trashed_mails: snap.trashed_mails.clone(),
        mail_labels: fp_keyed_map(&snap.mail_labels),
        mail_assigned_labels: fp_keyed_map(&snap.mail_assigned_labels),
        mailing_lists: fp_keyed_map(&snap.mailing_lists),
        calendar_events: fp_keyed_map(&snap.calendar_events),
        bookmark_meta: compute_bookmark_meta_fps(snap),
        bookmark_folders: compute_bookmark_folders_fps(snap),
        access_groups: fp_keyed_map(&snap.access_groups),
        chat_channels: compute_chat_channels_fps(snap),
        char_contacts: fp_keyed_map(&snap.char_contacts),
        char_contact_notifications: fp_keyed_map(&snap.char_contact_notifications),
        char_contact_labels: fp_keyed_map(&snap.char_contact_labels),
        corp_contacts: fp_keyed_map(&snap.corp_contacts),
        corp_contact_labels: fp_keyed_map(&snap.corp_contact_labels),
        alliance_contacts: fp_keyed_map(&snap.alliance_contacts),
        alliance_contact_labels: fp_keyed_map(&snap.alliance_contact_labels),
        char_notes: fp_keyed_map(&snap.char_notes),
    }
}

#[cfg(test)]
pub(super) fn load_d6_social_into_snapshot(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
) -> Result<(), String> {
    let mut ctx = super::quarantine::LoadContext::new(false);
    load_d6_social_into_snapshot_with_ctx(conn, snap, &mut ctx)
}

pub(super) fn load_d6_social_into_snapshot_with_ctx(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    use super::quarantine::{row_col_string, row_composite_key_string, row_payload_summary};

    fn load_pair_set(
        conn: &Connection,
        ctx: &mut super::quarantine::LoadContext,
        table: &'static str,
        dst: &mut HashSet<(i64, i64)>,
    ) -> Result<(), String> {
        dst.clear();
        let sql = format!("SELECT rowid, character_id, mail_id FROM {table}");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let res: Result<(i64, i64), String> =
                (|| Ok((r.get::<_, i64>(1).map_err(|e| e.to_string())?, r.get::<_, i64>(2).map_err(|e| e.to_string())?)))();
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
        Ok(())
    }

    fn load_i64_json_map<T: DeserializeOwned>(
        conn: &Connection,
        ctx: &mut super::quarantine::LoadContext,
        table: &'static str,
        pk_col: &str,
        json_col: &str,
        dst: &mut HashMap<i64, T>,
    ) -> Result<(), String> {
        dst.clear();
        let sql = format!("SELECT rowid, {pk_col}, {json_col} FROM {table}");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let res: Result<(i64, T), String> = (|| {
                let id: i64 = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let val: T = serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok((id, val))
            })();
            match res {
                Ok((id, val)) => {
                    dst.insert(id, val);
                }
                Err(err) => {
                    let key = row_col_string(r, 1);
                    let payload = row_col_string(r, 2);
                    ctx.record_bad_row(table, table, key, payload, err, rowid)?;
                }
            }
        }
        Ok(())
    }

    // 1. read_mails, deleted_mails, trashed_mails
    load_pair_set(conn, ctx, "read_mails", &mut snap.read_mails)?;
    load_pair_set(conn, ctx, "deleted_mails", &mut snap.deleted_mails)?;
    load_pair_set(conn, ctx, "trashed_mails", &mut snap.trashed_mails)?;

    // 2. mail_labels & mail_assigned_labels
    load_i64_json_map::<Vec<MailLabelRecord>>(
        conn,
        ctx,
        "mail_labels",
        "character_id",
        "labels_json",
        &mut snap.mail_labels,
    )?;
    snap.mail_assigned_labels.clear();
    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, mail_id, labels_json FROM mail_assigned_labels")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let res: Result<((i64, i64), Vec<i64>), String> = (|| {
                let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let mid: i64 = r.get(2).map_err(|e| e.to_string())?;
                let js: String = r.get(3).map_err(|e| e.to_string())?;
                let labels: Vec<i64> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok(((cid, mid), labels))
            })();
            match res {
                Ok((k, labels)) => {
                    snap.mail_assigned_labels.insert(k, labels);
                }
                Err(err) => {
                    let key = row_composite_key_string(r, 1, 2);
                    let payload = row_col_string(r, 3);
                    ctx.record_bad_row(
                        "mail_assigned_labels",
                        "mail_assigned_labels",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    // 3. mailing_lists & calendar_events
    load_i64_json_map::<MailingListRecord>(
        conn,
        ctx,
        "mailing_lists",
        "list_id",
        "record_json",
        &mut snap.mailing_lists,
    )?;
    load_i64_json_map::<CalendarEventRecord>(
        conn,
        ctx,
        "calendar_events",
        "event_id",
        "record_json",
        &mut snap.calendar_events,
    )?;

    // 4. bookmark_meta & bookmark_folders
    snap.bookmark_subfolders.clear();
    snap.bookmark_expiries.clear();
    {
        let mut meta_map = HashMap::<i64, PersistedBookmarkMeta>::new();
        load_i64_json_map(
            conn,
            ctx,
            "bookmark_meta",
            "bookmark_id",
            "record_json",
            &mut meta_map,
        )?;
        for (bid, meta) in meta_map {
            if snap.bookmarks.contains_key(&bid) {
                if let Some(sf) = meta.subfolder_id {
                    snap.bookmark_subfolders.insert(bid, sf);
                }
            }
            if let Some(exp) = meta.expiry_unix {
                if exp > 0 {
                    snap.bookmark_expiries.insert(bid, exp);
                }
            }
        }
    }
    snap.bookmark_folders.clear();
    snap.bookmark_folder_acls.clear();
    snap.bookmark_folder_known_by.clear();
    snap.bookmark_subfolder_parents.clear();
    {
        let mut folder_map = HashMap::<i64, PersistedBookmarkFolder>::new();
        load_i64_json_map(
            conn,
            ctx,
            "bookmark_folders",
            "folder_id",
            "record_json",
            &mut folder_map,
        )?;
        for (fid, persisted) in folder_map {
            snap.bookmark_folders.insert(fid, persisted.record);
            if !persisted.access_group_ids.is_empty() {
                snap.bookmark_folder_acls
                    .insert(fid, persisted.access_group_ids);
            }
            for (cid, active) in persisted.known_by {
                snap.bookmark_folder_known_by.insert((fid, cid), active);
            }
            if let Some(pfid) = persisted.parent_folder_id {
                snap.bookmark_subfolder_parents.insert(fid, pfid);
            }
        }
    }

    load_i64_json_map::<Vec<i64>>(
        conn,
        ctx,
        "access_groups",
        "group_id",
        "members_json",
        &mut snap.access_groups,
    )?;

    // 5. chat_channels & chat_channel_acl
    snap.chat_channels.clear();
    snap.chat_channel_acl.clear();
    {
        let mut stmt = conn
            .prepare("SELECT rowid, channel_id, record_json FROM chat_channels")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let res: Result<(String, PersistedChatChannel), String> = (|| {
                let cid: String = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let persisted: PersistedChatChannel =
                    serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok((cid, persisted))
            })();
            match res {
                Ok((cid, persisted)) => {
                    if !persisted.standalone_acl_only {
                        snap.chat_channels.insert(cid.clone(), persisted.record);
                    }
                    if is_chat_acl_non_empty(&persisted.acl) {
                        snap.chat_channel_acl.insert(cid, persisted.acl);
                    }
                }
                Err(err) => {
                    let key = row_col_string(r, 1);
                    let payload = row_col_string(r, 2);
                    ctx.record_bad_row("chat_channels", "chat_channels", key, payload, err, rowid)?;
                }
            }
        }
    }

    // 6. char/corp/alliance contacts and contact labels
    load_i64_json_map::<Vec<SocialContactRecord>>(
        conn,
        ctx,
        "char_contacts",
        "character_id",
        "contacts_json",
        &mut snap.char_contacts,
    )?;
    load_i64_json_map::<Vec<SocialContactNotificationRecord>>(
        conn,
        ctx,
        "char_contact_notifications",
        "character_id",
        "notifications_json",
        &mut snap.char_contact_notifications,
    )?;
    load_i64_json_map::<Vec<(i64, String)>>(
        conn,
        ctx,
        "char_contact_labels",
        "character_id",
        "labels_json",
        &mut snap.char_contact_labels,
    )?;
    load_i64_json_map::<Vec<SocialContactRecord>>(
        conn,
        ctx,
        "corp_contacts",
        "corporation_id",
        "contacts_json",
        &mut snap.corp_contacts,
    )?;
    load_i64_json_map::<Vec<(i64, String, u32)>>(
        conn,
        ctx,
        "corp_contact_labels",
        "corporation_id",
        "labels_json",
        &mut snap.corp_contact_labels,
    )?;
    load_i64_json_map::<Vec<SocialContactRecord>>(
        conn,
        ctx,
        "alliance_contacts",
        "alliance_id",
        "contacts_json",
        &mut snap.alliance_contacts,
    )?;
    load_i64_json_map::<Vec<(i64, String)>>(
        conn,
        ctx,
        "alliance_contact_labels",
        "alliance_id",
        "labels_json",
        &mut snap.alliance_contact_labels,
    )?;

    // 7. char_notes
    snap.char_notes.clear();
    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, item_id, note FROM char_notes")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let res: Result<((i64, i64), String), String> = (|| {
                let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let iid: i64 = r.get(2).map_err(|e| e.to_string())?;
                let note: String = r.get(3).map_err(|e| e.to_string())?;
                Ok(((cid, iid), note))
            })();
            match res {
                Ok((k, note)) => {
                    snap.char_notes.insert(k, note);
                }
                Err(err) => {
                    let key = row_composite_key_string(r, 1, 2);
                    let payload = row_col_string(r, 3);
                    ctx.record_bad_row("char_notes", "char_notes", key, payload, err, rowid)?;
                }
            }
        }
    }

    Ok(())
}

fn sync_pair_set_table(
    tx: &Transaction<'_>,
    table: &str,
    snap_set: &HashSet<(i64, i64)>,
    fp_set: &mut HashSet<(i64, i64)>,
) -> Result<bool, String> {
    if snap_set == fp_set {
        return Ok(false);
    }
    let del_sql = format!("DELETE FROM {table} WHERE character_id = ?1 AND mail_id = ?2");
    let ins_sql = format!("INSERT OR IGNORE INTO {table} (character_id, mail_id) VALUES (?1, ?2)");
    let removed: Vec<(i64, i64)> = fp_set.difference(snap_set).copied().collect();
    for (cid, mid) in removed {
        tx.execute(&del_sql, params![cid, mid])
            .map_err(|e| e.to_string())?;
        fp_set.remove(&(cid, mid));
    }
    let added: Vec<(i64, i64)> = snap_set.difference(fp_set).copied().collect();
    for (cid, mid) in added {
        tx.execute(&ins_sql, params![cid, mid])
            .map_err(|e| e.to_string())?;
        fp_set.insert((cid, mid));
    }
    Ok(true)
}

fn sync_i64_json_table<V: Serialize>(
    tx: &Transaction<'_>,
    table: &str,
    pk_col: &str,
    json_col: &str,
    snap_map: &HashMap<i64, V>,
    fp_map: &mut HashMap<i64, u64>,
) -> Result<bool, String> {
    let mut changed = false;
    let del_sql = format!("DELETE FROM {table} WHERE {pk_col} = ?1");
    let ups_sql = format!("INSERT OR REPLACE INTO {table} ({pk_col}, {json_col}) VALUES (?1, ?2)");
    let removed: Vec<i64> = fp_map
        .keys()
        .filter(|k| !snap_map.contains_key(*k))
        .copied()
        .collect();
    for k in removed {
        changed = true;
        tx.execute(&del_sql, params![k])
            .map_err(|e| e.to_string())?;
        fp_map.remove(&k);
    }
    for (&k, v) in snap_map {
        let h = fp_json(v);
        if fp_map.get(&k) != Some(&h) {
            changed = true;
            let js = to_checked_json_string(v)?;
            tx.execute(&ups_sql, params![k, js])
                .map_err(|e| e.to_string())?;
            fp_map.insert(k, h);
        }
    }
    Ok(changed)
}

pub(super) fn sync_d6_social(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut staged = fp.d6_social.clone();
    let mut changed = false;

    changed |= sync_pair_set_table(tx, "read_mails", &snap.read_mails, &mut staged.read_mails)?;
    changed |= sync_pair_set_table(
        tx,
        "deleted_mails",
        &snap.deleted_mails,
        &mut staged.deleted_mails,
    )?;
    changed |= sync_pair_set_table(
        tx,
        "trashed_mails",
        &snap.trashed_mails,
        &mut staged.trashed_mails,
    )?;

    changed |= sync_i64_json_table(
        tx,
        "mail_labels",
        "character_id",
        "labels_json",
        &snap.mail_labels,
        &mut staged.mail_labels,
    )?;

    {
        let removed: Vec<(i64, i64)> = staged
            .mail_assigned_labels
            .keys()
            .filter(|k| !snap.mail_assigned_labels.contains_key(*k))
            .copied()
            .collect();
        for (cid, mid) in removed {
            changed = true;
            tx.execute(
                "DELETE FROM mail_assigned_labels WHERE character_id = ?1 AND mail_id = ?2",
                params![cid, mid],
            )
            .map_err(|e| e.to_string())?;
            staged.mail_assigned_labels.remove(&(cid, mid));
        }
        for (&(cid, mid), labels) in &snap.mail_assigned_labels {
            let h = fp_json(labels);
            if staged.mail_assigned_labels.get(&(cid, mid)) != Some(&h) {
                changed = true;
                let js = to_checked_json_string(labels)?;
                tx.execute(
                    "INSERT OR REPLACE INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES (?1, ?2, ?3)",
                    params![cid, mid, js],
                )
                .map_err(|e| e.to_string())?;
                staged.mail_assigned_labels.insert((cid, mid), h);
            }
        }
    }

    changed |= sync_i64_json_table(
        tx,
        "mailing_lists",
        "list_id",
        "record_json",
        &snap.mailing_lists,
        &mut staged.mailing_lists,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "calendar_events",
        "event_id",
        "record_json",
        &snap.calendar_events,
        &mut staged.calendar_events,
    )?;

    {
        let mut meta_ids: BTreeSet<i64> = snap.bookmark_subfolders.keys().copied().collect();
        meta_ids.extend(snap.bookmark_expiries.keys().copied());
        let removed: Vec<i64> = staged
            .bookmark_meta
            .keys()
            .filter(|k| !meta_ids.contains(k))
            .copied()
            .collect();
        for bid in removed {
            changed = true;
            tx.execute(
                "DELETE FROM bookmark_meta WHERE bookmark_id = ?1",
                params![bid],
            )
            .map_err(|e| e.to_string())?;
            staged.bookmark_meta.remove(&bid);
        }
        for bid in meta_ids {
            let meta = build_persisted_bookmark_meta(snap, bid);
            let h = fp_json(&meta);
            if staged.bookmark_meta.get(&bid) != Some(&h) {
                changed = true;
                let js = to_checked_json_string(&meta)?;
                tx.execute(
                    "INSERT OR REPLACE INTO bookmark_meta (bookmark_id, record_json) VALUES (?1, ?2)",
                    params![bid, js],
                )
                .map_err(|e| e.to_string())?;
                staged.bookmark_meta.insert(bid, h);
            }
        }
    }

    {
        let removed: Vec<i64> = staged
            .bookmark_folders
            .keys()
            .filter(|k| !snap.bookmark_folders.contains_key(*k))
            .copied()
            .collect();
        for fid in removed {
            changed = true;
            tx.execute(
                "DELETE FROM bookmark_folders WHERE folder_id = ?1",
                params![fid],
            )
            .map_err(|e| e.to_string())?;
            staged.bookmark_folders.remove(&fid);
        }
        for (&fid, folder) in &snap.bookmark_folders {
            let persisted = build_persisted_bookmark_folder(snap, fid, folder);
            let h = fp_json(&persisted);
            if staged.bookmark_folders.get(&fid) != Some(&h) {
                changed = true;
                let js = to_checked_json_string(&persisted)?;
                tx.execute(
                    "INSERT OR REPLACE INTO bookmark_folders (folder_id, record_json) VALUES (?1, ?2)",
                    params![fid, js],
                )
                .map_err(|e| e.to_string())?;
                staged.bookmark_folders.insert(fid, h);
            }
        }
    }

    changed |= sync_i64_json_table(
        tx,
        "access_groups",
        "group_id",
        "members_json",
        &snap.access_groups,
        &mut staged.access_groups,
    )?;

    {
        let next_chat_fps = compute_chat_channels_fps(snap);
        let removed: Vec<String> = staged
            .chat_channels
            .keys()
            .filter(|k| !next_chat_fps.contains_key(*k))
            .cloned()
            .collect();
        for cid in removed {
            changed = true;
            tx.execute(
                "DELETE FROM chat_channels WHERE channel_id = ?1",
                params![cid],
            )
            .map_err(|e| e.to_string())?;
            staged.chat_channels.remove(&cid);
        }
        for (cid, &h) in &next_chat_fps {
            if staged.chat_channels.get(cid) != Some(&h) {
                if let Some(persisted) = build_persisted_chat_channel(snap, cid) {
                    changed = true;
                    let js = to_checked_json_string(&persisted)?;
                    tx.execute(
                        "INSERT OR REPLACE INTO chat_channels (channel_id, record_json) VALUES (?1, ?2)",
                        params![cid, js],
                    )
                    .map_err(|e| e.to_string())?;
                    staged.chat_channels.insert(cid.clone(), h);
                }
            }
        }
    }

    changed |= sync_i64_json_table(
        tx,
        "char_contacts",
        "character_id",
        "contacts_json",
        &snap.char_contacts,
        &mut staged.char_contacts,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "char_contact_notifications",
        "character_id",
        "notifications_json",
        &snap.char_contact_notifications,
        &mut staged.char_contact_notifications,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "char_contact_labels",
        "character_id",
        "labels_json",
        &snap.char_contact_labels,
        &mut staged.char_contact_labels,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "corp_contacts",
        "corporation_id",
        "contacts_json",
        &snap.corp_contacts,
        &mut staged.corp_contacts,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "corp_contact_labels",
        "corporation_id",
        "labels_json",
        &snap.corp_contact_labels,
        &mut staged.corp_contact_labels,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "alliance_contacts",
        "alliance_id",
        "contacts_json",
        &snap.alliance_contacts,
        &mut staged.alliance_contacts,
    )?;
    changed |= sync_i64_json_table(
        tx,
        "alliance_contact_labels",
        "alliance_id",
        "labels_json",
        &snap.alliance_contact_labels,
        &mut staged.alliance_contact_labels,
    )?;

    {
        let removed: Vec<(i64, i64)> = staged
            .char_notes
            .keys()
            .filter(|k| !snap.char_notes.contains_key(*k))
            .copied()
            .collect();
        for (cid, iid) in removed {
            changed = true;
            tx.execute(
                "DELETE FROM char_notes WHERE character_id = ?1 AND item_id = ?2",
                params![cid, iid],
            )
            .map_err(|e| e.to_string())?;
            staged.char_notes.remove(&(cid, iid));
        }
        for (&(cid, iid), note) in &snap.char_notes {
            let h = fp_json(note);
            if staged.char_notes.get(&(cid, iid)) != Some(&h) {
                changed = true;
                tx.execute(
                    "INSERT OR REPLACE INTO char_notes (character_id, item_id, note) VALUES (?1, ?2, ?3)",
                    params![cid, iid, note],
                )
                .map_err(|e| e.to_string())?;
                staged.char_notes.insert((cid, iid), h);
            }
        }
    }

    fp.d6_social = staged;
    Ok(changed)
}

#[cfg(test)]
#[path = "social_store_edges_tests.rs"]
mod social_store_edges_tests;
