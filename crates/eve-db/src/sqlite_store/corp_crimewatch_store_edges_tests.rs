use super::*;
use crate::sqlite_store::fingerprints::{FnvHasher, FnvSerializer};
use rusqlite::Connection;
use serde::Serialize;

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
fn corp_crimewatch_domain_rows_load_and_sync_edge_branches() {
    use crate::sqlite_store::quarantine::LoadContext;
    use crate::sqlite_store::schema::apply_migrations;

    // 1. Missing tables tolerate load cleanly and report had_legacy = false.
    let conn_empty = Connection::open_in_memory().unwrap();
    let mut snap = WorldStateSnapshot::default();
    assert!(!load_corp_crimewatch_domain_rows(
        &conn_empty,
        &mut snap,
        &mut LoadContext::new(false),
    )
    .unwrap());

    // 2. Unknown legacy key is tolerated; corrupt domain_rows / legacy / blob rows fail closed under
    // strict LoadContext::new(false) and quarantine cleanly under LoadContext::quarantine_in_memory().
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    conn.execute_batch(LEGACY_D6_CORP_CRIMEWATCH_SQL).unwrap();
    conn.execute(
        "INSERT INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES ('unknown_key', '{}')",
        [],
    )
    .unwrap();
    assert!(load_corp_crimewatch_domain_rows(
        &conn,
        &mut snap,
        &mut LoadContext::new(false),
    )
    .unwrap());

    for bad_domain in [DOMAIN_CORP_GOVERNANCE, DOMAIN_CRIMEWATCH, DOMAIN_WARFARE] {
        conn.execute(
            "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, 'bad_key/1', '{bad_json')",
            [bad_domain],
        )
        .unwrap();
        assert!(load_corp_crimewatch_domain_rows(
            &conn,
            &mut snap,
            &mut LoadContext::new(false),
        )
        .is_err());
        assert!(load_corp_crimewatch_domain_rows(
            &conn,
            &mut snap,
            &mut LoadContext::quarantine_in_memory(),
        )
        .unwrap());
        conn.execute("DELETE FROM domain_rows WHERE domain = ?1", [bad_domain])
            .unwrap();
    }

    for key in [KEY_CORP_GOV, KEY_CRIMEWATCH, KEY_WARFARE] {
        conn.execute(
            "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, '{bad_json')",
            [key],
        )
        .unwrap();
        assert!(load_corp_crimewatch_domain_rows(
            &conn,
            &mut snap,
            &mut LoadContext::new(false),
        )
        .is_err());
        assert!(load_corp_crimewatch_domain_rows(
            &conn,
            &mut snap,
            &mut LoadContext::quarantine_in_memory(),
        )
        .unwrap());
        conn.execute("DELETE FROM d6_corp_crimewatch_kv WHERE store_key = ?1", [key])
            .unwrap();
    }

    // 3. Duplicate legacy `world_kv_blobs` cleanup drops only Family 4 keys and keeps other blobs intact.
    conn.execute(
        "INSERT OR REPLACE INTO world_kv_blobs (blob_key, blob_json) VALUES ('corp_container_logs', '[]'), ('wh_polarizations', '[]'), ('killmail_details', '[]')",
        [],
    )
    .unwrap();
    {
        let tx = conn.unchecked_transaction().unwrap();
        drop_legacy_corp_crimewatch_tables(&tx).unwrap();
        tx.commit().unwrap();
    }
    let legacy_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'd6_corp_crimewatch_kv'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy_exists, 0);
    let remaining_blobs: i64 = conn
        .query_row("SELECT COUNT(*) FROM world_kv_blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining_blobs, 1);

    // 4. Sync error propagation when domain_rows is missing or blocked by trigger across all 3 domains.
    let mut conn_err = Connection::open_in_memory().unwrap();
    apply_migrations(&conn_err).unwrap();
    for (trigger_domain, mutate_fn) in [
        (
            DOMAIN_CORP_GOVERNANCE,
            (|s: &mut WorldStateSnapshot| {
                s.corp_hangar_divisions
                    .entry(98_000_002)
                    .or_default()
                    .insert(1, "Div".into());
            }) as fn(&mut WorldStateSnapshot),
        ),
        (DOMAIN_CRIMEWATCH, |s: &mut WorldStateSnapshot| {
            s.zarzakh_gate_locks.insert(55, (1, 999));
        }),
        (DOMAIN_WARFARE, |s: &mut WorldStateSnapshot| {
            s.war_state.wars.insert(66, Default::default());
        }),
    ] {
        let sql = format!(
            "DROP TRIGGER IF EXISTS fail_sync;
             CREATE TRIGGER fail_sync BEFORE INSERT ON domain_rows
             WHEN NEW.domain = '{trigger_domain}'
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;"
        );
        conn_err.execute_batch(&sql).unwrap();
        let mut s = WorldStateSnapshot::default();
        mutate_fn(&mut s);
        {
            let tx = conn_err.transaction().unwrap();
            let err = sync_corp_crimewatch_domain_rows(&tx, &s).unwrap_err();
            assert!(err.contains("blocked"), "unexpected err: {err}");
        }
    }
    conn_err.execute("DROP TABLE domain_rows", []).unwrap();
    {
        let tx = conn_err.transaction().unwrap();
        assert!(sync_corp_crimewatch_domain_rows(&tx, &snap).is_err());
    }
}

#[test]
fn corp_crimewatch_rollback_fails_closed_without_partial_overwrite() {
    use crate::sqlite_store::schema::apply_migrations;

    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut base = WorldStateSnapshot::default();
    base.corp_hangar_divisions
        .entry(98_000_001)
        .or_default()
        .insert(1, "BaseHangar".into());
    base.zarzakh_gate_locks.insert(1001, (1, 1_700_000_000));
    base.war_state.wars.insert(1, Default::default());

    {
        let tx = conn.transaction().unwrap();
        assert!(sync_corp_crimewatch_domain_rows(&tx, &base).unwrap());
        tx.commit().unwrap();
    }

    // Mutate all 3 domains in memory and corrupt the middle domain (`DOMAIN_CRIMEWATCH`).
    let mut dirty = base.clone();
    dirty.corp_hangar_divisions.clear();
    dirty.zarzakh_gate_locks.insert(2002, (2, 2_000));
    dirty.war_state.wars.insert(2, Default::default());

    conn.execute(
        "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, 'zarzakh_gate_locks/1001', '{corrupt')",
        [DOMAIN_CRIMEWATCH],
    )
    .unwrap();
    let failures_before = crate::persist_failures_total();
    let err = rollback_corp_crimewatch(&conn, &mut dirty).unwrap_err();
    assert!(err.contains(DOMAIN_CRIMEWATCH), "unexpected err: {err}");
    assert!(crate::persist_failures_total() >= failures_before + 1);
    // Neither earlier `corp_governance` nor later `warfare` domain was mutated.
    assert!(dirty.corp_hangar_divisions.is_empty());
    assert!(dirty.zarzakh_gate_locks.contains_key(&2002));
    assert!(dirty.war_state.wars.contains_key(&2));

    // Dropping `domain_rows` also fails rollback closed without mutating `dirty`.
    conn.execute("DROP TABLE domain_rows", []).unwrap();
    let failures_before_drop = crate::persist_failures_total();
    let drop_err = rollback_corp_crimewatch(&conn, &mut dirty).unwrap_err();
    assert!(drop_err.contains("domain_rows"), "unexpected err: {drop_err}");
    assert!(crate::persist_failures_total() >= failures_before_drop + 1);
    assert!(dirty.corp_hangar_divisions.is_empty());
    assert!(dirty.zarzakh_gate_locks.contains_key(&2002));
    assert!(dirty.war_state.wars.contains_key(&2));
}
