//! Unit and mutation-killing tests for `sqlite_store::corp_crimewatch_store`.

use super::*;
use crate::sqlite_store::schema::apply_migrations;
use serde::ser::{Error as _, Serializer as _};

#[derive(Serialize)]
struct UnitStructSample;

#[derive(Serialize)]
struct NewtypeStructSample(u32);

#[derive(Serialize)]
struct TupleStructSample(i16, &'static str);

#[derive(Serialize)]
enum SampleEnum {
    UnitA,
    UnitB,
    Newtype(i32),
    Tuple(u8, bool),
    Struct { x: i64, y: String },
}

struct BytesSample<'a>(&'a [u8]);

impl Serialize for BytesSample<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

struct UnsizedSeqSample<'a>(&'a [i32]);

impl Serialize for UnsizedSeqSample<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        for v in self.0 {
            seq.serialize_element(v)?;
        }
        seq.end()
    }
}

#[test]
fn fnv_serializer_covers_all_serde_data_model_types_and_detects_mutations() {
    let err = FnvSerError::custom("boom");
    assert_eq!(err.to_string(), "fnv serialization error");
    assert!(std::error::Error::source(&err).is_none());

    // Bool & integer widths
    assert_ne!(canonical_fp(&true), canonical_fp(&false));
    assert_ne!(canonical_fp(&1i8), canonical_fp(&2i8));
    assert_ne!(canonical_fp(&1i16), canonical_fp(&2i16));
    assert_ne!(canonical_fp(&1i32), canonical_fp(&2i32));
    assert_ne!(canonical_fp(&1i64), canonical_fp(&2i64));
    assert_ne!(canonical_fp(&1u8), canonical_fp(&2u8));
    assert_ne!(canonical_fp(&1u16), canonical_fp(&2u16));
    assert_ne!(canonical_fp(&1u32), canonical_fp(&2u32));
    assert_ne!(canonical_fp(&1u64), canonical_fp(&2u64));
    // Type tag separation: i64(1) != u64(1) != bool(true)
    assert_ne!(canonical_fp(&1i64), canonical_fp(&1u64));
    assert_ne!(canonical_fp(&1u64), canonical_fp(&true));

    // Floats: -0.0 and +0.0 canonicalize to the same hash, distinct non-zero floats differ
    assert_eq!(canonical_fp(&0.0f64), canonical_fp(&-0.0f64));
    assert_eq!(canonical_fp(&0.0f32), canonical_fp(&-0.0f32));
    assert_ne!(canonical_fp(&0.0f64), canonical_fp(&1.5f64));
    assert_ne!(canonical_fp(&1.25f32), canonical_fp(&2.5f32));

    // Char, str, bytes, option, unit
    assert_ne!(canonical_fp(&'a'), canonical_fp(&'b'));
    assert_ne!(canonical_fp("alpha"), canonical_fp("beta"));
    assert_ne!(
        canonical_fp(&BytesSample(b"abc")),
        canonical_fp(&BytesSample(b"abd"))
    );
    assert_ne!(
        canonical_fp(&BytesSample(b"ab")),
        canonical_fp(&BytesSample(b"abc"))
    );
    assert_ne!(canonical_fp(&None::<i32>), canonical_fp(&Some(0i32)));
    assert_ne!(canonical_fp(&Some(1i32)), canonical_fp(&Some(2i32)));
    assert_ne!(canonical_fp(&()), canonical_fp(&None::<i32>));

    // Structs, newtypes, tuples, enums
    assert_eq!(canonical_fp(&UnitStructSample), canonical_fp(&UnitStructSample));
    assert_eq!(canonical_fp(&NewtypeStructSample(42)), canonical_fp(&42u32));
    assert_ne!(
        canonical_fp(&TupleStructSample(1, "a")),
        canonical_fp(&TupleStructSample(2, "a"))
    );
    assert_ne!(
        canonical_fp(&TupleStructSample(1, "a")),
        canonical_fp(&TupleStructSample(1, "b"))
    );
    assert_ne!(canonical_fp(&(10u32, 20u32)), canonical_fp(&(10u32, 21u32)));
    assert_ne!(canonical_fp(&vec![1i32, 2]), canonical_fp(&vec![1i32, 3]));
    assert_ne!(canonical_fp(&vec![1i32, 2]), canonical_fp(&vec![1i32, 2, 3]));
    assert_ne!(
        canonical_fp(&UnsizedSeqSample(&[1, 2])),
        canonical_fp(&UnsizedSeqSample(&[1, 3]))
    );

    // Enum variants
    assert_ne!(
        canonical_fp(&SampleEnum::UnitA),
        canonical_fp(&SampleEnum::UnitB)
    );
    assert_ne!(
        canonical_fp(&SampleEnum::Newtype(1)),
        canonical_fp(&SampleEnum::Newtype(2))
    );
    assert_ne!(
        canonical_fp(&SampleEnum::Tuple(1, true)),
        canonical_fp(&SampleEnum::Tuple(1, false))
    );
    assert_ne!(
        canonical_fp(&SampleEnum::Struct {
            x: 1,
            y: "a".into()
        }),
        canonical_fp(&SampleEnum::Struct {
            x: 2,
            y: "a".into()
        })
    );
    assert_ne!(
        canonical_fp(&SampleEnum::Struct {
            x: 1,
            y: "a".into()
        }),
        canonical_fp(&SampleEnum::Struct {
            x: 1,
            y: "b".into()
        })
    );

    // Order-independent map hashing
    let mut m1 = HashMap::new();
    m1.insert("k1", 100i64);
    m1.insert("k2", 200i64);
    let mut m2 = HashMap::new();
    m2.insert("k2", 200i64);
    m2.insert("k1", 100i64);
    assert_eq!(canonical_fp(&m1), canonical_fp(&m2));
    m2.insert("k2", 201i64);
    assert_ne!(canonical_fp(&m1), canonical_fp(&m2));
    m2.insert("k3", 300i64);
    assert_ne!(canonical_fp(&m1), canonical_fp(&m2));

    // Direct FnvSerializer unit_struct name sensitivity check
    let mut ha = FnvHasher::new();
    FnvSerializer(&mut ha)
        .serialize_unit_struct("StructOne")
        .unwrap();
    let mut hb = FnvHasher::new();
    FnvSerializer(&mut hb)
        .serialize_unit_struct("StructTwo")
        .unwrap();
    assert_ne!(ha.finish(), hb.finish());
}

#[test]
fn d6_corp_crimewatch_payload_roundtrip_sync_load_and_rollback() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mig_ver: i64 = conn
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = ?1",
            params![D6_CORP_CRIMEWATCH_SCHEMA_VERSION],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mig_ver, 4);

    let mut snap = WorldStateSnapshot::default();
    let base_fps = compute_d6_corp_crimewatch_fingerprints(&snap);

    // 1. Populate CorpGovernanceDomain fields
    snap.corp_governance.insert(98_000_001, CorpGovState::default());
    snap.employment_history
        .insert(90_000_001, vec![(98_000_001, 1_700_000_000, None)]);
    snap.corp_hangar_divisions
        .insert(98_000_001, HashMap::from([(1u8, "Main Hangar".into())]));
    snap.corp_member_limits.insert(98_000_001, 50);
    snap.corp_member_titles
        .insert((98_000_001, 90_000_001), vec![1, 2]);

    // 2. Populate CrimewatchDomain fields including unordered ACL set
    snap.crimewatch_states
        .insert(90_000_001, PersistedCrimeWatchState::default());
    snap.wh_polarizations.insert((90_000_001, 31_000_001), 9999);
    snap.zarzakh_gate_locks
        .insert(90_000_001, (30_010_000, 12345));
    snap.structure_docking_acl_denied.insert(
        1_000_000_000_001,
        HashSet::from([90_000_005, 90_000_002, 90_000_009]),
    );

    // 3. Populate WarfareDomain fields
    snap.sov_prospecting_levels.insert(30_000_142, 3);
    snap.fw_corp_enlisted_on.insert(98_000_001, 1_700_000_100);
    snap.fw_wars.push((500_001, 500_002));

    let populated_fps = compute_d6_corp_crimewatch_fingerprints(&snap);
    assert_ne!(populated_fps.corp_governance_fp, base_fps.corp_governance_fp);
    assert_ne!(populated_fps.crimewatch_fp, base_fps.crimewatch_fp);
    assert_ne!(populated_fps.warfare_fp, base_fps.warfare_fp);

    // Sync to SQLite
    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        let changed = sync_d6_corp_crimewatch(&tx, &snap, &mut fp).unwrap();
        assert!(changed);
        // Second sync with identical state should be a no-op
        let changed_again = sync_d6_corp_crimewatch(&tx, &snap, &mut fp).unwrap();
        assert!(!changed_again);
        tx.commit().unwrap();
    }
    assert_eq!(fp.d6_corp_crimewatch, populated_fps);

    // Verify CrimewatchDomainPayload sorted the ACL denied vector deterministically in JSON
    let cw_json: String = conn
        .query_row(
            "SELECT payload_json FROM d6_corp_crimewatch_kv WHERE store_key = ?1",
            params![KEY_CRIMEWATCH],
            |r| r.get(0),
        )
        .unwrap();
    assert!(cw_json.contains("[90000002,90000005,90000009]"));

    // Load into a fresh snapshot and verify all 3 domains
    let mut loaded = WorldStateSnapshot::default();
    load_d6_corp_crimewatch(&conn, &mut loaded);
    assert_eq!(loaded.corp_member_limits.get(&98_000_001), Some(&50));
    assert_eq!(
        loaded.corp_member_titles.get(&(98_000_001, 90_000_001)),
        Some(&vec![1, 2])
    );
    assert_eq!(
        loaded.wh_polarizations.get(&(90_000_001, 31_000_001)),
        Some(&9999)
    );
    assert_eq!(
        loaded
            .structure_docking_acl_denied
            .get(&1_000_000_000_001),
        Some(&HashSet::from([90_000_002, 90_000_005, 90_000_009]))
    );
    assert_eq!(
        loaded.sov_prospecting_levels.get(&30_000_142),
        Some(&3)
    );
    assert_eq!(loaded.fw_wars, vec![(500_001, 500_002)]);
    assert_eq!(
        compute_d6_corp_crimewatch_fingerprints(&loaded),
        populated_fps
    );

    // Mutate each domain in memory and verify rollback_d6_corp_crimewatch restores all 3
    loaded.corp_member_limits.insert(98_000_001, 999);
    loaded.wh_polarizations.clear();
    loaded.fw_wars.clear();
    assert_ne!(
        compute_d6_corp_crimewatch_fingerprints(&loaded),
        populated_fps
    );

    rollback_d6_corp_crimewatch(&conn, &mut loaded, &fp);
    assert_eq!(loaded.corp_member_limits.get(&98_000_001), Some(&50));
    assert_eq!(
        loaded.wh_polarizations.get(&(90_000_001, 31_000_001)),
        Some(&9999)
    );
    assert_eq!(loaded.fw_wars, vec![(500_001, 500_002)]);
    assert_eq!(
        compute_d6_corp_crimewatch_fingerprints(&loaded),
        populated_fps
    );
}

#[test]
fn corp_crimewatch_rollback_preserves_state_on_missing_or_corrupt_kv_row() {
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    // 1. Non-default baseline in memory when `d6_corp_crimewatch_kv` has NO row (or corrupt JSON):
    // rollback must NOT apply `Payload::default()` and wipe in-memory corp/crimewatch/warfare state.
    let mut snap = WorldStateSnapshot::default();
    snap.corp_member_limits.insert(98_000_001, 75);
    snap.wh_polarizations.insert((90_000_001, 31_000_001), 4242);
    snap.fw_wars.push((500_001, 500_004));
    snap.insurgency_runtime
        .active_state_notified
        .extend([90_000_001, 90_000_002, 90_000_003]);

    let mut fp = TableFingerprints::default();
    fp.d6_corp_crimewatch = compute_d6_corp_crimewatch_fingerprints(&snap);

    // Dirty all three domains in memory while `d6_corp_crimewatch_kv` is empty.
    snap.corp_member_limits.insert(98_000_002, 999);
    snap.zarzakh_gate_locks.insert(90_000_001, (30_100_000, 777));
    snap.sov_prospecting_levels.insert(30_000_142, 5);
    rollback_d6_corp_crimewatch(&conn, &mut snap, &fp);
    assert_eq!(
        snap.corp_member_limits.get(&98_000_001),
        Some(&75),
        "missing KV row must not wipe non-default corp_governance state"
    );
    assert_eq!(
        snap.wh_polarizations.get(&(90_000_001, 31_000_001)),
        Some(&4242),
        "missing KV row must not wipe non-default crimewatch state"
    );
    assert_eq!(
        snap.fw_wars,
        vec![(500_001, 500_004)],
        "missing KV row must not wipe non-default warfare state"
    );

    // Corrupt JSON in KV table must likewise refuse to wipe non-default in-memory state.
    for key in [KEY_CORP_GOV, KEY_CRIMEWATCH, KEY_WARFARE] {
        conn.execute(
            "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, '{corrupt')",
            params![key],
        )
        .unwrap();
    }
    rollback_d6_corp_crimewatch(&conn, &mut snap, &fp);
    assert_eq!(snap.corp_member_limits.get(&98_000_001), Some(&75));
    assert_eq!(snap.wh_polarizations.get(&(90_000_001, 31_000_001)), Some(&4242));
    assert_eq!(snap.fw_wars, vec![(500_001, 500_004)]);

    // 2. Converse: when committed baseline genuinely WAS empty (`default`) and no KV row exists,
    // rolling back an aborted first-transaction insertion cleanly reverts back to empty.
    conn.execute("DELETE FROM d6_corp_crimewatch_kv", []).unwrap();
    let empty_base = WorldStateSnapshot::default();
    let mut empty_fp = TableFingerprints::default();
    empty_fp.d6_corp_crimewatch = compute_d6_corp_crimewatch_fingerprints(&empty_base);

    let mut dirty_from_empty = empty_base;
    dirty_from_empty.corp_member_limits.insert(98_000_099, 10);
    dirty_from_empty
        .wh_polarizations
        .insert((90_000_099, 31_000_001), 1);
    dirty_from_empty.fw_wars.push((500_002, 500_003));
    rollback_d6_corp_crimewatch(&conn, &mut dirty_from_empty, &empty_fp);
    assert!(dirty_from_empty.corp_member_limits.is_empty());
    assert!(dirty_from_empty.wh_polarizations.is_empty());
    assert!(dirty_from_empty.fw_wars.is_empty());
}
