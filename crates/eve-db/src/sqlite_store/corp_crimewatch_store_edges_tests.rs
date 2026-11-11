use super::*;
use crate::sqlite_store::fingerprints::FnvHasher;
use rusqlite::Connection;
use serde::Serialize;

fn init_migrations_table(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_unix INTEGER NOT NULL);",
    )
    .unwrap();
}

#[derive(Serialize)]
enum EdgeEnum {
    Unit,
    Tuple(u32, String),
}

#[derive(Serialize)]
struct EdgeStruct {
    a: u8,
    b: EdgeEnum,
    unit_val: (),
}

struct UnsizedSeq;

impl Serialize for UnsizedSeq {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        seq.serialize_element(&1u32)?;
        seq.serialize_element(&2u32)?;
        seq.end()
    }
}

#[test]
fn fnv_serializer_covers_unit_variants_structs_unsized_seq_and_neg_zero() {
    let mut h_neg = FnvHasher::new();
    (-0.0f32).serialize(FnvSerializer(&mut h_neg)).unwrap();
    let mut h_pos = FnvHasher::new();
    (0.0f32).serialize(FnvSerializer(&mut h_pos)).unwrap();
    assert_eq!(h_neg.0, h_pos.0);

    let mut h_struct = FnvHasher::new();
    EdgeStruct {
        a: 7,
        b: EdgeEnum::Tuple(42, "hi".into()),
        unit_val: (),
    }
    .serialize(FnvSerializer(&mut h_struct))
    .unwrap();
    let fp_tuple = h_struct.0;

    let mut h_unit_var = FnvHasher::new();
    EdgeEnum::Unit.serialize(FnvSerializer(&mut h_unit_var)).unwrap();
    assert_ne!(fp_tuple, h_unit_var.0);

    let mut h_unsized = FnvHasher::new();
    UnsizedSeq.serialize(FnvSerializer(&mut h_unsized)).unwrap();
    assert_ne!(h_unsized.0, 0);
}

#[test]
fn corp_crimewatch_migration_and_load_edge_branches() {
    // Missing schema_migrations fails migration
    let conn_no_mig = Connection::open_in_memory().unwrap();
    assert!(apply_d6_corp_crimewatch_migration(&conn_no_mig).is_err());

    // apply_d6_corp_crimewatch_migration fails if DB is read-only or schema_migrations has wrong columns
    let tmp = std::env::temp_dir().join(format!(
        "evomu_d6_cw_ro_{}_{:?}.sqlite",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&tmp);
    drop(Connection::open(&tmp).unwrap());
    let ro = Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(apply_d6_corp_crimewatch_migration(&ro).is_err());
    drop(ro);
    let _ = std::fs::remove_file(&tmp);

    let conn_bad_mig = Connection::open_in_memory().unwrap();
    conn_bad_mig
        .execute_batch("CREATE TABLE schema_migrations (bad INTEGER);")
        .unwrap();
    assert!(apply_d6_corp_crimewatch_migration(&conn_bad_mig).is_err());

    // load_d6_corp_crimewatch tolerates missing table gracefully
    let conn_empty = Connection::open_in_memory().unwrap();
    let mut snap = WorldStateSnapshot::default();
    load_d6_corp_crimewatch(&conn_empty, &mut snap);

    // Unknown store_key is ignored; corrupt JSON in each of the 3 known keys is ignored without panicking
    let mut conn = Connection::open_in_memory().unwrap();
    init_migrations_table(&conn);
    apply_d6_corp_crimewatch_migration(&conn).unwrap();
    conn.execute(
        "INSERT INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES ('unknown_key', '{}')",
        [],
    )
    .unwrap();
    load_d6_corp_crimewatch(&conn, &mut snap);

    for key in [KEY_CORP_GOV, KEY_CRIMEWATCH, KEY_WARFARE] {
        conn.execute(
            "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, '{bad_json')",
            [key],
        )
        .unwrap();
        load_d6_corp_crimewatch(&conn, &mut snap);
        conn.execute("DELETE FROM d6_corp_crimewatch_kv WHERE store_key = ?1", [key])
            .unwrap();
    }
    let _ = &mut conn;
}

#[test]
fn corp_crimewatch_sync_error_and_rollback_fallbacks() {
    let mut conn = Connection::open_in_memory().unwrap();
    init_migrations_table(&conn);
    apply_d6_corp_crimewatch_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    let mut fp = TableFingerprints {
        d6_corp_crimewatch: compute_d6_corp_crimewatch_fingerprints(&snap),
        ..TableFingerprints::default()
    };
    // Clean sync is a no-op returning false
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_d6_corp_crimewatch(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // Mutate all 3 domains so all 3 become dirty and trigger sync
    snap.corp_hangar_divisions
        .entry(98000001)
        .or_default()
        .insert(1, "Hangars".into());
    snap.zarzakh_gate_locks.insert(1001, (1, 1_700_000_000));
    snap.war_state.wars.insert(1, Default::default());
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_corp_crimewatch(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    load_d6_corp_crimewatch(&conn, &mut loaded);
    assert_eq!(
        loaded.corp_hangar_divisions.get(&98000001).and_then(|m| m.get(&1)).map(String::as_str),
        Some("Hangars")
    );
    assert_eq!(loaded.zarzakh_gate_locks.get(&1001), Some(&(1, 1_700_000_000)));
    assert!(loaded.war_state.wars.contains_key(&1));

    // Corrupt JSON on warfare key -> rollback restores clean domains and preserves non-default warfare state
    conn.execute(
        "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, 'corrupt')",
        [KEY_WARFARE],
    )
    .unwrap();
    snap.corp_hangar_divisions.clear();
    snap.war_state.wars.insert(2, Default::default());
    rollback_d6_corp_crimewatch(&conn, &mut snap, &fp);
    assert!(snap.war_state.wars.contains_key(&2));
    assert!(snap.corp_hangar_divisions.contains_key(&98000001));

    // Missing row in DB triggers default rollback for crimewatch domain when baseline fp is empty
    conn.execute(
        "DELETE FROM d6_corp_crimewatch_kv WHERE store_key = ?1",
        [KEY_CRIMEWATCH],
    )
    .unwrap();
    let empty_fp = TableFingerprints {
        d6_corp_crimewatch: compute_d6_corp_crimewatch_fingerprints(&WorldStateSnapshot::default()),
        ..TableFingerprints::default()
    };
    snap.zarzakh_gate_locks.insert(9999, (2, 123));
    rollback_d6_corp_crimewatch(&conn, &mut snap, &empty_fp);
    assert!(snap.zarzakh_gate_locks.is_empty());

    // SQL error during sync on each dirty domain preserves fp unchanged
    let mut conn_err = Connection::open_in_memory().unwrap();
    init_migrations_table(&conn_err);
    apply_d6_corp_crimewatch_migration(&conn_err).unwrap();
    for (trigger_key, mutate_fn) in [
        (
            KEY_CORP_GOV,
            (|s: &mut WorldStateSnapshot| {
                s.corp_hangar_divisions
                    .entry(98000002)
                    .or_default()
                    .insert(1, "Div".into());
            }) as fn(&mut WorldStateSnapshot),
        ),
        (KEY_CRIMEWATCH, |s: &mut WorldStateSnapshot| {
            s.zarzakh_gate_locks.insert(55, (1, 999));
        }),
        (KEY_WARFARE, |s: &mut WorldStateSnapshot| {
            s.war_state.wars.insert(66, Default::default());
        }),
    ] {
        let sql = format!(
            "DROP TRIGGER IF EXISTS fail_sync;
             CREATE TRIGGER fail_sync BEFORE INSERT ON d6_corp_crimewatch_kv
             WHEN NEW.store_key = '{trigger_key}'
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;"
        );
        conn_err.execute_batch(&sql).unwrap();
        let mut s = WorldStateSnapshot::default();
        let mut s_fp = TableFingerprints {
            d6_corp_crimewatch: compute_d6_corp_crimewatch_fingerprints(&s),
            ..TableFingerprints::default()
        };
        let orig_fp = s_fp.d6_corp_crimewatch;
        mutate_fn(&mut s);
        {
            let tx = conn_err.transaction().unwrap();
            assert!(sync_d6_corp_crimewatch(&tx, &s, &mut s_fp).is_err());
        }
        assert_eq!(s_fp.d6_corp_crimewatch, orig_fp);
    }

    // Clean rollback when table dropped is a harmless no-op
    conn.execute("DROP TABLE d6_corp_crimewatch_kv", []).unwrap();
    let clean_snap = WorldStateSnapshot::default();
    let clean_fp = TableFingerprints {
        d6_corp_crimewatch: compute_d6_corp_crimewatch_fingerprints(&clean_snap),
        ..TableFingerprints::default()
    };
    let mut dirty_dropped = clean_snap.clone();
    rollback_d6_corp_crimewatch(&conn, &mut dirty_dropped, &clean_fp);
    assert_eq!(dirty_dropped.next_entity_id, clean_snap.next_entity_id);
}
