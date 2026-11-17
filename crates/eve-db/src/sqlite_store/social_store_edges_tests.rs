#![forbid(unsafe_code)]

use super::{
    drop_legacy_social_tables, load_social_domain_rows, sync_social_domain_rows,
    PersistedBookmarkFolder, PersistedBookmarkMeta, PersistedChatChannel, LEGACY_D6_SOCIAL_SQL,
    LEGACY_SOCIAL_TABLES,
};
use crate::durable_domain_types::SocialContactNotificationRecord;
use crate::persistence::WorldRepository;
use crate::sqlite_store::domain_rows::{
    DOMAIN_SOCIAL_BOOKMARKS, DOMAIN_SOCIAL_CHAT_CALENDAR, DOMAIN_SOCIAL_CONTACTS_NOTES,
    DOMAIN_SOCIAL_MAIL,
};
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::quarantine::LoadContext;
use crate::sqlite_store::schema::apply_migrations;
use crate::types::{
    BookmarkFolderRecord, CalendarEventRecord, ChatChannelAclRecord, ChatChannelRecord,
    MailLabelRecord, MailingListRecord, SocialContactRecord, WorldStateSnapshot,
};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

fn temp_social_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "evomu_social_edges_{tag}_{}.sqlite",
        std::process::id()
    ))
}

fn existing_legacy_social_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .filter(|name| LEGACY_SOCIAL_TABLES.contains(&name.as_str()))
        .collect()
}

fn populate_all_24_social_fields(snap: &mut WorldStateSnapshot) {
    let cid = 90_000_001_i64;
    let peer = 90_000_002_i64;
    snap.read_mails.insert((cid, 1001));
    snap.deleted_mails.insert((cid, 1002));
    snap.trashed_mails.insert((cid, 1003));
    snap.mail_labels.insert(
        cid,
        vec![MailLabelRecord {
            label_id: 1,
            name: "Important".into(),
            color: "#ff0000".into(),
        }],
    );
    snap.mail_assigned_labels.insert((cid, 1001), vec![1]);
    snap.mailing_lists.insert(
        31_000_001,
        MailingListRecord {
            list_id: 31_000_001,
            name: "FleetAlerts".into(),
            owner_id: cid,
            default_access: 1,
            default_role: 2,
            cost: 100,
            members: HashMap::from([(cid, 3)]),
            entity_access: HashMap::from([(98_000_001, 1)]),
            welcome_mail: Some(("Welcome".into(), "Fly safe".into())),
            mail_ids: HashSet::from([1001]),
        },
    );

    snap.bookmark_folders.insert(
        32_000_001,
        BookmarkFolderRecord {
            folder_id: 32_000_001,
            owner_id: cid,
            folder_name: "CorpSafe".into(),
            is_shared: true,
            corporation_id: Some(98_000_001),
            description: "Shared corp bookmarks".into(),
            admin_group_id: Some(33_000_001),
            manage_group_id: Some(33_000_002),
            use_group_id: None,
            view_group_id: None,
        },
    );
    snap.bookmark_folders.insert(
        32_000_002,
        BookmarkFolderRecord {
            folder_id: 32_000_002,
            owner_id: cid,
            folder_name: "Tacticals".into(),
            ..Default::default()
        },
    );
    snap.bookmark_subfolders.insert(34_000_001, 32_000_002);
    snap.bookmark_subfolder_parents
        .insert(32_000_002, 32_000_001);
    snap.bookmark_expiries.insert(34_000_001, 1_950_000_000);
    snap.bookmark_folder_acls
        .insert(32_000_001, vec![33_000_001, 33_000_002]);
    snap.access_groups.insert(33_000_001, vec![cid, peer]);
    snap.bookmark_folder_known_by.insert((32_000_001, peer), true);

    snap.calendar_events.insert(
        35_000_001,
        CalendarEventRecord {
            event_id: 35_000_001,
            owner_id: cid,
            creator_id: cid,
            event_date_time: 1_700_001_000,
            duration: 60,
            importance: 1,
            title: "Stratop".into(),
            description: "Undock in Jita".into(),
            scope: "Corporation".into(),
            responses: HashMap::from([(cid, 1), (peer, 2)]),
            is_deleted: false,
        },
    );
    snap.chat_channels.insert(
        "corp_98000001".into(),
        ChatChannelRecord {
            channel_id: "corp_98000001".into(),
            name: "Corp".into(),
            motd: "Welcome".into(),
            owner_id: Some(cid),
            members: HashSet::from([cid, peer]),
            is_system_local: false,
            solar_system_id: Some(30_000_142),
            is_private: true,
            operators: HashSet::from([cid]),
            password: Some("secret".into()),
            is_delayed_local: false,
        },
    );
    snap.chat_channel_acl.insert(
        "corp_98000001".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::from([cid]),
            deny_list: HashSet::new(),
            muted_until: HashMap::new(),
        },
    );
    snap.chat_channel_acl.insert(
        "global".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::new(),
            deny_list: HashSet::from([peer]),
            muted_until: HashMap::from([(peer, (1_900_000_000, "spam".into()))]),
        },
    );

    let contact = SocialContactRecord {
        contact_id: peer,
        contact_type: "character".into(),
        standing: 5.0,
        is_watched: true,
        is_blocked: false,
        label_ids: vec![1],
    };
    snap.char_contacts.insert(cid, vec![contact.clone()]);
    snap.char_contact_notifications.insert(
        cid,
        vec![SocialContactNotificationRecord {
            notification_id: 77,
            sender_character_id: peer,
            send_date_unix: 1_700_000_200,
            standing_level: 5.0,
            message: "Added".into(),
        }],
    );
    snap.char_contact_labels
        .insert(cid, vec![(1, "Friends".into())]);
    snap.corp_contacts
        .insert(98_000_001, vec![contact.clone()]);
    snap.corp_contact_labels
        .insert(98_000_001, vec![(1, "Blues".into(), 2)]);
    snap.alliance_contacts.insert(99_000_001, vec![contact]);
    snap.alliance_contact_labels
        .insert(99_000_001, vec![(1, "Allies".into())]);
    snap.char_notes.insert((cid, peer), "Trusted scout".into());
}

fn assert_all_24_social_fields_equal(actual: &WorldStateSnapshot, expected: &WorldStateSnapshot) {
    assert_eq!(actual.read_mails, expected.read_mails);
    assert_eq!(actual.deleted_mails, expected.deleted_mails);
    assert_eq!(actual.trashed_mails, expected.trashed_mails);
    assert_eq!(actual.mail_labels, expected.mail_labels);
    assert_eq!(actual.mail_assigned_labels, expected.mail_assigned_labels);
    assert_eq!(actual.mailing_lists, expected.mailing_lists);
    assert_eq!(actual.bookmark_folders, expected.bookmark_folders);
    assert_eq!(actual.bookmark_subfolders, expected.bookmark_subfolders);
    assert_eq!(
        actual.bookmark_subfolder_parents,
        expected.bookmark_subfolder_parents
    );
    assert_eq!(actual.bookmark_expiries, expected.bookmark_expiries);
    assert_eq!(actual.bookmark_folder_acls, expected.bookmark_folder_acls);
    assert_eq!(actual.access_groups, expected.access_groups);
    assert_eq!(
        actual.bookmark_folder_known_by,
        expected.bookmark_folder_known_by
    );
    assert_eq!(actual.calendar_events, expected.calendar_events);
    assert_eq!(actual.chat_channels, expected.chat_channels);
    assert_eq!(actual.chat_channel_acl, expected.chat_channel_acl);
    assert_eq!(actual.char_contacts, expected.char_contacts);
    assert_eq!(
        actual.char_contact_notifications,
        expected.char_contact_notifications
    );
    assert_eq!(actual.char_contact_labels, expected.char_contact_labels);
    assert_eq!(actual.corp_contacts, expected.corp_contacts);
    assert_eq!(actual.corp_contact_labels, expected.corp_contact_labels);
    assert_eq!(actual.alliance_contacts, expected.alliance_contacts);
    assert_eq!(
        actual.alliance_contact_labels,
        expected.alliance_contact_labels
    );
    assert_eq!(actual.char_notes, expected.char_notes);
}

#[test]
fn social_legacy_tables_migration_preserves_newer_domain_rows_and_drops_tables() {
    let path = temp_social_db_path("legacy_migration");
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("apply_migrations");
    assert!(
        existing_legacy_social_tables(&conn).is_empty(),
        "fresh schema must not create any of the 19 legacy social tables"
    );

    // Simulate a pre-#916 database containing all 19 legacy social tables.
    conn.execute_batch(LEGACY_D6_SOCIAL_SQL)
        .expect("create legacy d6_social tables");
    assert_eq!(existing_legacy_social_tables(&conn).len(), 19);

    let mut legacy = WorldStateSnapshot::default();
    populate_all_24_social_fields(&mut legacy);
    let cid = 90_000_001_i64;
    let peer = 90_000_002_i64;

    // Seed parent bookmark so live `bookmark_meta.subfolder_id` survives orphan filtering, plus
    // an orphaned `bookmark_meta` row (id 34_000_099) that must NOT revive `bookmark_subfolders`.
    conn.execute(
        "INSERT INTO bookmarks (bookmark_id, owner_id, label, solar_system_id, x, y, z, item_id, memo, folder_id, is_shared) VALUES (34000001, ?1, 'Safe', 30000142, 1.0, 2.0, 3.0, NULL, '', 32000001, 1)",
        [cid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO read_mails (character_id, mail_id) VALUES (?1, 1001)",
        [cid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO deleted_mails (character_id, mail_id) VALUES (?1, 1002)",
        [cid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO trashed_mails (character_id, mail_id) VALUES (?1, 1003)",
        [cid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mail_labels (character_id, labels_json) VALUES (?1, ?2)",
        params![cid, serde_json::to_string(&legacy.mail_labels[&cid]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES (?1, 1001, ?2)",
        params![
            cid,
            serde_json::to_string(&legacy.mail_assigned_labels[&(cid, 1001)]).unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mailing_lists (list_id, record_json) VALUES (31000001, ?1)",
        [serde_json::to_string(&legacy.mailing_lists[&31_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO calendar_events (event_id, record_json) VALUES (35000001, ?1)",
        [serde_json::to_string(&legacy.calendar_events[&35_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (34000001, ?1), (34000099, ?2)",
        params![
            serde_json::to_string(&PersistedBookmarkMeta {
                subfolder_id: Some(32_000_002),
                expiry_unix: Some(1_950_000_000),
            })
            .unwrap(),
            serde_json::to_string(&PersistedBookmarkMeta {
                subfolder_id: Some(32_000_002),
                expiry_unix: Some(0),
            })
            .unwrap(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (32000001, ?1), (32000002, ?2)",
        params![
            serde_json::to_string(&PersistedBookmarkFolder {
                record: legacy.bookmark_folders[&32_000_001].clone(),
                access_group_ids: vec![33_000_001, 33_000_002],
                known_by: vec![(peer, true)],
                parent_folder_id: None,
            })
            .unwrap(),
            serde_json::to_string(&PersistedBookmarkFolder {
                record: legacy.bookmark_folders[&32_000_002].clone(),
                access_group_ids: vec![],
                known_by: vec![],
                parent_folder_id: Some(32_000_001),
            })
            .unwrap(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO access_groups (group_id, members_json) VALUES (33000001, ?1)",
        [serde_json::to_string(&legacy.access_groups[&33_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chat_channels (channel_id, record_json) VALUES ('corp_98000001', ?1), ('global', ?2)",
        params![
            serde_json::to_string(&PersistedChatChannel {
                record: legacy.chat_channels["corp_98000001"].clone(),
                acl: legacy.chat_channel_acl["corp_98000001"].clone(),
                standalone_acl_only: false,
            })
            .unwrap(),
            serde_json::to_string(&PersistedChatChannel {
                record: ChatChannelRecord {
                    channel_id: "global".into(),
                    ..Default::default()
                },
                acl: legacy.chat_channel_acl["global"].clone(),
                standalone_acl_only: true,
            })
            .unwrap(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO char_contacts (character_id, contacts_json) VALUES (?1, ?2)",
        params![cid, serde_json::to_string(&legacy.char_contacts[&cid]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO char_contact_notifications (character_id, notifications_json) VALUES (?1, ?2)",
        params![
            cid,
            serde_json::to_string(&legacy.char_contact_notifications[&cid]).unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (?1, ?2)",
        params![
            cid,
            serde_json::to_string(&legacy.char_contact_labels[&cid]).unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (98000001, ?1)",
        [serde_json::to_string(&legacy.corp_contacts[&98_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (98000001, ?1)",
        [serde_json::to_string(&legacy.corp_contact_labels[&98_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (99000001, ?1)",
        [serde_json::to_string(&legacy.alliance_contacts[&99_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (99000001, ?1)",
        [serde_json::to_string(&legacy.alliance_contact_labels[&99_000_001]).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO char_notes (character_id, item_id, note) VALUES (?1, ?2, 'LegacyNote')",
        params![cid, peer],
    )
    .unwrap();

    // Newer `domain_rows` overrides must take precedence over stale legacy rows across domains.
    let mut newer_folder = legacy.bookmark_folders[&32_000_001].clone();
    newer_folder.folder_name = "NewerCorpSafe".into();
    let mut newer_list = legacy.mailing_lists[&31_000_001].clone();
    newer_list.name = "NewerFleetAlerts".into();
    let mut newer_event = legacy.calendar_events[&35_000_001].clone();
    newer_event.title = "NewerStratop".into();
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES
            (?1, 'mailing_lists/31000001', ?2),
            (?3, 'bookmark_folders/32000001', ?4),
            (?5, 'calendar_events/35000001', ?6),
            (?7, ?8, '\"NewerNote\"')",
        params![
            DOMAIN_SOCIAL_MAIL,
            serde_json::to_string(&newer_list).unwrap(),
            DOMAIN_SOCIAL_BOOKMARKS,
            serde_json::to_string(&newer_folder).unwrap(),
            DOMAIN_SOCIAL_CHAT_CALENDAR,
            serde_json::to_string(&newer_event).unwrap(),
            DOMAIN_SOCIAL_CONTACTS_NOTES,
            format!("char_notes/{cid}|{peer}"),
        ],
    )
    .unwrap();

    let migrated = load_snapshot_from_sqlite(&conn).expect("load + migrate legacy social tables");
    assert!(
        existing_legacy_social_tables(&conn).is_empty(),
        "startup migration must drop all 19 legacy social tables"
    );
    assert_eq!(migrated.read_mails, legacy.read_mails);
    assert_eq!(migrated.deleted_mails, legacy.deleted_mails);
    assert_eq!(migrated.trashed_mails, legacy.trashed_mails);
    assert_eq!(migrated.mail_labels, legacy.mail_labels);
    assert_eq!(migrated.mail_assigned_labels, legacy.mail_assigned_labels);
    assert_eq!(migrated.mailing_lists[&31_000_001].name, "NewerFleetAlerts");
    assert_eq!(
        migrated.bookmark_folders[&32_000_001].folder_name,
        "NewerCorpSafe"
    );
    assert_eq!(migrated.bookmark_subfolders, legacy.bookmark_subfolders);
    assert!(
        !migrated.bookmark_subfolders.contains_key(&34_000_099),
        "orphaned bookmark subfolder must not be revived"
    );
    assert!(
        !migrated.bookmark_expiries.contains_key(&34_000_099),
        "zero bookmark expiry must not be persisted"
    );
    assert_eq!(
        migrated.bookmark_subfolder_parents,
        legacy.bookmark_subfolder_parents
    );
    assert_eq!(migrated.bookmark_expiries, legacy.bookmark_expiries);
    assert_eq!(migrated.bookmark_folder_acls, legacy.bookmark_folder_acls);
    assert_eq!(migrated.access_groups, legacy.access_groups);
    assert_eq!(
        migrated.bookmark_folder_known_by,
        legacy.bookmark_folder_known_by
    );
    assert_eq!(migrated.calendar_events[&35_000_001].title, "NewerStratop");
    assert_eq!(migrated.chat_channels, legacy.chat_channels);
    assert!(
        !migrated.chat_channels.contains_key("global"),
        "standalone_acl_only must not create phantom ChatChannelRecord"
    );
    assert_eq!(migrated.chat_channel_acl, legacy.chat_channel_acl);
    assert_eq!(migrated.char_contacts, legacy.char_contacts);
    assert_eq!(
        migrated.char_contact_notifications,
        legacy.char_contact_notifications
    );
    assert_eq!(migrated.char_contact_labels, legacy.char_contact_labels);
    assert_eq!(migrated.corp_contacts, legacy.corp_contacts);
    assert_eq!(migrated.corp_contact_labels, legacy.corp_contact_labels);
    assert_eq!(migrated.alliance_contacts, legacy.alliance_contacts);
    assert_eq!(
        migrated.alliance_contact_labels,
        legacy.alliance_contact_labels
    );
    assert_eq!(migrated.char_notes.get(&(cid, peer)), Some(&"NewerNote".into()));
    drop(conn);

    // Reopen with WorldRepository, delete a migrated row, and confirm it never resurrects.
    let repo = WorldRepository::open_sqlite(&path).expect("reopen migrated db");
    repo.with_mut_snapshot(|snap| {
        snap.char_notes.remove(&(cid, peer));
        snap.chat_channel_acl.remove("global");
    });
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&path).expect("second reopen");
    reopened.with_snapshot(|snap| {
        assert!(!snap.char_notes.contains_key(&(cid, peer)));
        assert!(!snap.chat_channel_acl.contains_key("global"));
        assert_eq!(snap.mailing_lists[&31_000_001].name, "NewerFleetAlerts");
    });
    drop(reopened);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn social_load_missing_tables_unknown_keys_and_corrupt_rows_fail_closed_or_quarantine() {
    // 1. Dropped `domain_rows` without legacy tables loads cleanly as `Ok(false)`.
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    conn.execute("DROP TABLE domain_rows", []).unwrap();
    let mut empty = WorldStateSnapshot::default();
    assert!(!load_social_domain_rows(&conn, &mut empty, &mut LoadContext::new(false)).unwrap());

    // `sync_social_domain_rows` rejects missing `domain_rows`.
    let mut populated = WorldStateSnapshot::default();
    populate_all_24_social_fields(&mut populated);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_social_domain_rows(&tx, &populated).is_err());
    }

    // 2. Corrupt JSON, unknown fields, and keys without `/` across all 4 social domains fail
    //    closed under strict load and quarantine under quarantine mode.
    for domain in [
        DOMAIN_SOCIAL_MAIL,
        DOMAIN_SOCIAL_BOOKMARKS,
        DOMAIN_SOCIAL_CHAT_CALENDAR,
        DOMAIN_SOCIAL_CONTACTS_NOTES,
    ] {
        for (bad_key, bad_val) in [
            ("unknown_field/1", "1"),
            ("malformed_no_slash", "1"),
            ("mailing_lists/bad_int", "{}"),
        ] {
            if bad_key == "mailing_lists/bad_int" && domain != DOMAIN_SOCIAL_MAIL {
                continue;
            }
            let c = Connection::open_in_memory().unwrap();
            apply_migrations(&c).unwrap();
            c.execute(
                "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                params![domain, bad_key, bad_val],
            )
            .unwrap();
            let mut snap = WorldStateSnapshot::default();
            let err = load_social_domain_rows(&c, &mut snap, &mut LoadContext::new(false))
                .expect_err("strict load must fail closed");
            assert!(err.contains(domain), "expected domain in `{err}`");

            let mut qctx = LoadContext::quarantine_in_memory();
            assert!(!load_social_domain_rows(&c, &mut snap, &mut qctx).unwrap());
            assert_eq!(qctx.quarantined.len(), 1);
            assert_eq!(qctx.quarantined[0].source_table, "domain_rows");
            assert_eq!(qctx.quarantined[0].key, bad_key);
        }
    }

    // 3. Corrupt JSON / wrong SQLite types in legacy 19 tables fail closed strictly and quarantine.
    let legacy_corrupt_sql = [
        "INSERT INTO read_mails (character_id, mail_id) VALUES ('bad_int', 1)",
        "INSERT INTO deleted_mails (character_id, mail_id) VALUES (1, 'bad_int')",
        "INSERT INTO trashed_mails (character_id, mail_id) VALUES ('bad_int', 1)",
        "INSERT INTO mail_labels (character_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES (1, 1, '{bad')",
        "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES ('bad_int', 1, '[1]')",
        "INSERT INTO mailing_lists (list_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO calendar_events (event_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO access_groups (group_id, members_json) VALUES (1, '{bad')",
        "INSERT INTO chat_channels (channel_id, record_json) VALUES ('ch1', '{bad')",
        "INSERT INTO char_contacts (character_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO char_contact_notifications (character_id, notifications_json) VALUES (1, '{bad')",
        "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO char_notes (character_id, item_id, note) VALUES ('bad_int', 1, 'note')",
        "INSERT INTO char_notes (character_id, item_id, note) VALUES (1, 1, X'DEAD')",
    ];
    for sql in legacy_corrupt_sql {
        let mut c = Connection::open_in_memory().unwrap();
        apply_migrations(&c).unwrap();
        c.execute_batch(LEGACY_D6_SOCIAL_SQL).unwrap();
        c.execute(sql, []).unwrap();
        let mut s = WorldStateSnapshot::default();
        assert!(
            load_social_domain_rows(&c, &mut s, &mut LoadContext::new(false)).is_err(),
            "expected strict legacy error for `{sql}`"
        );
        let mut qctx = LoadContext::quarantine_in_memory();
        assert!(
            load_social_domain_rows(&c, &mut s, &mut qctx).unwrap(),
            "expected had_legacy=true for `{sql}`"
        );
        assert_eq!(qctx.quarantined.len(), 1);
        let tx = c.transaction().unwrap();
        drop_legacy_social_tables(&tx).unwrap();
        tx.commit().unwrap();
        assert!(existing_legacy_social_tables(&c).is_empty());
    }
}

#[test]
fn social_load_domain_rows_fails_closed_on_corrupt_row() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut committed = WorldStateSnapshot::default();
    populate_all_24_social_fields(&mut committed);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_social_domain_rows(&tx, &committed).unwrap());
        tx.commit().unwrap();
    }

    // Mutate every domain in memory.
    let mut dirty = committed.clone();
    dirty.read_mails.clear();
    dirty.deleted_mails.insert((90_000_001, 9999));
    dirty.trashed_mails.clear();
    dirty.mail_labels.clear();
    dirty.mail_assigned_labels.clear();
    dirty.mailing_lists.clear();
    dirty.bookmark_folders.clear();
    dirty.bookmark_subfolders.clear();
    dirty.bookmark_subfolder_parents.clear();
    dirty.bookmark_expiries.clear();
    dirty.bookmark_folder_acls.clear();
    dirty.access_groups.clear();
    dirty.bookmark_folder_known_by.clear();
    dirty.calendar_events.clear();
    dirty.chat_channels.clear();
    dirty.chat_channel_acl.clear();
    dirty.char_contacts.clear();
    dirty.char_contact_notifications.clear();
    dirty.char_contact_labels.clear();
    dirty.corp_contacts.clear();
    dirty.corp_contact_labels.clear();
    dirty.alliance_contacts.clear();
    dirty.alliance_contact_labels.clear();
    dirty.char_notes.insert((90_000_001, 90_000_002), "Dirty".into());
    let expected_dirty = dirty.clone();

    // Corrupt 1 row in `social.contacts_notes` -> `load_social_domain_rows`
    // must fail closed without mutating any field of `dirty`.
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'char_notes/90000001|999', '{bad')",
        [DOMAIN_SOCIAL_CONTACTS_NOTES],
    )
    .unwrap();
    let load_err = load_social_domain_rows(&conn, &mut dirty, &mut LoadContext::new(false))
        .expect_err("corrupt row must fail load_social_domain_rows");
    assert!(load_err.contains(DOMAIN_SOCIAL_CONTACTS_NOTES), "{load_err}");
    assert_all_24_social_fields_equal(&dirty, &expected_dirty);
}
