use super::*;
use rusqlite::Connection;
use std::collections::HashSet;

fn init_migrations_table(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_unix INTEGER NOT NULL);",
    )
    .unwrap();
}

fn get_social_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'schema_migrations' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn get_table_columns(conn: &Connection, table: &str) -> Vec<(String, String)> {
    let sql = format!("PRAGMA table_info(\"{table}\")");
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn social_migration_readonly_and_missing_schema_migrations_fail() {
    // Missing schema_migrations causes migration error
    let no_mig = Connection::open_in_memory().unwrap();
    assert!(apply_d6_social_migration(&no_mig).is_err());

    // Wrong schema_migrations columns causes error
    let bad_mig = Connection::open_in_memory().unwrap();
    bad_mig
        .execute_batch("CREATE TABLE schema_migrations (bad INTEGER);")
        .unwrap();
    assert!(apply_d6_social_migration(&bad_mig).is_err());

    // Read-only DB fails DDL execution
    let tmp = std::env::temp_dir().join(format!(
        "evomu_d6_soc_ro_{}_{:?}.sqlite",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&tmp);
    drop(Connection::open(&tmp).unwrap());
    let ro = Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(apply_d6_social_migration(&ro).is_err());
    drop(ro);
    let _ = std::fs::remove_file(&tmp);

    let base = Connection::open_in_memory().unwrap();
    init_migrations_table(&base);
    apply_d6_social_migration(&base).unwrap();
    let tables = get_social_tables(&base);
    assert_eq!(tables.len(), 19, "expected 19 d6 social tables, got {:?}", tables);
}

#[test]
fn social_load_missing_tables_and_corrupt_rows_cover_all_19_tables() {
    let proto = Connection::open_in_memory().unwrap();
    init_migrations_table(&proto);
    apply_d6_social_migration(&proto).unwrap();
    let tables = get_social_tables(&proto);

    for table in &tables {
        // 1. Missing table error branch
        let conn_missing = Connection::open_in_memory().unwrap();
        init_migrations_table(&conn_missing);
        apply_d6_social_migration(&conn_missing).unwrap();
        conn_missing
            .execute(&format!("DROP TABLE \"{table}\""), [])
            .unwrap();
        let mut snap = WorldStateSnapshot::default();
        assert!(
            load_d6_social_into_snapshot(&conn_missing, &mut snap).is_err(),
            "expected error when table {table} is dropped"
        );

        // 2. Integer type mismatch in primary/first INTEGER column -> row.map_err
        let cols = get_table_columns(&proto, table);
        if cols.iter().any(|(_, ty)| ty.eq_ignore_ascii_case("INTEGER")) {
            let conn_bad_type = Connection::open_in_memory().unwrap();
            init_migrations_table(&conn_bad_type);
            apply_d6_social_migration(&conn_bad_type).unwrap();
            conn_bad_type
                .execute(&format!("DROP TABLE \"{table}\""), [])
                .unwrap();
            let col_defs: Vec<String> = cols
                .iter()
                .map(|(name, _)| format!("\"{name}\" TEXT"))
                .collect();
            conn_bad_type
                .execute(
                    &format!("CREATE TABLE \"{table}\" ({})", col_defs.join(", ")),
                    [],
                )
                .unwrap();
            let vals: Vec<&str> = cols.iter().map(|_| "'not_an_int'").collect();
            conn_bad_type
                .execute(
                    &format!("INSERT INTO \"{table}\" VALUES ({})", vals.join(", ")),
                    [],
                )
                .unwrap();
            let mut snap_bad = WorldStateSnapshot::default();
            assert!(
                load_d6_social_into_snapshot(&conn_bad_type, &mut snap_bad).is_err(),
                "expected row type error on {table}"
            );
        }

        // 3. Corrupt JSON in *_json column fails cleanly
        if cols.iter().any(|(name, _)| name.ends_with("_json")) {
            let conn_bad_json = Connection::open_in_memory().unwrap();
            init_migrations_table(&conn_bad_json);
            apply_d6_social_migration(&conn_bad_json).unwrap();
            let placeholders: Vec<String> = cols
                .iter()
                .map(|(name, ty)| {
                    if name.ends_with("_json") {
                        "'{corrupt_json'".to_string()
                    } else if ty.eq_ignore_ascii_case("INTEGER") {
                        "1".to_string()
                    } else {
                        "'key1'".to_string()
                    }
                })
                .collect();
            conn_bad_json
                .execute(
                    &format!("INSERT INTO \"{table}\" VALUES ({})", placeholders.join(", ")),
                    [],
                )
                .unwrap();
            let mut snap_json = WorldStateSnapshot::default();
            assert!(
                load_d6_social_into_snapshot(&conn_bad_json, &mut snap_json).is_err(),
                "expected corrupt JSON error on {table}"
            );
        }
    }
}

#[test]
fn social_fingerprint_sync_load_and_rollback_edges() {
    let mut conn = Connection::open_in_memory().unwrap();
    init_migrations_table(&conn);
    apply_d6_social_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    // Populate bookmark folders, folder ACLs, subfolder parents, and bookmark meta
    snap.bookmark_folders.insert(10, BookmarkFolderRecord::default());
    snap.bookmark_folder_acls.insert(10, vec![1001]);
    snap.bookmark_subfolders.insert(1, 10);
    snap.bookmark_expiries.insert(1, 9_999_999);
    snap.bookmark_subfolder_parents.insert(11, 10);

    // Chat channel + ACL (allow_list, deny_list, muted_until) + standalone empty ACL ignored
    let ch = ChatChannelRecord {
        channel_id: "500".into(),
        name: "EdgeChan".into(),
        motd: "hello".into(),
        owner_id: Some(1001),
        members: HashSet::new(),
        is_system_local: false,
        solar_system_id: None,
        is_private: true,
        operators: HashSet::new(),
        password: None,
        is_delayed_local: false,
    };
    snap.chat_channels.insert("500".into(), ch);
    let mut ch_acl = ChatChannelAclRecord::default();
    ch_acl.allow_list.insert(2001);
    ch_acl.deny_list.insert(2002);
    ch_acl.muted_until.insert(2003, (9_999_999, "mute".into()));
    snap.chat_channel_acl.insert("500".into(), ch_acl);

    let mut standalone_acl = ChatChannelAclRecord::default();
    standalone_acl.allow_list.insert(3001);
    snap.chat_channel_acl.insert("501".into(), standalone_acl);
    // Empty standalone ACL should be ignored by fingerprint and sync
    snap.chat_channel_acl.insert("502".into(), ChatChannelAclRecord::default());

    let fp_initial = compute_d6_social_fp(&snap);

    // Mutating each social ACL/subfolder structure changes the fingerprint
    let mut snap_f_acl = snap.clone();
    snap_f_acl.bookmark_folder_acls.get_mut(&10).unwrap().push(1002);
    assert_ne!(compute_d6_social_fp(&snap_f_acl), fp_initial);

    let mut snap_sub = snap.clone();
    snap_sub.bookmark_subfolder_parents.insert(10, 99);
    assert_ne!(compute_d6_social_fp(&snap_sub), fp_initial);

    let mut snap_allow = snap.clone();
    snap_allow
        .chat_channel_acl
        .get_mut("501")
        .unwrap()
        .allow_list
        .insert(3002);
    assert_ne!(compute_d6_social_fp(&snap_allow), fp_initial);

    // Empty standalone ACL does not change fp
    let mut snap_no_empty = snap.clone();
    snap_no_empty.chat_channel_acl.remove("502");
    assert_eq!(compute_d6_social_fp(&snap_no_empty), fp_initial);

    // Ensure folder 11 is in bookmark_folders so its subfolder parent is persisted & reloaded
    snap.bookmark_folders.insert(11, BookmarkFolderRecord::default());
    // Sync roundtrip and reload clears stale entries
    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_social(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(fp.d6_social, compute_d6_social_fp(&snap));
    // Clean sync is a no-op
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_d6_social(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    loaded.bookmark_subfolder_parents.insert(999, 888); // stale entry must be cleared
    load_d6_social_into_snapshot(&conn, &mut loaded).unwrap();
    assert!(!loaded.bookmark_subfolder_parents.contains_key(&999));
    assert_eq!(loaded.bookmark_subfolder_parents.get(&11), Some(&10));
    assert!(loaded.chat_channel_acl.get("501").unwrap().allow_list.contains(&3001));
    assert!(!loaded.chat_channel_acl.contains_key("502"));

    // Rollback restores dirty snapshot to persisted state
    let mut dirty = loaded.clone();
    dirty.bookmark_subfolder_parents.insert(777, 10);
    rollback_d6_social(&conn, &mut dirty, &fp);
    assert!(!dirty.bookmark_subfolder_parents.contains_key(&777));

    // Clean rollback is a no-op even if a table is dropped
    let clean_fp = TableFingerprints {
        d6_social: compute_d6_social_fp(&loaded),
        ..TableFingerprints::default()
    };
    let tables = get_social_tables(&conn);
    conn.execute(&format!("DROP TABLE \"{}\"", tables[0]), []).unwrap();
    let before_noop = loaded.clone();
    rollback_d6_social(&conn, &mut loaded, &clean_fp);
    assert_eq!(
        compute_d6_social_fp(&loaded),
        compute_d6_social_fp(&before_noop)
    );

    // Sync SQL error when a social table is missing preserves fp
    let mut dirty_for_err = loaded.clone();
    dirty_for_err.read_mails.insert((999, 999));
    dirty_for_err.access_groups.insert(999, vec![1]);
    dirty_for_err
        .bookmark_folders
        .insert(555, BookmarkFolderRecord::default());
    dirty_for_err.bookmark_subfolder_parents.insert(555, 10);
    let saved_fp = fp.d6_social.clone();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_social(&tx, &dirty_for_err, &mut fp).is_err());
    }
    assert_eq!(fp.d6_social, saved_fp);
}
