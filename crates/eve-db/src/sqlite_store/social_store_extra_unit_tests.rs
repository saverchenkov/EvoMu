//! Deep branch-coverage and mutation-killing unit tests for `sqlite_store::social_store`.

use super::fingerprints::TableFingerprints;
use super::schema::apply_migrations;
use super::social_store::{
    apply_d6_social_migration, compute_d6_social_fp, load_d6_social_into_snapshot, sync_d6_social,
};
use crate::durable_domain_types::SocialContactNotificationRecord;
use crate::snapshot_ext::{
    CalendarEventRecord, ChatChannelAclRecord, MailingListRecord, SocialContactRecord,
    WorldStateSnapshot,
};
use crate::types::{BookmarkFolderRecord, BookmarkRecord, ChatChannelRecord, MailLabelRecord};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

#[test]
fn compute_d6_social_fp_detects_every_single_social_collection_mutation() {
    let base = WorldStateSnapshot::default();
    let base_fp = compute_d6_social_fp(&base);

    let assert_changes = |mutator: &dyn Fn(&mut WorldStateSnapshot), label: &str| {
        let mut s = base.clone();
        mutator(&mut s);
        assert_ne!(
            compute_d6_social_fp(&s),
            base_fp,
            "compute_d6_social_fp missed mutation in {label}"
        );
    };

    assert_changes(&|s| { s.read_mails.insert((1, 2)); }, "read_mails");
    assert_changes(&|s| { s.deleted_mails.insert((1, 2)); }, "deleted_mails");
    assert_changes(&|s| { s.trashed_mails.insert((1, 2)); }, "trashed_mails");
    assert_changes(
        &|s| {
            s.mail_labels.insert(
                1,
                vec![MailLabelRecord {
                    label_id: 1,
                    name: "L".into(),
                    color: "#000".into(),
                }],
            );
        },
        "mail_labels",
    );
    assert_changes(&|s| { s.mail_assigned_labels.insert((1, 2), vec![3]); }, "mail_assigned_labels");
    assert_changes(
        &|s| {
            s.mailing_lists.insert(
                1,
                MailingListRecord {
                    list_id: 1,
                    name: "ML".into(),
                    owner_id: 1,
                    default_access: 0,
                    default_role: 0,
                    cost: 0,
                    members: HashMap::new(),
                    entity_access: HashMap::new(),
                    welcome_mail: None,
                    mail_ids: HashSet::new(),
                },
            );
        },
        "mailing_lists",
    );
    assert_changes(
        &|s| {
            s.calendar_events.insert(
                1,
                CalendarEventRecord {
                    event_id: 1,
                    owner_id: 1,
                    creator_id: 1,
                    event_date_time: 100,
                    duration: 30,
                    importance: 0,
                    title: "T".into(),
                    description: "D".into(),
                    scope: "Personal".into(),
                    responses: HashMap::new(),
                    is_deleted: false,
                },
            );
        },
        "calendar_events",
    );
    // `snap.bookmarks` is persisted via `bookmarks` in `sync.rs`, so mutating `bookmarks` alone
    // must NOT dirty `social_store` fingerprints.
    {
        let mut s = base.clone();
        s.bookmarks.insert(
            1,
            BookmarkRecord {
                bookmark_id: 1,
                owner_id: 1,
                label: "BM".into(),
                solar_system_id: 30_000_142,
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        assert_eq!(
            compute_d6_social_fp(&s),
            base_fp,
            "mutating bookmarks table must not dirty social_store fingerprints"
        );
    }
    assert_changes(&|s| { s.bookmark_subfolders.insert(1, 2); }, "bookmark_subfolders");
    assert_changes(&|s| { s.bookmark_expiries.insert(1, 500); }, "bookmark_expiries");
    assert_changes(
        &|s| {
            s.bookmark_folders.insert(
                10,
                BookmarkFolderRecord {
                    folder_id: 10,
                    owner_id: 1,
                    folder_name: "F".into(),
                    ..Default::default()
                },
            );
        },
        "bookmark_folders",
    );
    assert_changes(&|s| { s.access_groups.insert(5, vec![1, 2]); }, "access_groups");
    {
        let mut folder_base = base.clone();
        folder_base.bookmark_folders.insert(
            10,
            BookmarkFolderRecord {
                folder_id: 10,
                owner_id: 1,
                folder_name: "F".into(),
                ..Default::default()
            },
        );
        let folder_fp = compute_d6_social_fp(&folder_base);

        let mut with_known = folder_base.clone();
        with_known.bookmark_folder_known_by.insert((10, 1), true);
        assert_ne!(compute_d6_social_fp(&with_known), folder_fp);

        let mut with_acl = folder_base.clone();
        with_acl.bookmark_folder_acls.insert(10, vec![5]);
        assert_ne!(compute_d6_social_fp(&with_acl), folder_fp);

        let mut with_parent = folder_base;
        with_parent.bookmark_subfolder_parents.insert(10, 1);
        assert_ne!(compute_d6_social_fp(&with_parent), folder_fp);
    }
    assert_changes(
        &|s| {
            s.chat_channels.insert(
                "ch1".into(),
                ChatChannelRecord {
                    channel_id: "ch1".into(),
                    name: "C".into(),
                    motd: String::new(),
                    owner_id: Some(1),
                    members: HashSet::new(),
                    is_system_local: false,
                    solar_system_id: None,
                    is_private: false,
                    operators: HashSet::new(),
                    password: None,
                    is_delayed_local: false,
                },
            );
        },
        "chat_channels",
    );

    let contact = SocialContactRecord {
        contact_id: 2,
        contact_type: "character".into(),
        standing: 10.0,
        is_watched: true,
        is_blocked: false,
        label_ids: vec![],
    };
    assert_changes(&|s| { s.char_contacts.insert(1, vec![contact.clone()]); }, "char_contacts");
    assert_changes(
        &|s| {
            s.char_contact_notifications.insert(
                1,
                vec![SocialContactNotificationRecord {
                    notification_id: 1,
                    sender_character_id: 2,
                    send_date_unix: 100,
                    standing_level: 5.0,
                    message: "hi".into(),
                }],
            );
        },
        "char_contact_notifications",
    );
    assert_changes(
        &|s| { s.char_contact_labels.insert(1, vec![(1, "L".into())]); },
        "char_contact_labels",
    );
    assert_changes(&|s| { s.corp_contacts.insert(1, vec![contact.clone()]); }, "corp_contacts");
    assert_changes(
        &|s| { s.corp_contact_labels.insert(1, vec![(1, "CL".into(), 1)]); },
        "corp_contact_labels",
    );
    assert_changes(
        &|s| { s.alliance_contacts.insert(1, vec![contact.clone()]); },
        "alliance_contacts",
    );
    assert_changes(
        &|s| { s.alliance_contact_labels.insert(1, vec![(1, "AL".into())]); },
        "alliance_contact_labels",
    );
    assert_changes(&|s| { s.char_notes.insert((1, 2), "note".into()); }, "char_notes");
}

#[test]
fn social_store_load_edge_cases_for_bookmark_meta_folders_and_chat_acls() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_social_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    // Bookmark 100 exists in snap.bookmarks; bookmark 200 is an orphan (not in snap.bookmarks).
    snap.bookmarks.insert(
        100,
        BookmarkRecord {
            bookmark_id: 100,
            owner_id: 90_000_001,
            label: "Live".into(),
            solar_system_id: 30_000_142,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            item_id: None,
            memo: String::new(),
            folder_id: Some(10),
            is_shared: false,
        },
    );
    snap.bookmark_subfolders.insert(100, 77);
    snap.bookmark_subfolders.insert(200, 88); // orphan subfolder_id -> ignored on load if bookmark 200 not in snap.bookmarks
    snap.bookmark_expiries.insert(100, 0); // expiry == 0 -> filtered out on load (`exp > 0`)
    snap.bookmark_expiries.insert(200, 1_900_000_000); // expiry > 0 -> loaded even without bookmark entry

    // Folder 10 has empty ACL and no parent; Folder 11 has non-empty ACL, parent, and known_by.
    snap.bookmark_folders.insert(
        10,
        BookmarkFolderRecord {
            folder_id: 10,
            owner_id: 90_000_001,
            folder_name: "EmptyAcl".into(),
            ..Default::default()
        },
    );
    snap.bookmark_folders.insert(
        11,
        BookmarkFolderRecord {
            folder_id: 11,
            owner_id: 90_000_001,
            folder_name: "WithAcl".into(),
            ..Default::default()
        },
    );
    snap.bookmark_folder_acls.insert(11, vec![500, 501]);
    snap.bookmark_subfolder_parents.insert(11, 10);
    snap.bookmark_folder_known_by.insert((11, 90_000_001), true);
    snap.bookmark_folder_known_by.insert((11, 90_000_002), false);

    // Chat channels: ch_empty has default empty ACL; ch_deny has deny_list; ch_mute has muted_until.
    for id in ["ch_empty", "ch_deny", "ch_mute"] {
        snap.chat_channels.insert(
            id.into(),
            ChatChannelRecord {
                channel_id: id.into(),
                name: id.into(),
                motd: String::new(),
                owner_id: Some(90_000_001),
                members: HashSet::new(),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
    }
    snap.chat_channel_acl.insert(
        "ch_deny".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::new(),
            deny_list: HashSet::from([90_000_003]),
            muted_until: HashMap::new(),
        },
    );
    snap.chat_channel_acl.insert(
        "ch_mute".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::new(),
            deny_list: HashSet::new(),
            muted_until: HashMap::from([(90_000_004, (9_999, "spam".into()))]),
        },
    );

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_social(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    loaded.bookmarks = snap.bookmarks.clone();
    load_d6_social_into_snapshot(&conn, &mut loaded).unwrap();

    // Bookmark 100 is in loaded.bookmarks so subfolder 77 is restored; 200 is not so 88 is omitted.
    assert_eq!(loaded.bookmark_subfolders.get(&100), Some(&77));
    assert!(!loaded.bookmark_subfolders.contains_key(&200));

    // Expiry 0 is skipped (`exp > 0`), while expiry 1_900_000_000 is loaded.
    assert!(!loaded.bookmark_expiries.contains_key(&100));
    assert_eq!(loaded.bookmark_expiries.get(&200), Some(&1_900_000_000));

    // Folder 10 has no ACL entry; Folder 11 has ACL, parent, and known_by.
    assert!(!loaded.bookmark_folder_acls.contains_key(&10));
    assert_eq!(loaded.bookmark_folder_acls.get(&11), Some(&vec![500, 501]));
    assert_eq!(loaded.bookmark_subfolder_parents.get(&11), Some(&10));
    assert_eq!(loaded.bookmark_folder_known_by.get(&(11, 90_000_001)), Some(&true));
    assert_eq!(loaded.bookmark_folder_known_by.get(&(11, 90_000_002)), Some(&false));

    // Chat channel ACL: ch_empty omitted, ch_deny and ch_mute present.
    assert!(!loaded.chat_channel_acl.contains_key("ch_empty"));
    assert!(loaded.chat_channel_acl["ch_deny"].deny_list.contains(&90_000_003));
    assert_eq!(
        loaded.chat_channel_acl["ch_mute"].muted_until.get(&90_000_004),
        Some(&(9_999, "spam".into()))
    );

    // Reloading into a cleared snapshot restores persisted state from SQLite.
    let mut reloaded = WorldStateSnapshot::default();
    load_d6_social_into_snapshot(&conn, &mut reloaded).unwrap();
    assert_eq!(reloaded.bookmark_folders.len(), 2);
}

#[test]
fn social_store_single_domain_mutation_never_wipes_untouched_tables() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_social_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.read_mails.insert((1, 100));
    snap.deleted_mails.insert((1, 101));
    snap.trashed_mails.insert((1, 102));
    snap.mail_labels.insert(
        1,
        vec![MailLabelRecord {
            label_id: 1,
            name: "Ops".into(),
            color: "#ffffff".into(),
        }],
    );
    snap.mail_assigned_labels.insert((1, 100), vec![1]);
    snap.mailing_lists.insert(
        50,
        MailingListRecord {
            list_id: 50,
            name: "Ping".into(),
            owner_id: 1,
            default_access: 1,
            default_role: 0,
            cost: 0,
            members: HashMap::from([(1, 3), (2, 0)]),
            entity_access: HashMap::from([(98_000_001, 1)]),
            welcome_mail: None,
            mail_ids: HashSet::from([100, 200]),
        },
    );
    snap.calendar_events.insert(
        60,
        CalendarEventRecord {
            event_id: 60,
            owner_id: 1,
            creator_id: 1,
            event_date_time: 1_700_000_000,
            duration: 60,
            importance: 1,
            title: "CTA".into(),
            description: "Formup".into(),
            scope: "Corporation".into(),
            responses: HashMap::from([(1, 1), (2, 2)]),
            is_deleted: false,
        },
    );
    snap.bookmark_subfolders.insert(100, 10);
    snap.bookmark_expiries.insert(100, 1_900_000_000);
    snap.bookmark_folders.insert(
        10,
        BookmarkFolderRecord {
            folder_id: 10,
            owner_id: 1,
            folder_name: "Tac".into(),
            ..Default::default()
        },
    );
    snap.access_groups.insert(7, vec![1, 2]);
    snap.chat_channels.insert(
        "player_77".into(),
        ChatChannelRecord {
            channel_id: "player_77".into(),
            name: "Intel".into(),
            motd: String::new(),
            owner_id: Some(1),
            members: HashSet::from([1, 2]),
            is_system_local: false,
            solar_system_id: None,
            is_private: true,
            operators: HashSet::from([1]),
            password: None,
            is_delayed_local: false,
        },
    );
    let contact = SocialContactRecord {
        contact_id: 2,
        contact_type: "character".into(),
        standing: 5.0,
        is_watched: true,
        is_blocked: false,
        label_ids: vec![1],
    };
    snap.char_contacts.insert(1, vec![contact.clone()]);
    snap.char_contact_notifications.insert(
        1,
        vec![SocialContactNotificationRecord {
            notification_id: 9,
            sender_character_id: 2,
            send_date_unix: 1_700_000_000,
            standing_level: 5.0,
            message: "o7".into(),
        }],
    );
    snap.char_contact_labels.insert(1, vec![(1, "Blue".into())]);
    snap.corp_contacts.insert(98_000_001, vec![contact.clone()]);
    snap.corp_contact_labels
        .insert(98_000_001, vec![(1, "CorpBlue".into(), 0)]);
    snap.alliance_contacts.insert(99_000_001, vec![contact]);
    snap.alliance_contact_labels
        .insert(99_000_001, vec![(1, "AllyBlue".into())]);
    snap.char_notes.insert((1, 2), "Trusted FC".into());

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_social(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // Install BEFORE DELETE AND BEFORE INSERT triggers on all 18 untouched tables so any
    // monolithic full-table wipe (`DELETE FROM ...` + re-insert) aborts immediately.
    let untouched_tables = [
        "deleted_mails",
        "trashed_mails",
        "mail_labels",
        "mail_assigned_labels",
        "mailing_lists",
        "calendar_events",
        "bookmark_meta",
        "bookmark_folders",
        "access_groups",
        "chat_channels",
        "char_contacts",
        "char_contact_notifications",
        "char_contact_labels",
        "corp_contacts",
        "corp_contact_labels",
        "alliance_contacts",
        "alliance_contact_labels",
        "char_notes",
    ];
    for tbl in untouched_tables {
        conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER no_del_{tbl} BEFORE DELETE ON {tbl} BEGIN SELECT RAISE(ABORT, 'unexpected delete on {tbl}'); END;
             CREATE TEMP TRIGGER no_ins_{tbl} BEFORE INSERT ON {tbl} BEGIN SELECT RAISE(ABORT, 'unexpected insert on {tbl}'); END;"
        ))
        .unwrap();
    }

    // Mark a single new mail read: only `read_mails` may receive 1 row insert.
    snap.read_mails.insert((1, 105));
    let before_changes = conn.total_changes();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_social(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(
        conn.total_changes() - before_changes,
        1,
        "marking 1 mail read must execute exactly 1 SQLite row write"
    );

    // JSON roundtrip scrambles HashMap/HashSet capacities and SipHash seeds; resync must be a 0-write no-op.
    let roundtripped: WorldStateSnapshot =
        serde_json::from_str(&serde_json::to_string(&snap).unwrap()).unwrap();
    let before_resync = conn.total_changes();
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_d6_social(&tx, &roundtripped, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(conn.total_changes(), before_resync);
}
