#![forbid(unsafe_code)]

use super::domain_rows::{
    apply_domain_row_changes, validate_domain_row_changes, DomainRowChange,
    DOMAIN_SOCIAL_BOOKMARKS, DOMAIN_SOCIAL_CHAT_CALENDAR, DOMAIN_SOCIAL_CONTACTS_NOTES,
    DOMAIN_SOCIAL_MAIL,
};
use super::load::load_snapshot_from_sqlite;
use super::quarantine::LoadContext;
use super::schema::apply_migrations;
use super::social_store::{
    collect_social_domain_row_diffs, load_social_domain_rows, sync_social_domain_rows,
};
use super::sync::replace_sqlite_from_snapshot;
use crate::durable_domain_types::SocialContactNotificationRecord;
use crate::persistence::{
    reset_thread_cold_fp_clones_for_test, thread_cold_fp_clones_for_test, WorldRepository,
};
use crate::tx_engine::{ChangeSet, TxError, TxSqlExecutor};
use crate::types::{
    BookmarkFolderRecord, CalendarEventRecord, ChatChannelAclRecord, ChatChannelRecord,
    MailLabelRecord, MailingListRecord, SocialContactRecord, WorldStateSnapshot,
};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

fn temp_extra_social_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "evomu_social_extra_{tag}_{}.sqlite",
        std::process::id()
    ))
}

#[test]
fn social_domain_row_diffs_detect_every_single_social_collection_mutation() {
    let base = WorldStateSnapshot::default();
    let cid = 90_000_001_i64;
    let peer = 90_000_002_i64;

    let mut cases: Vec<(&'static str, &'static str, &str, Box<dyn FnOnce(&mut WorldStateSnapshot)>)> = vec![
        ("read_mails", DOMAIN_SOCIAL_MAIL, "read_mails/90000001|1", Box::new(move |s| { s.read_mails.insert((cid, 1)); })),
        ("deleted_mails", DOMAIN_SOCIAL_MAIL, "deleted_mails/90000001|2", Box::new(move |s| { s.deleted_mails.insert((cid, 2)); })),
        ("trashed_mails", DOMAIN_SOCIAL_MAIL, "trashed_mails/90000001|3", Box::new(move |s| { s.trashed_mails.insert((cid, 3)); })),
        ("mail_labels", DOMAIN_SOCIAL_MAIL, "mail_labels/90000001", Box::new(move |s| {
            s.mail_labels.insert(cid, vec![MailLabelRecord { label_id: 1, name: "L".into(), color: "#fff".into() }]);
        })),
        ("mail_assigned_labels", DOMAIN_SOCIAL_MAIL, "mail_assigned_labels/90000001|1", Box::new(move |s| {
            s.mail_assigned_labels.insert((cid, 1), vec![1]);
        })),
        ("mailing_lists", DOMAIN_SOCIAL_MAIL, "mailing_lists/10", Box::new(move |s| {
            s.mailing_lists.insert(10, MailingListRecord { list_id: 10, name: "ML".into(), owner_id: cid, ..Default::default() });
        })),
        ("bookmark_folders", DOMAIN_SOCIAL_BOOKMARKS, "bookmark_folders/20", Box::new(move |s| {
            s.bookmark_folders.insert(20, BookmarkFolderRecord { folder_id: 20, owner_id: cid, folder_name: "BF".into(), ..Default::default() });
        })),
        ("bookmark_subfolders", DOMAIN_SOCIAL_BOOKMARKS, "bookmark_subfolders/30", Box::new(|s| {
            s.bookmark_subfolders.insert(30, 20);
        })),
        ("bookmark_subfolder_parents", DOMAIN_SOCIAL_BOOKMARKS, "bookmark_subfolder_parents/21", Box::new(|s| {
            s.bookmark_subfolder_parents.insert(21, 20);
        })),
        ("bookmark_expiries", DOMAIN_SOCIAL_BOOKMARKS, "bookmark_expiries/30", Box::new(|s| {
            s.bookmark_expiries.insert(30, 1_900_000_000);
        })),
        ("bookmark_folder_acls", DOMAIN_SOCIAL_BOOKMARKS, "bookmark_folder_acls/20", Box::new(|s| {
            s.bookmark_folder_acls.insert(20, vec![40]);
        })),
        ("access_groups", DOMAIN_SOCIAL_BOOKMARKS, "access_groups/40", Box::new(move |s| {
            s.access_groups.insert(40, vec![cid]);
        })),
        ("bookmark_folder_known_by", DOMAIN_SOCIAL_BOOKMARKS, "bookmark_folder_known_by/20|90000002", Box::new(move |s| {
            s.bookmark_folder_known_by.insert((20, peer), true);
        })),
        ("calendar_events", DOMAIN_SOCIAL_CHAT_CALENDAR, "calendar_events/50", Box::new(move |s| {
            s.calendar_events.insert(50, CalendarEventRecord { event_id: 50, owner_id: cid, creator_id: cid, title: "Op".into(), ..Default::default() });
        })),
        ("chat_channels", DOMAIN_SOCIAL_CHAT_CALENDAR, "chat_channels/ch1", Box::new(|s| {
            s.chat_channels.insert("ch1".into(), ChatChannelRecord { channel_id: "ch1".into(), name: "Ch".into(), ..Default::default() });
        })),
        ("chat_channel_acl", DOMAIN_SOCIAL_CHAT_CALENDAR, "chat_channel_acl/ch1", Box::new(move |s| {
            s.chat_channel_acl.insert("ch1".into(), ChatChannelAclRecord { allow_list: HashSet::from([cid]), ..Default::default() });
        })),
        ("char_contacts", DOMAIN_SOCIAL_CONTACTS_NOTES, "char_contacts/90000001", Box::new(move |s| {
            s.char_contacts.insert(cid, vec![SocialContactRecord { contact_id: peer, contact_type: "character".into(), standing: 1.0, is_watched: false, is_blocked: false, label_ids: vec![] }]);
        })),
        ("char_contact_notifications", DOMAIN_SOCIAL_CONTACTS_NOTES, "char_contact_notifications/90000001", Box::new(move |s| {
            s.char_contact_notifications.insert(cid, vec![SocialContactNotificationRecord { notification_id: 1, sender_character_id: peer, send_date_unix: 100, standing_level: 1.0, message: "Hi".into() }]);
        })),
        ("char_contact_labels", DOMAIN_SOCIAL_CONTACTS_NOTES, "char_contact_labels/90000001", Box::new(move |s| {
            s.char_contact_labels.insert(cid, vec![(1, "F".into())]);
        })),
        ("corp_contacts", DOMAIN_SOCIAL_CONTACTS_NOTES, "corp_contacts/98000001", Box::new(move |s| {
            s.corp_contacts.insert(98_000_001, vec![SocialContactRecord { contact_id: peer, contact_type: "character".into(), standing: 5.0, is_watched: false, is_blocked: false, label_ids: vec![] }]);
        })),
        ("corp_contact_labels", DOMAIN_SOCIAL_CONTACTS_NOTES, "corp_contact_labels/98000001", Box::new(|s| {
            s.corp_contact_labels.insert(98_000_001, vec![(1, "C".into(), 1)]);
        })),
        ("alliance_contacts", DOMAIN_SOCIAL_CONTACTS_NOTES, "alliance_contacts/99000001", Box::new(move |s| {
            s.alliance_contacts.insert(99_000_001, vec![SocialContactRecord { contact_id: peer, contact_type: "character".into(), standing: -5.0, is_watched: false, is_blocked: false, label_ids: vec![] }]);
        })),
        ("alliance_contact_labels", DOMAIN_SOCIAL_CONTACTS_NOTES, "alliance_contact_labels/99000001", Box::new(|s| {
            s.alliance_contact_labels.insert(99_000_001, vec![(1, "A".into())]);
        })),
        ("char_notes", DOMAIN_SOCIAL_CONTACTS_NOTES, "char_notes/90000001|90000002", Box::new(move |s| {
            s.char_notes.insert((cid, peer), "Note".into());
        })),
    ];

    for (label, expected_domain, expected_key, mutate) in cases.drain(..) {
        let mut after = base.clone();
        mutate(&mut after);

        let cs = ChangeSet::diff(1, &base, &after);
        assert!(
            !cs.cold_domains.d6_social,
            "{label} must use domain_rows instead of cold d6_social"
        );
        assert!(
            !cs.cold_domains.any(),
            "{label} must not flag any cold domain"
        );
        assert_eq!(
            cs.domain_rows.len(),
            1,
            "{label} single insert must emit 1 DomainRowChange"
        );
        assert!(
            matches!(
                &cs.domain_rows[0],
                DomainRowChange::Upsert { domain, key, .. }
                    if *domain == expected_domain && key == expected_key
            ),
            "{label} unexpected upsert diff: {:?}",
            cs.domain_rows[0]
        );

        let mut del_diffs = Vec::new();
        collect_social_domain_row_diffs(&after, &base, &mut del_diffs);
        assert_eq!(
            del_diffs.len(),
            1,
            "{label} delete must emit 1 DomainRowChange"
        );
        assert!(
            matches!(
                &del_diffs[0],
                DomainRowChange::Delete { domain, key }
                    if *domain == expected_domain && key == expected_key
            ),
            "{label} unexpected delete diff: {:?}",
            del_diffs[0]
        );
    }

    // Mutating standalone bookmark folder metadata keys without changing `bookmark_folders`
    // must still emit row diffs and round-trip cleanly.
    let mut folder_base = WorldStateSnapshot::default();
    folder_base.bookmark_folders.insert(
        40,
        BookmarkFolderRecord {
            folder_id: 40,
            owner_id: cid,
            folder_name: "Folder".into(),
            ..Default::default()
        },
    );
    let mut with_meta = folder_base.clone();
    with_meta.bookmark_folder_known_by.insert((40, peer), true);
    with_meta.bookmark_folder_acls.insert(40, vec![50]);
    with_meta.bookmark_subfolder_parents.insert(41, 40);
    let mut meta_diffs = Vec::new();
    collect_social_domain_row_diffs(&folder_base, &with_meta, &mut meta_diffs);
    assert_eq!(meta_diffs.len(), 3);
}

#[test]
fn social_tx_single_row_granularity_zero_cold_clones_and_rejects_non_finite() {
    let path = temp_extra_social_path("tx_row_granularity");
    let _ = std::fs::remove_file(&path);
    let repo = WorldRepository::open_sqlite(&path).expect("open_sqlite");
    let cid = 90_000_001_i64;
    let peer = 90_000_002_i64;

    repo.tx("seed_social_domains", |wtx| -> Result<(), TxError> {
        wtx.snap.read_mails.insert((cid, 1));
        wtx.snap.mail_labels.insert(
            cid,
            vec![MailLabelRecord {
                label_id: 1,
                name: "Inbox".into(),
                color: "#000000".into(),
            }],
        );
        wtx.snap.mailing_lists.insert(
            200_000_110,
            MailingListRecord {
                list_id: 200_000_110,
                name: "ML".into(),
                owner_id: cid,
                ..Default::default()
            },
        );
        wtx.snap.bookmark_folders.insert(
            200_000_120,
            BookmarkFolderRecord {
                folder_id: 200_000_120,
                owner_id: cid,
                folder_name: "Safe".into(),
                ..Default::default()
            },
        );
        wtx.snap.calendar_events.insert(
            200_000_130,
            CalendarEventRecord {
                event_id: 200_000_130,
                owner_id: cid,
                creator_id: cid,
                title: "Op".into(),
                ..Default::default()
            },
        );
        wtx.snap.chat_channels.insert(
            "ch1".into(),
            ChatChannelRecord {
                channel_id: "ch1".into(),
                name: "Ch".into(),
                ..Default::default()
            },
        );
        wtx.snap.chat_channel_acl.insert(
            "ch1".into(),
            ChatChannelAclRecord {
                allow_list: HashSet::from([cid]),
                ..Default::default()
            },
        );
        wtx.snap.char_contacts.insert(
            cid,
            vec![SocialContactRecord {
                contact_id: peer,
                contact_type: "character".into(),
                standing: 5.0,
                is_watched: false,
                is_blocked: false,
                label_ids: vec![],
            }],
        );
        wtx.snap.char_contact_notifications.insert(
            cid,
            vec![SocialContactNotificationRecord {
                notification_id: 1,
                sender_character_id: peer,
                send_date_unix: 100,
                standing_level: 5.0,
                message: "Hi".into(),
            }],
        );
        wtx.snap.char_notes.insert((cid, peer), "Scout".into());
        Ok(())
    })
    .expect("seed social domains");

    // Mutating single social entries triggers 0 cold fingerprint clones.
    reset_thread_cold_fp_clones_for_test();
    repo.tx("update_one_note", |wtx| -> Result<(), TxError> {
        wtx.snap.char_notes.insert((cid, peer), "UpdatedScout".into());
        Ok(())
    })
    .expect("note update");
    assert_eq!(thread_cold_fp_clones_for_test(), 0);

    reset_thread_cold_fp_clones_for_test();
    repo.tx("insert_read_mail", |wtx| -> Result<(), TxError> {
        wtx.snap.read_mails.insert((cid, 2));
        Ok(())
    })
    .expect("read mail insert");
    assert_eq!(thread_cold_fp_clones_for_test(), 0);

    // Direct `ChangeSet::diff` + `apply_domain_row_changes` executes strictly 1 SQLite write.
    let before_snap = repo.with_snapshot(Clone::clone);
    let mut after_snap = before_snap.clone();
    after_snap
        .char_notes
        .insert((cid, peer), "SingleWriteNote".into());
    let changes = ChangeSet::diff(2, &before_snap, &after_snap);
    assert_eq!(changes.domain_rows.len(), 1);
    assert!(!changes.cold_domains.d6_social);
    assert!(!changes.cold_domains.any());

    let mut mem_conn = Connection::open_in_memory().unwrap();
    apply_migrations(&mem_conn).unwrap();
    replace_sqlite_from_snapshot(&mut mem_conn, &before_snap).unwrap();
    let before_changes = mem_conn.total_changes();
    {
        let sql_tx = mem_conn.transaction().unwrap();
        let mut exec = TxSqlExecutor::new(&sql_tx, crate::tx_engine::TxFaultPlan::None);
        apply_domain_row_changes(&mut exec, &changes.domain_rows).unwrap();
        sql_tx.commit().unwrap();
    }
    assert_eq!(
        mem_conn.total_changes() - before_changes,
        1,
        "mutating 1 social entry must execute strictly 1 SQLite row write"
    );

    // Deleting 1 entry executes strictly 1 SQLite row delete.
    let mut deleted_snap = after_snap.clone();
    deleted_snap.read_mails.remove(&(cid, 2));
    let del_changes = ChangeSet::diff(3, &after_snap, &deleted_snap);
    assert_eq!(del_changes.domain_rows.len(), 1);
    let before_del = mem_conn.total_changes();
    {
        let sql_tx = mem_conn.transaction().unwrap();
        let mut exec = TxSqlExecutor::new(&sql_tx, crate::tx_engine::TxFaultPlan::None);
        apply_domain_row_changes(&mut exec, &del_changes.domain_rows).unwrap();
        sql_tx.commit().unwrap();
    }
    assert_eq!(
        mem_conn.total_changes() - before_del,
        1,
        "deleting 1 social entry must execute strictly 1 SQLite row delete"
    );

    // Non-finite floats in contacts and contact notifications fail validation and rollback cleanly.
    for bad_val in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut bad_contacts = before_snap.clone();
        bad_contacts.char_contacts.get_mut(&cid).unwrap()[0].standing = bad_val;
        let bad_diff = ChangeSet::diff(4, &before_snap, &bad_contacts);
        let err = validate_domain_row_changes(&bad_diff.domain_rows, 1024 * 1024)
            .expect_err("NaN/Inf in SocialContactRecord.standing must fail validation");
        assert!(matches!(err, TxError::NonFinite { .. }), "got: {err}");
        let msg = err.to_string();
        assert!(msg.contains(DOMAIN_SOCIAL_CONTACTS_NOTES), "{msg}");
        assert!(msg.contains("char_contacts/90000001"), "{msg}");

        let tx_err: Result<(), TxError> = repo.tx("reject_bad_contact_notif_float", |wtx| {
            wtx.snap
                .char_contact_notifications
                .get_mut(&cid)
                .unwrap()[0]
                .standing_level = bad_val;
            Ok(())
        });
        let tx_msg = tx_err
            .expect_err("repo.tx must reject NaN/Inf in SocialContactNotificationRecord")
            .to_string();
        assert!(tx_msg.contains(DOMAIN_SOCIAL_CONTACTS_NOTES), "{tx_msg}");
        assert!(tx_msg.contains("char_contact_notifications/90000001"), "{tx_msg}");
    }
    assert_eq!(
        repo.with_snapshot(|s| s.char_contact_notifications[&cid][0].standing_level),
        5.0
    );

    drop(repo);

    // Reopening from disk preserves cross-table social entity high-water marks (#1087).
    let reopened_conn = Connection::open(&path).unwrap();
    let (loaded, _) = load_snapshot_from_sqlite(&reopened_conn).expect("load_snapshot_from_sqlite");
    assert!(loaded.next_entity_id >= 200_000_130);
    assert!(
        super::load::query_cross_table_entity_high_water(&reopened_conn).unwrap() >= 200_000_130
    );
    assert!(super::load::sqlite_entity_id_exists(&reopened_conn, 200_000_130).unwrap());
    assert!(!super::load::sqlite_entity_id_exists(&reopened_conn, 200_000_131).unwrap());
    drop(reopened_conn);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn social_store_all_collections_sync_load_and_single_domain_isolation() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    let cid = 90_000_001_i64;
    let peer = 90_000_002_i64;
    snap.read_mails.insert((cid, 1));
    snap.deleted_mails.insert((cid, 2));
    snap.trashed_mails.insert((cid, 3));
    snap.mail_labels.insert(
        cid,
        vec![MailLabelRecord {
            label_id: 1,
            name: "L".into(),
            color: "#112233".into(),
        }],
    );
    snap.mail_assigned_labels.insert((cid, 1), vec![1]);
    snap.mailing_lists.insert(
        10,
        MailingListRecord {
            list_id: 10,
            name: "ML".into(),
            owner_id: cid,
            default_access: 1,
            default_role: 0,
            cost: 0,
            members: HashMap::from([(cid, 1)]),
            entity_access: HashMap::new(),
            welcome_mail: Some(("W".into(), "B".into())),
            mail_ids: HashSet::from([1]),
        },
    );
    snap.bookmark_folders.insert(
        40,
        BookmarkFolderRecord {
            folder_id: 40,
            owner_id: cid,
            folder_name: "Main".into(),
            is_shared: true,
            corporation_id: Some(98_000_001),
            description: "Desc".into(),
            admin_group_id: Some(50),
            manage_group_id: None,
            use_group_id: None,
            view_group_id: None,
        },
    );
    snap.bookmark_subfolders.insert(30, 40);
    snap.bookmark_subfolder_parents.insert(41, 40);
    snap.bookmark_expiries.insert(30, 1_900_000_000);
    snap.bookmark_folder_acls.insert(40, vec![50]);
    snap.access_groups.insert(50, vec![cid]);
    snap.bookmark_folder_known_by.insert((40, peer), true);
    snap.calendar_events.insert(
        20,
        CalendarEventRecord {
            event_id: 20,
            owner_id: cid,
            creator_id: cid,
            event_date_time: 100,
            duration: 30,
            importance: 0,
            title: "Ev".into(),
            description: "D".into(),
            scope: "Personal".into(),
            responses: HashMap::from([(cid, 1)]),
            is_deleted: false,
        },
    );
    snap.chat_channels.insert(
        "ch1".into(),
        ChatChannelRecord {
            channel_id: "ch1".into(),
            name: "Ch".into(),
            motd: "Motd".into(),
            owner_id: Some(cid),
            members: HashSet::from([cid]),
            is_system_local: false,
            solar_system_id: None,
            is_private: true,
            operators: HashSet::from([cid]),
            password: None,
            is_delayed_local: false,
        },
    );
    snap.chat_channel_acl.insert(
        "ch1".into(),
        ChatChannelAclRecord {
            allow_list: HashSet::from([cid]),
            deny_list: HashSet::new(),
            muted_until: HashMap::new(),
        },
    );
    let contact = SocialContactRecord {
        contact_id: peer,
        contact_type: "character".into(),
        standing: 2.5,
        is_watched: true,
        is_blocked: false,
        label_ids: vec![1],
    };
    snap.char_contacts.insert(cid, vec![contact.clone()]);
    snap.char_contact_notifications.insert(
        cid,
        vec![SocialContactNotificationRecord {
            notification_id: 1,
            sender_character_id: peer,
            send_date_unix: 100,
            standing_level: 2.5,
            message: "Hi".into(),
        }],
    );
    snap.char_contact_labels.insert(cid, vec![(1, "F".into())]);
    snap.corp_contacts.insert(98_000_001, vec![contact.clone()]);
    snap.corp_contact_labels
        .insert(98_000_001, vec![(1, "C".into(), 1)]);
    snap.alliance_contacts.insert(99_000_001, vec![contact]);
    snap.alliance_contact_labels
        .insert(99_000_001, vec![(1, "A".into())]);
    snap.char_notes.insert((cid, peer), "Trusted".into());

    {
        let tx = conn.transaction().unwrap();
        assert!(sync_social_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_social_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    assert!(!load_social_domain_rows(&conn, &mut loaded, &mut LoadContext::new(false)).unwrap());
    assert_eq!(loaded.read_mails, snap.read_mails);
    assert_eq!(loaded.bookmark_folder_known_by, snap.bookmark_folder_known_by);
    assert_eq!(loaded.chat_channel_acl, snap.chat_channel_acl);
    assert_eq!(loaded.char_notes, snap.char_notes);

    // Mutate only `char_notes` and confirm untouched fields across other domains stay intact.
    loaded.char_notes.insert((cid, peer), "Edited".into());
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_social_domain_rows(&tx, &loaded).unwrap());
        tx.commit().unwrap();
    }
    let mut reloaded = WorldStateSnapshot::default();
    load_social_domain_rows(&conn, &mut reloaded, &mut LoadContext::new(false)).unwrap();
    assert_eq!(reloaded.char_notes.get(&(cid, peer)), Some(&"Edited".into()));
    assert_eq!(reloaded.mailing_lists, snap.mailing_lists);
    assert_eq!(reloaded.bookmark_folders, snap.bookmark_folders);
    assert_eq!(reloaded.calendar_events, snap.calendar_events);
    assert_eq!(reloaded.corp_contact_labels, snap.corp_contact_labels);
}
