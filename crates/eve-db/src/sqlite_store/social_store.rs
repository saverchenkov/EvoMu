//! Relational SQLite persistence for `D-6` Batch 2 social durability (`0004_d6_social_durability`).
//!
//! Persists character/corporation/alliance contacts and contact labels, mail read/deleted/trash
//! flags and assigned labels, mailing lists, calendar events, bookmark extended metadata and
//! bookmark folders (including subfolders and ACLs), player-owned chat channels and ACLs, and
//! character/owner notes across server restarts.

use std::hash::Hash;

use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};

use super::fingerprints::{fnv1a_hasher, fp_json, TableFingerprints};
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
struct PersistedBookmarkMeta {
    #[serde(default)]
    subfolder_id: Option<i64>,
    #[serde(default)]
    expiry_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedBookmarkFolder {
    record: BookmarkFolderRecord,
    #[serde(default)]
    access_group_ids: Vec<i64>,
    #[serde(default)]
    known_by: Vec<(i64, bool)>,
    #[serde(default)]
    parent_folder_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedChatChannel {
    record: ChatChannelRecord,
    #[serde(default)]
    acl: ChatChannelAclRecord,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct D6SocialFingerprints {
    pub(crate) combined_hash: u64,
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

pub(super) fn compute_d6_social_fp(snap: &WorldStateSnapshot) -> D6SocialFingerprints {
    let mut h = fnv1a_hasher();

    let mut rm: Vec<(i64, i64)> = snap.read_mails.iter().copied().collect();
    rm.sort_unstable();
    rm.hash(&mut h);

    let mut dm: Vec<(i64, i64)> = snap.deleted_mails.iter().copied().collect();
    dm.sort_unstable();
    dm.hash(&mut h);

    let mut tm: Vec<(i64, i64)> = snap.trashed_mails.iter().copied().collect();
    tm.sort_unstable();
    tm.hash(&mut h);

    let mut ml_keys: Vec<i64> = snap.mail_labels.keys().copied().collect();
    ml_keys.sort_unstable();
    for k in ml_keys {
        k.hash(&mut h);
        fp_json(&snap.mail_labels[&k]).hash(&mut h);
    }

    let mut mal_keys: Vec<(i64, i64)> = snap.mail_assigned_labels.keys().copied().collect();
    mal_keys.sort_unstable();
    for k in mal_keys {
        k.hash(&mut h);
        snap.mail_assigned_labels[&k].hash(&mut h);
    }

    let mut mlist_keys: Vec<i64> = snap.mailing_lists.keys().copied().collect();
    mlist_keys.sort_unstable();
    for k in mlist_keys {
        k.hash(&mut h);
        fp_json(&snap.mailing_lists[&k]).hash(&mut h);
    }

    let mut cal_keys: Vec<i64> = snap.calendar_events.keys().copied().collect();
    cal_keys.sort_unstable();
    for k in cal_keys {
        k.hash(&mut h);
        fp_json(&snap.calendar_events[&k]).hash(&mut h);
    }

    let mut bm_keys: Vec<i64> = snap.bookmarks.keys().copied().collect();
    bm_keys.sort_unstable();
    for k in bm_keys {
        k.hash(&mut h);
        fp_json(&snap.bookmarks[&k]).hash(&mut h);
    }

    let mut bmsf_keys: Vec<i64> = snap.bookmark_subfolders.keys().copied().collect();
    bmsf_keys.sort_unstable();
    for k in bmsf_keys {
        k.hash(&mut h);
        snap.bookmark_subfolders[&k].hash(&mut h);
    }

    let mut bme_keys: Vec<i64> = snap.bookmark_expiries.keys().copied().collect();
    bme_keys.sort_unstable();
    for k in bme_keys {
        k.hash(&mut h);
        snap.bookmark_expiries[&k].hash(&mut h);
    }

    let mut bmf_keys: Vec<i64> = snap.bookmark_folders.keys().copied().collect();
    bmf_keys.sort_unstable();
    for k in bmf_keys {
        k.hash(&mut h);
        fp_json(&snap.bookmark_folders[&k]).hash(&mut h);
        snap.bookmark_folder_acls.get(&k).hash(&mut h);
        snap.bookmark_subfolder_parents.get(&k).hash(&mut h);
    }

    let mut ag_keys: Vec<i64> = snap.access_groups.keys().copied().collect();
    ag_keys.sort_unstable();
    for k in ag_keys {
        k.hash(&mut h);
        snap.access_groups.get(&k).hash(&mut h);
    }

    let mut kb_keys: Vec<(i64, i64)> = snap.bookmark_folder_known_by.keys().copied().collect();
    kb_keys.sort_unstable();
    for k in kb_keys {
        k.hash(&mut h);
        snap.bookmark_folder_known_by[&k].hash(&mut h);
    }

    let mut cc_keys: Vec<&String> = snap.chat_channels.keys().collect();
    cc_keys.sort_unstable();
    for k in cc_keys {
        k.hash(&mut h);
        fp_json(&snap.chat_channels[k]).hash(&mut h);
        if let Some(acl) = snap.chat_channel_acl.get(k) {
            fp_json(acl).hash(&mut h);
        }
    }

    let mut chc_keys: Vec<i64> = snap.char_contacts.keys().copied().collect();
    chc_keys.sort_unstable();
    for k in chc_keys {
        k.hash(&mut h);
        fp_json(&snap.char_contacts[&k]).hash(&mut h);
    }

    let mut chcl_keys: Vec<i64> = snap.char_contact_labels.keys().copied().collect();
    chcl_keys.sort_unstable();
    for k in chcl_keys {
        k.hash(&mut h);
        fp_json(&snap.char_contact_labels[&k]).hash(&mut h);
    }

    let mut coc_keys: Vec<i64> = snap.corp_contacts.keys().copied().collect();
    coc_keys.sort_unstable();
    for k in coc_keys {
        k.hash(&mut h);
        fp_json(&snap.corp_contacts[&k]).hash(&mut h);
    }

    let mut cocl_keys: Vec<i64> = snap.corp_contact_labels.keys().copied().collect();
    cocl_keys.sort_unstable();
    for k in cocl_keys {
        k.hash(&mut h);
        fp_json(&snap.corp_contact_labels[&k]).hash(&mut h);
    }

    let mut alc_keys: Vec<i64> = snap.alliance_contacts.keys().copied().collect();
    alc_keys.sort_unstable();
    for k in alc_keys {
        k.hash(&mut h);
        fp_json(&snap.alliance_contacts[&k]).hash(&mut h);
    }

    let mut alcl_keys: Vec<i64> = snap.alliance_contact_labels.keys().copied().collect();
    alcl_keys.sort_unstable();
    for k in alcl_keys {
        k.hash(&mut h);
        fp_json(&snap.alliance_contact_labels[&k]).hash(&mut h);
    }

    let mut cn_keys: Vec<(i64, i64)> = snap.char_notes.keys().copied().collect();
    cn_keys.sort_unstable();
    for k in cn_keys {
        k.hash(&mut h);
        snap.char_notes[&k].hash(&mut h);
    }

    D6SocialFingerprints {
        combined_hash: h.finish(),
    }
}

pub(super) fn load_d6_social_into_snapshot(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
) -> Result<(), String> {
    // 1. read_mails, deleted_mails, trashed_mails
    snap.read_mails.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, mail_id FROM read_mails")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            snap.read_mails.insert(row.map_err(|e| e.to_string())?);
        }
    }
    snap.deleted_mails.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, mail_id FROM deleted_mails")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            snap.deleted_mails.insert(row.map_err(|e| e.to_string())?);
        }
    }
    snap.trashed_mails.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, mail_id FROM trashed_mails")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            snap.trashed_mails.insert(row.map_err(|e| e.to_string())?);
        }
    }

    // 2. mail_labels & mail_assigned_labels
    snap.mail_labels.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, labels_json FROM mail_labels")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, js) = row.map_err(|e| e.to_string())?;
            let labels: Vec<MailLabelRecord> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.mail_labels.insert(cid, labels);
        }
    }
    snap.mail_assigned_labels.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, mail_id, labels_json FROM mail_assigned_labels")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, mid, js) = row.map_err(|e| e.to_string())?;
            let labels: Vec<i64> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.mail_assigned_labels.insert((cid, mid), labels);
        }
    }

    // 3. mailing_lists & calendar_events
    snap.mailing_lists.clear();
    {
        let mut stmt = conn
            .prepare("SELECT list_id, record_json FROM mailing_lists")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (lid, js) = row.map_err(|e| e.to_string())?;
            let rec: MailingListRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.mailing_lists.insert(lid, rec);
        }
    }
    snap.calendar_events.clear();
    {
        let mut stmt = conn
            .prepare("SELECT event_id, record_json FROM calendar_events")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (eid, js) = row.map_err(|e| e.to_string())?;
            let rec: CalendarEventRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.calendar_events.insert(eid, rec);
        }
    }

    // 4. bookmark_meta & bookmark_folders
    snap.bookmark_subfolders.clear();
    snap.bookmark_expiries.clear();
    {
        let mut stmt = conn
            .prepare("SELECT bookmark_id, record_json FROM bookmark_meta")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (bid, js) = row.map_err(|e| e.to_string())?;
            let meta: PersistedBookmarkMeta = serde_json::from_str(&js).map_err(|e| e.to_string())?;
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
        let mut stmt = conn
            .prepare("SELECT folder_id, record_json FROM bookmark_folders")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (fid, js) = row.map_err(|e| e.to_string())?;
            let persisted: PersistedBookmarkFolder =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
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

    snap.access_groups.clear();
    {
        let mut stmt = conn
            .prepare("SELECT group_id, members_json FROM access_groups")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (gid, js) = row.map_err(|e| e.to_string())?;
            let members: Vec<i64> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.access_groups.insert(gid, members);
        }
    }

    // 5. chat_channels & chat_channel_acl
    snap.chat_channels.clear();
    snap.chat_channel_acl.clear();
    {
        let mut stmt = conn
            .prepare("SELECT channel_id, record_json FROM chat_channels")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, js) = row.map_err(|e| e.to_string())?;
            let persisted: PersistedChatChannel =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.chat_channels.insert(cid.clone(), persisted.record);
            if !persisted.acl.allow_list.is_empty()
                || !persisted.acl.deny_list.is_empty()
                || !persisted.acl.muted_until.is_empty()
            {
                snap.chat_channel_acl.insert(cid, persisted.acl);
            }
        }
    }

    // 6. char/corp/alliance contacts and contact labels
    snap.char_contacts.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, contacts_json FROM char_contacts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, js) = row.map_err(|e| e.to_string())?;
            let list: Vec<SocialContactRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.char_contacts.insert(cid, list);
        }
    }
    snap.char_contact_labels.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, labels_json FROM char_contact_labels")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, js) = row.map_err(|e| e.to_string())?;
            let list: Vec<(i64, String)> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.char_contact_labels.insert(cid, list);
        }
    }
    snap.corp_contacts.clear();
    {
        let mut stmt = conn
            .prepare("SELECT corporation_id, contacts_json FROM corp_contacts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, js) = row.map_err(|e| e.to_string())?;
            let list: Vec<SocialContactRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.corp_contacts.insert(cid, list);
        }
    }
    snap.corp_contact_labels.clear();
    {
        let mut stmt = conn
            .prepare("SELECT corporation_id, labels_json FROM corp_contact_labels")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, js) = row.map_err(|e| e.to_string())?;
            let list: Vec<(i64, String, u32)> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.corp_contact_labels.insert(cid, list);
        }
    }
    snap.alliance_contacts.clear();
    {
        let mut stmt = conn
            .prepare("SELECT alliance_id, contacts_json FROM alliance_contacts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (aid, js) = row.map_err(|e| e.to_string())?;
            let list: Vec<SocialContactRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.alliance_contacts.insert(aid, list);
        }
    }
    snap.alliance_contact_labels.clear();
    {
        let mut stmt = conn
            .prepare("SELECT alliance_id, labels_json FROM alliance_contact_labels")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (aid, js) = row.map_err(|e| e.to_string())?;
            let list: Vec<(i64, String)> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.alliance_contact_labels.insert(aid, list);
        }
    }

    // 7. char_notes
    snap.char_notes.clear();
    {
        let mut stmt = conn
            .prepare("SELECT character_id, item_id, note FROM char_notes")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (cid, iid, note) = row.map_err(|e| e.to_string())?;
            snap.char_notes.insert((cid, iid), note);
        }
    }

    Ok(())
}

pub(super) fn sync_d6_social(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let next_fp = compute_d6_social_fp(snap);
    if fp.d6_social == next_fp {
        return Ok(false);
    }

    tx.execute_batch(
        r#"
        DELETE FROM read_mails;
        DELETE FROM deleted_mails;
        DELETE FROM trashed_mails;
        DELETE FROM mail_labels;
        DELETE FROM mail_assigned_labels;
        DELETE FROM mailing_lists;
        DELETE FROM calendar_events;
        DELETE FROM bookmark_meta;
        DELETE FROM bookmark_folders;
        DELETE FROM chat_channels;
        DELETE FROM char_contacts;
        DELETE FROM char_contact_labels;
        DELETE FROM corp_contacts;
        DELETE FROM corp_contact_labels;
        DELETE FROM alliance_contacts;
        DELETE FROM alliance_contact_labels;
        DELETE FROM char_notes;
        DELETE FROM access_groups;
        "#,
    )
    .map_err(|e| e.to_string())?;

    for &(cid, mid) in &snap.read_mails {
        tx.execute(
            "INSERT INTO read_mails (character_id, mail_id) VALUES (?1, ?2)",
            params![cid, mid],
        )
        .map_err(|e| e.to_string())?;
    }
    for &(cid, mid) in &snap.deleted_mails {
        tx.execute(
            "INSERT INTO deleted_mails (character_id, mail_id) VALUES (?1, ?2)",
            params![cid, mid],
        )
        .map_err(|e| e.to_string())?;
    }
    for &(cid, mid) in &snap.trashed_mails {
        tx.execute(
            "INSERT INTO trashed_mails (character_id, mail_id) VALUES (?1, ?2)",
            params![cid, mid],
        )
        .map_err(|e| e.to_string())?;
    }

    for (cid, labels) in &snap.mail_labels {
        let js = serde_json::to_string(labels).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO mail_labels (character_id, labels_json) VALUES (?1, ?2)",
            params![cid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (&(cid, mid), labels) in &snap.mail_assigned_labels {
        let js = serde_json::to_string(labels).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES (?1, ?2, ?3)",
            params![cid, mid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    for (lid, rec) in &snap.mailing_lists {
        let js = serde_json::to_string(rec).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO mailing_lists (list_id, record_json) VALUES (?1, ?2)",
            params![lid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (eid, rec) in &snap.calendar_events {
        let js = serde_json::to_string(rec).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO calendar_events (event_id, record_json) VALUES (?1, ?2)",
            params![eid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    let mut meta_ids: std::collections::BTreeSet<i64> =
        snap.bookmark_subfolders.keys().copied().collect();
    meta_ids.extend(snap.bookmark_expiries.keys().copied());
    for bid in meta_ids {
        let meta = PersistedBookmarkMeta {
            subfolder_id: snap.bookmark_subfolders.get(&bid).copied(),
            expiry_unix: snap.bookmark_expiries.get(&bid).copied(),
        };
        let js = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (?1, ?2)",
            params![bid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    for (&fid, folder) in &snap.bookmark_folders {
        let known_by: Vec<(i64, bool)> = snap
            .bookmark_folder_known_by
            .iter()
            .filter_map(|(&(f, c), &active)| (f == fid).then_some((c, active)))
            .collect();
        let persisted = PersistedBookmarkFolder {
            record: folder.clone(),
            access_group_ids: snap
                .bookmark_folder_acls
                .get(&fid)
                .cloned()
                .unwrap_or_default(),
            known_by,
            parent_folder_id: snap.bookmark_subfolder_parents.get(&fid).copied(),
        };
        let js = serde_json::to_string(&persisted).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (?1, ?2)",
            params![fid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    for (&gid, members) in &snap.access_groups {
        let js = serde_json::to_string(members).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO access_groups (group_id, members_json) VALUES (?1, ?2)",
            params![gid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    for (cid, chan) in &snap.chat_channels {
        let persisted = PersistedChatChannel {
            record: chan.clone(),
            acl: snap
                .chat_channel_acl
                .get(cid)
                .cloned()
                .unwrap_or_default(),
        };
        let js = serde_json::to_string(&persisted).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO chat_channels (channel_id, record_json) VALUES (?1, ?2)",
            params![cid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    for (cid, contacts) in &snap.char_contacts {
        let js = serde_json::to_string(contacts).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO char_contacts (character_id, contacts_json) VALUES (?1, ?2)",
            params![cid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (cid, labels) in &snap.char_contact_labels {
        let js = serde_json::to_string(labels).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (?1, ?2)",
            params![cid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (cid, contacts) in &snap.corp_contacts {
        let js = serde_json::to_string(contacts).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (?1, ?2)",
            params![cid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (cid, labels) in &snap.corp_contact_labels {
        let js = serde_json::to_string(labels).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (?1, ?2)",
            params![cid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (aid, contacts) in &snap.alliance_contacts {
        let js = serde_json::to_string(contacts).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (?1, ?2)",
            params![aid, js],
        )
        .map_err(|e| e.to_string())?;
    }
    for (aid, labels) in &snap.alliance_contact_labels {
        let js = serde_json::to_string(labels).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (?1, ?2)",
            params![aid, js],
        )
        .map_err(|e| e.to_string())?;
    }

    for (&(cid, iid), note) in &snap.char_notes {
        tx.execute(
            "INSERT INTO char_notes (character_id, item_id, note) VALUES (?1, ?2, ?3)",
            params![cid, iid, note],
        )
        .map_err(|e| e.to_string())?;
    }

    fp.d6_social = next_fp;
    Ok(true)
}

pub(super) fn rollback_d6_social(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) {
    if compute_d6_social_fp(snap) != fp.d6_social {
        let _ = load_d6_social_into_snapshot(conn, snap);
    }
}
