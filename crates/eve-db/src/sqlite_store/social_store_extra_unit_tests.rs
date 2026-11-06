//! Deep branch-coverage and mutation-killing unit tests for `sqlite_store::social_store`.

use super::fingerprints::TableFingerprints;
use super::schema::apply_migrations;
use super::social_store::{
    apply_d6_social_migration, compute_d6_social_fp, load_d6_social_into_snapshot,
    rollback_d6_social, sync_d6_social,
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
    assert_changes(
        &|s| {
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
        },
        "bookmarks",
    );
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
    assert_changes(
        &|s| { s.bookmark_folder_known_by.insert((10, 1), true); },
        "bookmark_folder_known_by",
    );
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

    // Rollback when clean is a no-op; rollback when dirty restores from SQLite.
    let clean_fp = compute_d6_social_fp(&loaded);
    fp.d6_social = clean_fp;
    rollback_d6_social(&conn, &mut loaded, &fp);
    assert_eq!(loaded.bookmark_folders.len(), 2);

    loaded.bookmark_folders.clear();
    rollback_d6_social(&conn, &mut loaded, &fp);
    assert_eq!(loaded.bookmark_folders.len(), 2);
}
