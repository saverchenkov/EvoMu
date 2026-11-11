//! Comprehensive unit and mutation-killing tests for `sqlite_store::sync_ext::sync_extension_tables`.
//!
//! Exercises each of the 9 per-key extension tables in complete isolation (insert -> `Ok(true)`,
//! unchanged -> `Ok(false)`, update in-place -> `Ok(true)`, delete -> `Ok(true)`, unchanged -> `Ok(false)`),
//! verifies column order and reloaded values from SQLite, and checks SQL error propagation.

use super::fingerprints::TableFingerprints;
use super::load::load_snapshot_from_sqlite;
use super::schema::apply_migrations;
use super::sync_ext::sync_extension_tables;
use crate::types::{CharacterLoginStats, IskCents, SkillHistoryEntry, WorldStateSnapshot};
use rusqlite::Connection;

fn run_sync_ext(
    conn: &mut Connection,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let tx = conn.transaction().unwrap();
    let changed = sync_extension_tables(&tx, snap, fp)?;
    tx.commit().unwrap();
    Ok(changed)
}

#[test]
fn sync_extension_tables_isolated_per_table_lifecycle_kills_all_cold_changed_mutants() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    let mut fp = TableFingerprints::default();

    // Empty baseline must return `Ok(false)`.
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 29. `character_last_jump_unix`
    snap.character_last_jump_unix.insert(101, 1_700_001_000);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_last_jump_unix.insert(101, 1_700_001_555);
    snap.character_last_jump_unix.insert(102, 1_700_002_000);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_last_jump_unix.remove(&101);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 30. `character_cspa_charges`
    snap.character_cspa_charges.insert(201, IskCents(295_000));
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_cspa_charges.insert(201, IskCents(500_000));
    snap.character_cspa_charges.insert(202, IskCents(750_000));
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_cspa_charges.remove(&201);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 31. `mail_timestamps`
    snap.mail_timestamps.insert(301, 1_700_010_000);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.mail_timestamps.insert(301, 1_700_010_999);
    snap.mail_timestamps.insert(302, 1_700_020_000);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.mail_timestamps.remove(&301);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 32. `character_login_stats` (distinct fields to verify SQL column ordering)
    snap.character_login_stats.insert(
        401,
        CharacterLoginStats {
            last_login_unix: 111,
            last_logout_unix: 222,
            logins_count: 3,
        },
    );
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_login_stats.insert(
        401,
        CharacterLoginStats {
            last_login_unix: 333,
            last_logout_unix: 444,
            logins_count: 5,
        },
    );
    snap.character_login_stats.insert(
        402,
        CharacterLoginStats {
            last_login_unix: 1_700_100,
            last_logout_unix: 1_700_200,
            logins_count: 42,
        },
    );
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_login_stats.remove(&401);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 33. `item_custom_names`
    snap.item_custom_names.insert(501, "Alpha Hull".into());
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.item_custom_names.insert(501, "Alpha Hull II".into());
    snap.item_custom_names.insert(502, "Raven Navy".into());
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.item_custom_names.remove(&501);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 34. `item_positions` (distinct x, y, z floats to verify column ordering)
    snap.item_positions.insert(601, [10.5, -20.25, 30.75]);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.item_positions.insert(601, [11.5, -21.25, 31.75]);
    snap.item_positions.insert(602, [123.5, -456.25, 789.125]);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.item_positions.remove(&601);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 35. `structure_vulnerability_windows` (distinct start/end timestamps)
    snap.structure_vulnerability_windows
        .insert(701, (1_000, 4_600));
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.structure_vulnerability_windows
        .insert(701, (2_000, 5_600));
    snap.structure_vulnerability_windows
        .insert(702, (10_000, 17_200));
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.structure_vulnerability_windows.remove(&701);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 36. `skill_history`
    snap.skill_history.insert(
        801,
        vec![SkillHistoryEntry {
            log_date_ft: 133_000_000_000_000_000,
            event_type_id: 33,
            skill_type_id: 3300,
            skill_level: 1,
            absolute_points: 250.0,
        }],
    );
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.skill_history.get_mut(&801).unwrap()[0].skill_level = 3;
    snap.skill_history.insert(
        802,
        vec![SkillHistoryEntry {
            log_date_ft: 133_000_000_999_000_000,
            event_type_id: 34,
            skill_type_id: 3301,
            skill_level: 4,
            absolute_points: 45_255.0,
        }],
    );
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.skill_history.remove(&801);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // 37. `character_doll_info` (distinct doll_dna and portrait_meta columns)
    snap.character_doll_info
        .insert(901, ("dna_v1".into(), "meta_v1".into()));
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_doll_info
        .insert(901, ("dna_v2".into(), "meta_v2".into()));
    snap.character_doll_info
        .insert(902, ("caldari_dna".into(), "caldari_portrait".into()));
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    snap.character_doll_info.remove(&901);
    assert!(run_sync_ext(&mut conn, &snap, &mut fp).unwrap());
    assert!(!run_sync_ext(&mut conn, &snap, &mut fp).unwrap());

    // Verify exact reloaded SQLite state across all 9 extension tables.
    let (reloaded, reloaded_fp) = load_snapshot_from_sqlite(&conn).unwrap();
    assert_eq!(reloaded.character_last_jump_unix.len(), 1);
    assert_eq!(reloaded.character_last_jump_unix[&102], 1_700_002_000);
    assert_eq!(reloaded.character_cspa_charges.len(), 1);
    assert_eq!(reloaded.character_cspa_charges[&202], IskCents(750_000));
    assert_eq!(reloaded.mail_timestamps.len(), 1);
    assert_eq!(reloaded.mail_timestamps[&302], 1_700_020_000);
    assert_eq!(reloaded.character_login_stats.len(), 1);
    assert_eq!(
        reloaded.character_login_stats[&402],
        CharacterLoginStats {
            last_login_unix: 1_700_100,
            last_logout_unix: 1_700_200,
            logins_count: 42,
        }
    );
    assert_eq!(reloaded.item_custom_names.len(), 1);
    assert_eq!(reloaded.item_custom_names[&502], "Raven Navy");
    assert_eq!(reloaded.item_positions.len(), 1);
    assert_eq!(reloaded.item_positions[&602], [123.5, -456.25, 789.125]);
    assert_eq!(reloaded.structure_vulnerability_windows.len(), 1);
    assert_eq!(
        reloaded.structure_vulnerability_windows[&702],
        (10_000, 17_200)
    );
    assert_eq!(reloaded.skill_history.len(), 1);
    assert_eq!(reloaded.skill_history[&802][0].skill_type_id, 3301);
    assert_eq!(reloaded.skill_history[&802][0].skill_level, 4);
    assert_eq!(reloaded.character_doll_info.len(), 1);
    assert_eq!(
        reloaded.character_doll_info[&902],
        ("caldari_dna".into(), "caldari_portrait".into())
    );
    assert_eq!(reloaded_fp.character_last_jump_unix, fp.character_last_jump_unix);
    assert_eq!(reloaded_fp.character_cspa_charges, fp.character_cspa_charges);
    assert_eq!(reloaded_fp.mail_timestamps, fp.mail_timestamps);
    assert_eq!(reloaded_fp.character_login_stats, fp.character_login_stats);
    assert_eq!(reloaded_fp.item_custom_names, fp.item_custom_names);
    assert_eq!(reloaded_fp.item_positions, fp.item_positions);
    assert_eq!(
        reloaded_fp.structure_vulnerability_windows,
        fp.structure_vulnerability_windows
    );
    assert_eq!(reloaded_fp.skill_history, fp.skill_history);
    assert_eq!(reloaded_fp.character_doll_info, fp.character_doll_info);
}

#[test]
fn sync_extension_tables_propagates_sql_errors_across_all_tables() {
    let tables_and_mutations: &[(&str, fn(&mut WorldStateSnapshot, &mut TableFingerprints, bool))] = &[
        ("character_last_jump_unix", |s, f, is_del| {
            if is_del {
                f.character_last_jump_unix.insert(1, 1);
            } else {
                s.character_last_jump_unix.insert(1, 1);
            }
        }),
        ("character_cspa_charges", |s, f, is_del| {
            if is_del {
                f.character_cspa_charges.insert(1, 1);
            } else {
                s.character_cspa_charges.insert(1, IskCents(1));
            }
        }),
        ("mail_timestamps", |s, f, is_del| {
            if is_del {
                f.mail_timestamps.insert(1, 1);
            } else {
                s.mail_timestamps.insert(1, 1);
            }
        }),
        ("character_login_stats", |s, f, is_del| {
            if is_del {
                f.character_login_stats.insert(1, 1);
            } else {
                s.character_login_stats
                    .insert(1, CharacterLoginStats::default());
            }
        }),
        ("item_custom_names", |s, f, is_del| {
            if is_del {
                f.item_custom_names.insert(1, 1);
            } else {
                s.item_custom_names.insert(1, "x".into());
            }
        }),
        ("item_positions", |s, f, is_del| {
            if is_del {
                f.item_positions.insert(1, 1);
            } else {
                s.item_positions.insert(1, [0.0, 0.0, 0.0]);
            }
        }),
        ("structure_vulnerability_windows", |s, f, is_del| {
            if is_del {
                f.structure_vulnerability_windows.insert(1, (1, 2));
            } else {
                s.structure_vulnerability_windows.insert(1, (1, 2));
            }
        }),
        ("skill_history", |s, f, is_del| {
            if is_del {
                f.skill_history.insert(1, 1);
            } else {
                s.skill_history.insert(1, vec![]);
            }
        }),
        ("character_doll_info", |s, f, is_del| {
            if is_del {
                f.character_doll_info.insert(1, 1);
            } else {
                s.character_doll_info.insert(1, ("a".into(), "b".into()));
            }
        }),
    ];

    for (table, mutate) in tables_and_mutations {
        for is_del in [true, false] {
            let mut conn = Connection::open_in_memory().unwrap();
            let mut snap = WorldStateSnapshot::default();
            let mut fp = TableFingerprints::default();
            mutate(&mut snap, &mut fp, is_del);
            let tx = conn.transaction().unwrap();
            assert!(
                sync_extension_tables(&tx, &snap, &mut fp).is_err(),
                "expected SQL error on missing table {table} (is_del={is_del})"
            );
        }
    }
}

#[test]
fn load_and_rollback_corrupt_or_missing_tables_graceful_paths() {
    use crate::sqlite_store::load::load_snapshot_from_sqlite;
    use crate::sqlite_store::rollback::rollback_dirty_from_sqlite;
    use crate::sqlite_store::schema::apply_migrations;

    // Corrupt skill_history fails closed by default and quarantines when enabled (#906)
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO skill_history (character_id, history_json) VALUES (90000001, '{bad')",
        [],
    )
    .unwrap();
    let err = load_snapshot_from_sqlite(&conn).expect_err("corrupt skill_history must fail closed");
    assert!(err.contains("skill_history") && err.contains("90000001"));
    let (loaded, _fp) =
        crate::sqlite_store::load_snapshot_from_sqlite_with_options(&conn, true).unwrap();
    assert!(!loaded.skill_history.contains_key(&90_000_001));

    // Rollback against empty DB where fp expected populated rows does not panic
    let mut snap = WorldStateSnapshot::default();
    let mut expect_fp = TableFingerprints::default();
    expect_fp.skill_history.insert(90_000_001, 12345);
    expect_fp.item_custom_names.insert(100, 123);
    expect_fp.item_positions.insert(100, 123);
    expect_fp.structure_vulnerability_windows.insert(1_000_000_000_001, (1, 2));
    expect_fp.character_doll_info.insert(90_000_001, 123);
    expect_fp.asteroids.insert(700, 123);
    expect_fp.character_last_jump_unix.insert(90_000_001, 1);
    expect_fp.character_cspa_charges.insert(90_000_001, 1);
    expect_fp.mail_timestamps.insert(1, 1);
    expect_fp.character_login_stats.insert(90_000_001, 1);
    rollback_dirty_from_sqlite(&conn, &mut snap, &expect_fp).unwrap();

    for tbl in [
        "asteroids",
        "character_last_jump_unix",
        "character_cspa_charges",
        "mail_timestamps",
        "character_login_stats",
        "item_custom_names",
        "item_positions",
        "structure_vulnerability_windows",
        "skill_history",
        "character_doll_info",
    ] {
        let c2 = Connection::open_in_memory().unwrap();
        apply_migrations(&c2).unwrap();
        c2.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        let (s2, _) = load_snapshot_from_sqlite(&c2).unwrap();
        let mut r2 = WorldStateSnapshot::default();
        rollback_dirty_from_sqlite(&c2, &mut r2, &expect_fp).unwrap();
        let _ = s2;
    }
    drop(conn);
}
